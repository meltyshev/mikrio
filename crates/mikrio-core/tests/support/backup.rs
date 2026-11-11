//! Independent offline copies for migration and recovery qualification.

use std::collections::BTreeMap;
use std::fs::{self, File, OpenOptions};
use std::io::{self, Read as _};
use std::path::{Path, PathBuf};

use sha2::{Digest as _, Sha256};

pub(crate) type Manifest = BTreeMap<PathBuf, Option<(u64, [u8; 32])>>;

pub(crate) fn manifest(root: &Path) -> io::Result<Manifest> {
    fn visit(root: &Path, relative: &Path, result: &mut Manifest) -> io::Result<()> {
        for entry in fs::read_dir(root.join(relative))? {
            let entry = entry?;
            let path = relative.join(entry.file_name());
            let metadata = entry.path().symlink_metadata()?;

            if metadata.is_dir() {
                result.insert(path.clone(), None);
                visit(root, &path, result)?;
                continue;
            }

            if !metadata.is_file() {
                return Err(io::Error::other("unsupported backup entry"));
            }

            // Windows byte-range locks also protect empty guard files. Bound
            // reads by their captured size so EOF does not issue a locked read.
            let mut file = File::open(entry.path())?.take(metadata.len());
            let mut hash = Sha256::new();
            let mut buffer = vec![0; 64 * 1024];

            loop {
                let length = file.read(&mut buffer)?;

                if length == 0 {
                    break;
                }

                hash.update(&buffer[..length]);
            }

            if file.limit() != 0 || file.get_ref().metadata()?.len() != metadata.len() {
                return Err(io::Error::other("backup source length changed"));
            }

            result.insert(path, Some((metadata.len(), hash.finalize().into())));
        }

        Ok(())
    }

    let mut result = BTreeMap::new();
    visit(root, Path::new(""), &mut result)?;
    Ok(result)
}

/// Caller holds source exclusion throughout copy and manifest verification.
pub(crate) fn copy_directory(source: &Path, target: &Path) -> io::Result<()> {
    fs::create_dir(target)?;
    fs::set_permissions(target, source.metadata()?.permissions())?;

    for entry in fs::read_dir(source)? {
        let entry = entry?;
        let destination = target.join(entry.file_name());
        let metadata = entry.path().symlink_metadata()?;

        if metadata.is_dir() {
            copy_directory(&entry.path(), &destination)?;
            continue;
        }

        if !metadata.is_file() {
            return Err(io::Error::other("unsupported backup entry"));
        }

        let mut input = File::open(entry.path())?.take(metadata.len());

        let mut output = OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&destination)?;

        output.set_permissions(metadata.permissions())?;
        let copied = io::copy(&mut input, &mut output)?;

        if copied != metadata.len() || input.get_ref().metadata()?.len() != metadata.len() {
            return Err(io::Error::other("backup source length changed"));
        }

        crate::sync_file(&output)?;

        #[cfg(unix)]
        {
            use std::os::unix::fs::MetadataExt as _;
            let copied = output.metadata()?;

            if copied.nlink() != 1
                || (metadata.dev(), metadata.ino()) == (copied.dev(), copied.ino())
            {
                return Err(io::Error::other("backup shares source file identity"));
            }
        }
    }

    crate::DurableDirectory::open(target)?.sync()?;
    Ok(())
}
