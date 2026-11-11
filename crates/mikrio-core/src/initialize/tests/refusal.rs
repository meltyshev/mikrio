use super::*;
use std::collections::BTreeMap;

#[derive(Debug, PartialEq, Eq)]
enum Entry {
    Directory,
    File(Vec<u8>),
    Link(PathBuf),
}

// Retain exact contents and link targets without following managed symlinks.
// Timestamps and filesystem allocation are outside this assertion's scope.
fn snapshot(root: &Path) -> io::Result<BTreeMap<PathBuf, Entry>> {
    let mut entries = BTreeMap::new();
    let mut directories = vec![root.to_path_buf()];

    while let Some(directory) = directories.pop() {
        for entry in fs::read_dir(directory)? {
            let path = entry?.path();
            let metadata = path.symlink_metadata()?;

            let value = if metadata.is_symlink() {
                Entry::Link(fs::read_link(&path)?)
            } else if metadata.is_dir() {
                directories.push(path.clone());
                Entry::Directory
            } else if metadata.is_file() {
                Entry::File(fs::read(&path)?)
            } else {
                return Err(io::Error::other("unexpected fixture file type"));
            };

            let relative = path
                .strip_prefix(root)
                .map_err(io::Error::other)?
                .to_path_buf();

            assert!(entries.insert(relative, value).is_none());
        }
    }

    Ok(entries)
}

#[cfg(unix)]
#[test]
fn nonportable_path_and_regular_file_refusals_preserve_inputs() -> TestResult {
    use std::os::unix::ffi::OsStringExt;

    let directory = TestDirectory::new()?;

    let invalid = directory
        .path()
        .join(std::ffi::OsString::from_vec(b"invalid-\xff/store".to_vec()));

    let before = snapshot(directory.path())?;
    assert_eq!(failure(&invalid, None), OpenFailure::InvalidArgument);
    assert_eq!(snapshot(directory.path())?, before);

    let path = directory.path().join("regular-file");
    fs::write(&path, b"existing contents")?;
    let before = snapshot(directory.path())?;
    assert_eq!(failure(&path, None), OpenFailure::InvalidStoreDirectory);
    assert_eq!(snapshot(directory.path())?, before);
    directory.finish()?;
    Ok(())
}

#[test]
fn recognized_partial_wrong_types_are_refused_without_recovery_writes() -> TestResult {
    let directory = TestDirectory::new()?;
    let marker = IdentityMarker::initializing(StoreId::from_bytes([51; 16]));

    for (index, name) in ["objects", "staging", "locks", "objects/00", "staging/ff"]
        .iter()
        .enumerate()
    {
        let path = directory.path().join(format!("file-{index}"));
        partial(&path, &marker)?;
        let artifact = path.join(name);
        fs::create_dir_all(artifact.parent().unwrap())?;
        fs::write(&artifact, b"preserve wrong-type artifact")?;
        let before = snapshot(&path)?;
        assert_eq!(failure(&path, None), OpenFailure::StoreCorrupt, "{name}");
        assert_eq!(snapshot(&path)?, before, "{name}");
    }

    for (index, name) in ["locks/00.lock", "metadata.sqlite3", "metadata.sqlite3-wal"]
        .iter()
        .enumerate()
    {
        let path = directory.path().join(format!("directory-{index}"));
        partial(&path, &marker)?;
        fs::create_dir_all(path.join(name))?;
        fs::write(path.join(name).join("sentinel"), b"preserve directory")?;
        let before = snapshot(&path)?;
        assert_eq!(failure(&path, None), OpenFailure::StoreCorrupt, "{name}");
        assert_eq!(snapshot(&path)?, before, "{name}");
    }

    directory.finish()?;
    Ok(())
}

