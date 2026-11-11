//! Fixed-file guard ownership for concurrent readers, writers, and reclamation.
//!
//! Only try-locks run here: waiting/backoff belongs at async admission, never in
//! an occupied payload worker. No upgrade, file creation, or explicit unlock is
//! exposed. An Arc owns the *one* locked descriptor until its last holder ends.

use std::collections::HashMap;
use std::fs::File;
use std::io;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, OnceLock, Weak};

mod platform;
pub(crate) use platform::DirectoryIdentity;

type Registry = HashMap<platform::DirectoryIdentity, Weak<Inner>>;
static REGISTRY: OnceLock<Mutex<Registry>> = OnceLock::new();

/// Store use precedes all shard guards. Targets follow the fixed persisted shard layout.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum GuardTarget {
    /// The lifetime guard, also used exclusively for initialization/maintenance.
    Store,
    /// One of the 256 first-byte blob shards.
    Shard(u8),
}

impl GuardTarget {
    fn index(self) -> usize {
        match self {
            Self::Store => 256,
            Self::Shard(shard) => usize::from(shard),
        }
    }

    fn path(self, directory: &Path) -> PathBuf {
        match self {
            Self::Store => directory.join(".mikrio.lock"),
            Self::Shard(shard) => directory.join("locks").join(format!("{shard:02x}.lock")),
        }
    }
}

/// Shared use or exclusive maintenance. A busy result never revokes a holder.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum GuardMode {
    /// Protect active native file work from reclamation.
    Shared,
    /// Exclude all other use of the fixed target.
    Exclusive,
}

/// Process-local accounting keyed by actual directory identity, not store UUID.
#[derive(Clone)]
pub struct GuardManager(Arc<Inner>);

struct Inner {
    directory: PathBuf,
    identity: platform::DirectoryIdentity,
    // Weak slots retain no descriptor while idle; at most 257 are locked here.
    holders: Mutex<[Weak<Lease>; 257]>,
}

struct Lease {
    mode: GuardMode,
    identity: platform::FileIdentity,
    // Closing the last descriptor releases the OS lock. Never File::try_clone
    // or File::unlock: either could invalidate another local holder's proof.
    _file: File,
}

/// Operation-owned guard. Move this into native work; dropping a waiting future
/// must not drop the guard owned by an already running file operation.
pub struct Guard {
    _lease: Arc<Lease>,
    // Prevent a second manager while a guard outlives its original manager handle.
    _owner: Arc<Inner>,
}

/// Sorted, deduplicated shard protection. Partial acquisition is rolled back.
pub struct GuardSet {
    _guards: Vec<Guard>,
}

impl GuardManager {
    /// Resolve aliases and join the local holder table. Does not create files.
    ///
    /// # Errors
    /// Reports missing/invalid directories, unsupported platform identity, and
    /// poisoned accounting. Unimplemented platforms return `Unsupported`.
    pub fn open(directory: &Path) -> io::Result<Self> {
        let directory = directory.canonicalize()?;
        let identity = platform::directory_identity(&directory)?;

        let mut registry = REGISTRY
            .get_or_init(Mutex::default)
            .lock()
            .map_err(poisoned)?;

        registry.retain(|_, owner| owner.strong_count() != 0);

        if let Some(owner) = registry.get(&identity).and_then(Weak::upgrade) {
            return Ok(Self(owner));
        }

        let owner = Arc::new(Inner {
            directory,
            identity,
            holders: Mutex::new(std::array::from_fn(|_| Weak::new())),
        });

        registry.insert(identity, Arc::downgrade(&owner));
        Ok(Self(owner))
    }

    pub(crate) fn identity(&self) -> DirectoryIdentity {
        self.0.identity
    }

    /// Make one nonblocking acquisition attempt on an existing fixed guard file.
    ///
    /// The caller owns store-use protection before acquiring any shard. On
    /// contention return `Ok(None)`; this is distinct from a failed OS call.
    ///
    /// # Errors
    /// Propagates I/O errors (including missing guards); never recreates a target.
    pub fn try_acquire(&self, target: GuardTarget, mode: GuardMode) -> io::Result<Option<Guard>> {
        if platform::directory_identity(&self.0.directory)? != self.0.identity {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "guard root was replaced",
            ));
        }

        let path = target.path(&self.0.directory);

        if matches!(target, GuardTarget::Shard(_))
            && !self.0.directory.join("locks").symlink_metadata()?.is_dir()
        {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "guard directory is not a real directory",
            ));
        }

        let identity = platform::path_file_identity(&path)?;
        let mut holders = self.0.holders.lock().map_err(poisoned)?;
        let slot = &mut holders[target.index()];

        if let Some(lease) = slot.upgrade() {
            if lease.identity != identity {
                return Err(io::Error::new(
                    io::ErrorKind::InvalidData,
                    "held guard target was replaced",
                ));
            }

            if lease.mode != GuardMode::Shared || mode != GuardMode::Shared {
                return Ok(None);
            }

            return Ok(Some(Guard {
                _lease: lease,
                _owner: Arc::clone(&self.0),
            }));
        }

        let file = platform::open_guard(&self.0.directory, target)?;

        if platform::file_identity(&file)? != identity {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "guard target changed during open",
            ));
        }

        match platform::try_lock(&file, mode) {
            Ok(()) => {}
            Err(std::fs::TryLockError::WouldBlock) => return Ok(None),
            Err(std::fs::TryLockError::Error(error)) => return Err(error),
        }

        if platform::path_file_identity(&path)? != identity {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "guard target changed during acquisition",
            ));
        }

        if platform::directory_identity(&self.0.directory)? != self.0.identity {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "guard root changed during acquisition",
            ));
        }

        let lease = Arc::new(Lease {
            mode,
            identity,
            _file: file,
        });

        *slot = Arc::downgrade(&lease);

        Ok(Some(Guard {
            _lease: lease,
            _owner: Arc::clone(&self.0),
        }))
    }

    /// Acquire unique shards in ascending order, with bounded temporary storage.
    ///
    /// On a busy shard or error, every guard acquired by this attempt is dropped.
    /// The caller retains its store-use guard and retries outside blocking workers.
    ///
    /// # Errors
    /// Reports a failed acquisition; no partial set escapes on failure.
    pub fn try_acquire_shards(
        &self,
        shards: &[u8],
        mode: GuardMode,
    ) -> io::Result<Option<GuardSet>> {
        let mut requested = [false; 256];

        for shard in shards {
            requested[usize::from(*shard)] = true;
        }

        let mut guards = Vec::new();

        for shard in 0..=u8::MAX {
            if !requested[usize::from(shard)] {
                continue;
            }

            let Some(guard) = self.try_acquire(GuardTarget::Shard(shard), mode)? else {
                return Ok(None);
            };

            guards.push(guard);
        }

        Ok(Some(GuardSet { _guards: guards }))
    }
}

fn poisoned<T>(_: std::sync::PoisonError<T>) -> io::Error {
    io::Error::other("guard accounting poisoned")
}
