//! Native publication effects. Callers own store/shard exclusion and recovery.

use std::fs::File;
use std::io;
use std::path::{Component, Path};

#[cfg(test)]
mod faults;
#[cfg(test)]
use faults::{Effect, Phase};

/// An open directory used for relative, same-filesystem publication.
///
/// This internal primitive does not infer ownership from a filename. Its caller
/// must hold the store or shard guards required for each filesystem change.
pub struct DurableDirectory {
    file: File,
    #[cfg(test)]
    faults: faults::Faults,
}

impl DurableDirectory {
    /// Open an existing directory without creating or replacing it.
    ///
    /// # Errors
    /// Returns native I/O errors, or Unsupported on an unimplemented platform.
    pub fn open(path: &Path) -> io::Result<Self> {
        platform::open_directory(path).map(|file| Self {
            file,
            #[cfg(test)]
            faults: faults::Faults::default(),
        })
    }

    pub(crate) fn identity(&self) -> io::Result<crate::guards::DirectoryIdentity> {
        crate::guards::DirectoryIdentity::from_directory(&self.file)
    }

    pub(crate) fn open_child(&self, name: &str) -> io::Result<Self> {
        basename(name)?;

        platform::open_child(&self.file, name).map(|file| Self {
            file,
            #[cfg(test)]
            faults: faults::Faults::default(),
        })
    }

    pub(crate) fn open_file(&self, name: &str) -> io::Result<File> {
        basename(name)?;
        platform::open_file(&self.file, name)
    }

    /// Exclusively create an empty owned file, with private permissions.
    ///
    /// # Errors
    /// Rejects a non-basename or any pre-existing target, including a symlink.
    pub fn create_file(&self, name: &str) -> io::Result<File> {
        basename(name)?;
        platform::create_file(&self.file, name)
    }

    /// Synchronize directory entries. A failure must remain visible to recovery.
    ///
    /// # Errors
    /// Returns the OS synchronization error without downgrading durability.
    pub fn sync(&self) -> io::Result<()> {
        self.file.sync_all()
    }

    /// Publish a fully written source without replacing an existing target.
    ///
    /// Synchronizes the payload before rename and both affected directories
    /// afterwards. A post-rename error does not establish that rename failed.
    /// Cross-device errors are returned; there is no copy fallback.
    ///
    /// # Errors
    /// Reports invalid basenames, existing targets and native open/sync/rename
    /// errors. The caller retains its guard and resolves ambiguous effects.
    pub fn publish_new(&self, source: &str, destination: &Self, name: &str) -> io::Result<()> {
        basename(source)?;
        basename(name)?;
        let file = platform::open_publication_file(&self.file, source)?;

        sync_file_observed(
            &file,
            #[cfg(test)]
            Some(&self.faults),
        )?;

        #[cfg(test)]
        self.faults.at(Effect::Rename, Phase::Before)?;

        platform::rename_new(&self.file, source, &destination.file, name)?;

        #[cfg(test)]
        self.faults.at(Effect::Rename, Phase::After)?;

        #[cfg(test)]
        self.faults.at(Effect::DestinationSync, Phase::Before)?;

        destination.sync()?;

        #[cfg(test)]
        self.faults.at(Effect::DestinationSync, Phase::After)?;

        #[cfg(test)]
        self.faults.at(Effect::SourceSync, Phase::Before)?;

        self.sync()?;

        #[cfg(test)]
        self.faults.at(Effect::SourceSync, Phase::After)?;

        Ok(())
    }

    /// Unlink a caller-owned file and synchronize its directory, even if absent.
    ///
    /// Retrying after a lost acknowledgment must still synchronize the directory.
    /// This does not establish ownership or authorize deleting arbitrary paths.
    ///
    /// # Errors
    /// Reports invalid basenames, non-NotFound unlink errors and sync errors.
    pub fn remove_owned(&self, name: &str) -> io::Result<()> {
        basename(name)?;

        #[cfg(test)]
        self.faults.at(Effect::Unlink, Phase::Before)?;

        match platform::unlink(&self.file, name) {
            Ok(()) => {}
            Err(error) if error.kind() == io::ErrorKind::NotFound => {}
            Err(error) => return Err(error),
        }

        #[cfg(test)]
        self.faults.at(Effect::Unlink, Phase::After)?;

        #[cfg(test)]
        self.faults.at(Effect::UnlinkSync, Phase::Before)?;

        self.sync()?;

        #[cfg(test)]
        self.faults.at(Effect::UnlinkSync, Phase::After)?;

        Ok(())
    }
}

