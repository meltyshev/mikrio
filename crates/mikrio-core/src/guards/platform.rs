//! Identity is platform-owned; no path/UUID-based fallback claims equivalence.

use super::{GuardMode, GuardTarget};
use std::fs::File;
use std::io;
use std::path::Path;

pub(super) fn try_lock(file: &File, mode: GuardMode) -> Result<(), std::fs::TryLockError> {
    #[cfg(test)]
    crate::protocol_fault::hit("guard", "lock.before").map_err(std::fs::TryLockError::Error)?;

    match mode {
        GuardMode::Shared => file.try_lock_shared(),
        GuardMode::Exclusive => file.try_lock(),
    }
}

#[cfg(not(windows))]
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub(crate) struct DirectoryIdentity(u64, u64);

#[cfg(not(windows))]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) struct FileIdentity(u64, u64);

#[cfg(windows)]
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub(crate) struct DirectoryIdentity(crate::windows::FileIdentity);

#[cfg(windows)]
pub(super) type FileIdentity = crate::windows::FileIdentity;

impl DirectoryIdentity {
    #[cfg(unix)]
    pub(crate) fn from_directory(file: &File) -> io::Result<Self> {
        use std::os::unix::fs::MetadataExt;

        let metadata = file.metadata()?;

        if !metadata.is_dir() {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "opened root is not a directory",
            ));
        }

        Ok(Self(metadata.dev(), metadata.ino()))
    }

    #[cfg(windows)]
    pub(crate) fn from_directory(file: &File) -> io::Result<Self> {
        crate::windows::identity(file).map(Self)
    }

    #[cfg(not(any(unix, windows)))]
    pub(crate) fn from_directory(_: &File) -> io::Result<Self> {
        Err(io::Error::new(
            io::ErrorKind::Unsupported,
            "native directory identity probe is not implemented for this platform",
        ))
    }
}

#[cfg(unix)]
pub(super) fn open_guard(directory: &Path, target: GuardTarget) -> io::Result<File> {
    use rustix::fs::{Mode, OFlags};

    let directory = rustix::fs::open(
        directory,
        OFlags::RDONLY | OFlags::DIRECTORY | OFlags::CLOEXEC | OFlags::NOFOLLOW,
        Mode::empty(),
    )?;

    let (directory, name) = match target {
        GuardTarget::Store => (directory, ".mikrio.lock".to_owned()),
        GuardTarget::Shard(shard) => (
            rustix::fs::openat(
                &directory,
                "locks",
                OFlags::RDONLY | OFlags::DIRECTORY | OFlags::CLOEXEC | OFlags::NOFOLLOW,
                Mode::empty(),
            )?,
            format!("{shard:02x}.lock"),
        ),
    };

    Ok(rustix::fs::openat(
        directory,
        name,
        OFlags::RDWR | OFlags::CLOEXEC | OFlags::NOFOLLOW | OFlags::NONBLOCK,
        Mode::empty(),
    )?
    .into())
}

#[cfg(windows)]
pub(super) fn open_guard(directory: &Path, target: GuardTarget) -> io::Result<File> {
    let directory = crate::windows::open_directory(directory)?;

    match target {
        GuardTarget::Store => crate::windows::open_writable_file(&directory, ".mikrio.lock"),
        GuardTarget::Shard(shard) => {
            let directory = crate::windows::open_child(&directory, "locks")?;
            crate::windows::open_writable_file(&directory, &format!("{shard:02x}.lock"))
        }
    }
}

#[cfg(not(any(unix, windows)))]
pub(super) fn open_guard(_: &Path, _: GuardTarget) -> io::Result<File> {
    Err(io::Error::new(
        io::ErrorKind::Unsupported,
        "native guard open is not implemented for this platform",
    ))
}

#[cfg(unix)]
fn metadata_identity(metadata: &std::fs::Metadata) -> io::Result<FileIdentity> {
    use std::os::unix::fs::MetadataExt;

    if !metadata.is_file() || metadata.nlink() != 1 {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "guard target is not a single regular file",
        ));
    }

    Ok(FileIdentity(metadata.dev(), metadata.ino()))
}

#[cfg(unix)]
pub(super) fn file_identity(file: &File) -> io::Result<FileIdentity> {
    metadata_identity(&file.metadata()?)
}

#[cfg(unix)]
pub(super) fn path_file_identity(path: &Path) -> io::Result<FileIdentity> {
    metadata_identity(&path.symlink_metadata()?)
}

#[cfg(windows)]
pub(super) fn file_identity(file: &File) -> io::Result<FileIdentity> {
    crate::windows::validate_single_file(file)?;
    crate::windows::identity(file)
}

#[cfg(windows)]
pub(super) fn path_file_identity(path: &Path) -> io::Result<FileIdentity> {
    crate::windows::path_file_identity(path)
}

#[cfg(not(any(unix, windows)))]
pub(super) fn file_identity(_: &File) -> io::Result<FileIdentity> {
    Err(io::Error::new(
        io::ErrorKind::Unsupported,
        "native guard identity is not implemented for this platform",
    ))
}

#[cfg(not(any(unix, windows)))]
pub(super) fn path_file_identity(_: &Path) -> io::Result<FileIdentity> {
    Err(io::Error::new(
        io::ErrorKind::Unsupported,
        "native guard identity is not implemented for this platform",
    ))
}

#[cfg(unix)]
pub(super) fn directory_identity(path: &Path) -> io::Result<DirectoryIdentity> {
    use std::os::unix::fs::MetadataExt;

    let metadata = path.metadata()?;

    if !metadata.is_dir() {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "guard root is not a directory",
        ));
    }

    Ok(DirectoryIdentity(metadata.dev(), metadata.ino()))
}

#[cfg(windows)]
pub(super) fn directory_identity(path: &Path) -> io::Result<DirectoryIdentity> {
    let directory = crate::windows::open_directory(path)?;
    crate::windows::identity(&directory).map(DirectoryIdentity)
}

#[cfg(not(any(unix, windows)))]
pub(super) fn directory_identity(_: &Path) -> io::Result<DirectoryIdentity> {
    Err(io::Error::new(
        io::ErrorKind::Unsupported,
        "native directory identity probe is not implemented for this platform",
    ))
}