#[test]
fn malformed_partial_shard_names_are_preserved_without_creating_layout() -> TestResult {
    let directory = TestDirectory::new()?;
    let marker = IdentityMarker::initializing(StoreId::from_bytes([52; 16]));

    for (index, name) in [
        "locks/00",
        "locks/a.lock",
        "locks/0g.lock",
        "objects/a",
        "objects/G0",
        "staging/ab.extra",
    ]
    .iter()
    .enumerate()
    {
        let path = directory.path().join(format!("store-{index}"));
        partial(&path, &marker)?;
        let artifact = path.join(name);

        if name.starts_with("locks/") {
            fs::create_dir_all(artifact.parent().unwrap())?;
            fs::write(artifact, b"preserve malformed guard")?;
        } else {
            fs::create_dir_all(artifact)?;
        }

        let before = snapshot(&path)?;
        assert_eq!(failure(&path, None), OpenFailure::StoreCorrupt, "{name}");
        assert_eq!(snapshot(&path)?, before, "{name}");
    }

    directory.finish()?;
    Ok(())
}

#[cfg(unix)]
#[test]
fn partial_managed_symlinks_preserve_both_store_and_target() -> TestResult {
    let directory = TestDirectory::new()?;
    let marker = IdentityMarker::initializing(StoreId::from_bytes([53; 16]));
    let target = directory.path().join("target");
    fs::create_dir(&target)?;
    fs::write(target.join("sentinel"), b"outside managed store")?;

    for (index, name) in ["objects", "staging/00", "locks/00.lock", "metadata.sqlite3"]
        .iter()
        .enumerate()
    {
        let path = directory.path().join(format!("store-{index}"));
        partial(&path, &marker)?;
        let artifact = path.join(name);
        fs::create_dir_all(artifact.parent().unwrap())?;
        std::os::unix::fs::symlink(&target, &artifact)?;
        let before = snapshot(directory.path())?;
        assert_eq!(failure(&path, None), OpenFailure::StoreCorrupt, "{name}");
        assert_eq!(snapshot(directory.path())?, before, "{name}");
    }

    directory.finish()?;
    Ok(())
}

#[test]
fn ready_wrong_types_are_not_repaired_and_original_identity_reopens() -> TestResult {
    let directory = TestDirectory::new()?;
    let path = directory.path().join("store");
    let store = OpenedStore::open(&path, Some("eu-west-1"))?;
    let record = store.record().clone();
    close(store)?;
    let saved = directory.path().join("saved");

    for name in ["objects", "staging/00", "locks/00.lock", "metadata.sqlite3"] {
        let artifact = path.join(name);
        fs::rename(&artifact, &saved)?;

        if saved.is_dir() {
            fs::write(&artifact, b"preserve wrong-type ready artifact")?;
        } else {
            fs::create_dir(&artifact)?;
            fs::write(artifact.join("sentinel"), b"preserve directory")?;
        }

        let before = snapshot(directory.path())?;
        assert_eq!(failure(&path, None), OpenFailure::StoreCorrupt, "{name}");
        assert_eq!(snapshot(directory.path())?, before, "{name}");

        if artifact.is_dir() {
            fs::remove_file(artifact.join("sentinel"))?;
            fs::remove_dir(&artifact)?;
        } else {
            fs::remove_file(&artifact)?;
        }

        fs::rename(&saved, &artifact)?;
        let reopened = OpenedStore::open(&path, None)?;
        assert_eq!(reopened.record(), &record);
        close(reopened)?;
    }

    directory.finish()?;
    Ok(())
}

#[cfg(unix)]
#[test]
fn ready_managed_symlinks_are_refused_without_changing_their_targets() -> TestResult {
    let directory = TestDirectory::new()?;
    let path = directory.path().join("store");
    let store = OpenedStore::open(&path, None)?;
    let record = store.record().clone();
    close(store)?;
    let saved = directory.path().join("saved");

    for name in ["objects", "staging/aa", "metadata.sqlite3"] {
        let artifact = path.join(name);
        fs::rename(&artifact, &saved)?;
        std::os::unix::fs::symlink(&saved, &artifact)?;
        let before = snapshot(directory.path())?;
        assert_eq!(failure(&path, None), OpenFailure::StoreCorrupt, "{name}");
        assert_eq!(snapshot(directory.path())?, before, "{name}");
        fs::remove_file(&artifact)?;
        fs::rename(&saved, &artifact)?;
        let reopened = OpenedStore::open(&path, None)?;
        assert_eq!(reopened.record(), &record);
        close(reopened)?;
    }

    directory.finish()?;
    Ok(())
}