/// Synchronize file content and metadata, including device flush on macOS.
///
/// # Errors
/// Returns failed flushes; a successful return is not a power-loss experiment.
pub fn sync_file(file: &File) -> io::Result<()> {
    sync_file_observed(
        file,
        #[cfg(test)]
        None,
    )
}

fn sync_file_observed(
    file: &File,
    #[cfg(test)] observer: Option<&faults::Faults>,
) -> io::Result<()> {
    #[cfg(test)]
    faults::observe(observer, Effect::FileSync, Phase::Before)?;

    file.sync_all()?;

    #[cfg(test)]
    faults::observe(observer, Effect::FileSync, Phase::After)?;

    #[cfg(target_os = "macos")]
    {
        #[cfg(test)]
        faults::observe(observer, Effect::DeviceSync, Phase::Before)?;

        rustix::fs::fcntl_fullfsync(file)?;

        #[cfg(test)]
        faults::observe(observer, Effect::DeviceSync, Phase::After)?;
    }

    Ok(())
}

fn basename(name: &str) -> io::Result<()> {
    let mut components = Path::new(name).components();

    if name.is_empty()
        || name.contains(['/', '\\', '\0'])
        || !matches!(components.next(), Some(Component::Normal(_)))
        || components.next().is_some()
    {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "expected a basename",
        ));
    }

    Ok(())
}

#[cfg(any(target_os = "macos", target_os = "linux"))]
mod platform {
    use super::{File, Path, io};
    use rustix::fs::{AtFlags, Mode, OFlags, RenameFlags};

    pub(super) fn open_directory(path: &Path) -> io::Result<File> {
        Ok(rustix::fs::open(
            path,
            OFlags::RDONLY | OFlags::DIRECTORY | OFlags::CLOEXEC,
            Mode::empty(),
        )?
        .into())
    }

    pub(super) fn create_file(directory: &File, name: &str) -> io::Result<File> {
        Ok(rustix::fs::openat(
            directory,
            name,
            OFlags::RDWR | OFlags::CREATE | OFlags::EXCL | OFlags::CLOEXEC | OFlags::NOFOLLOW,
            Mode::RUSR | Mode::WUSR,
        )?
        .into())
    }

    pub(super) fn open_child(directory: &File, name: &str) -> io::Result<File> {
        Ok(rustix::fs::openat(
            directory,
            name,
            OFlags::RDONLY | OFlags::DIRECTORY | OFlags::CLOEXEC | OFlags::NOFOLLOW,
            Mode::empty(),
        )?
        .into())
    }

    pub(super) fn open_file(directory: &File, name: &str) -> io::Result<File> {
        let file: File = rustix::fs::openat(
            directory,
            name,
            OFlags::RDONLY | OFlags::CLOEXEC | OFlags::NOFOLLOW | OFlags::NONBLOCK,
            Mode::empty(),
        )?
        .into();

        if !file.metadata()?.is_file() {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "publication source is not a regular file",
            ));
        }

        Ok(file)
    }

    pub(super) fn rename_new(
        source: &File,
        from: &str,
        destination: &File,
        to: &str,
    ) -> io::Result<()> {
        Ok(rustix::fs::renameat_with(
            source,
            from,
            destination,
            to,
            RenameFlags::NOREPLACE,
        )?)
    }

    pub(super) fn unlink(directory: &File, name: &str) -> io::Result<()> {
        Ok(rustix::fs::unlinkat(directory, name, AtFlags::empty())?)
    }

    pub(super) use open_file as open_publication_file;
}

#[cfg(windows)]
mod platform {
    pub(super) use crate::windows::{
        create_file, open_child, open_directory, open_file,
        open_writable_file as open_publication_file, rename_new, unlink,
    };
}

#[cfg(not(any(target_os = "macos", target_os = "linux", windows)))]
mod platform {
    use super::{File, Path, io};

    fn unsupported<T>() -> io::Result<T> {
        Err(io::Error::new(
            io::ErrorKind::Unsupported,
            "native publication is not implemented on this target",
        ))
    }

    pub(super) fn open_directory(_: &Path) -> io::Result<File> {
        unsupported()
    }

    pub(super) fn create_file(_: &File, _: &str) -> io::Result<File> {
        unsupported()
    }

    pub(super) fn open_child(_: &File, _: &str) -> io::Result<File> {
        unsupported()
    }

    pub(super) fn open_file(_: &File, _: &str) -> io::Result<File> {
        unsupported()
    }

    pub(super) use open_file as open_publication_file;

    pub(super) fn rename_new(_: &File, _: &str, _: &File, _: &str) -> io::Result<()> {
        unsupported()
    }

    pub(super) fn unlink(_: &File, _: &str) -> io::Result<()> {
        unsupported()
    }
}

#[cfg(test)]
mod tests;
