use super::*;
use std::collections::BTreeMap;

#[derive(Debug, PartialEq, Eq)]
enum Entry {
    Directory,
    File(Vec<u8>),
    Link(PathBuf),
}

fn snapshot(root: &Path) -> io::Result<BTreeMap<PathBuf, Entry>> {
    let mut entries = BTreeMap::new();
    let mut pending = vec![root.to_path_buf()];

    while let Some(directory) = pending.pop() {
        for entry in fs::read_dir(directory)? {
            let path = entry?.path();
            let metadata = path.symlink_metadata()?;

            let value = if metadata.is_symlink() {
                Entry::Link(fs::read_link(&path)?)
            } else if metadata.is_dir() {
                pending.push(path.clone());
                Entry::Directory
            } else if metadata.is_file() {
                Entry::File(fs::read(&path)?)
            } else {
                return Err(io::Error::other("unexpected fixture entry"));
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

#[test]
fn guard_created_after_empty_inspection_is_not_adopted_or_replaced() -> TestResult {
    let directory = TestDirectory::new()?.retain_on_failure();
    let path = directory.path().join("store");
    fs::create_dir(&path)?;
    let initial = inspect(&path)?;
    assert!(matches!(initial, Layout::Empty));
    let root = DurableDirectory::open(&path)?;
    // Place the competing creator between inspection and guard creation. This
    // caller never acquires positive provenance for the other creator's guard.
    fs::write(path.join(".mikrio.lock"), b"other creator's guard")?;
    let before = snapshot(&path)?;
    let manager = GuardManager::open(&path)?;

    let error =
        OpenedStore::open_prepared(path.clone(), None, &root, &initial, &manager, &REGISTRY)
            .err()
            .ok_or("a stale empty inspection must not adopt the competing guard")?;

    assert_eq!(error.failure(), OpenFailure::InvalidStoreDirectory);
    assert!(error.can_recheck_initialization());
    assert_eq!(snapshot(&path)?, before);
    assert!(!path.join("store.json").try_exists()?);

    assert!(
        manager
            .try_acquire(GuardTarget::Store, GuardMode::Exclusive)?
            .is_some()
    );

    directory.finish()?;
    Ok(())
}

#[test]
fn provisional_names_without_published_identity_preserve_their_contents() -> TestResult {
    let directory = TestDirectory::new()?.retain_on_failure();
    let marker = IdentityMarker::initializing(StoreId::from_bytes([71; 16]));

    for (index, guard, managed, retryable) in [
        (0, true, false, true),
        (1, false, false, false),
        (2, true, true, false),
    ] {
        let path = directory.path().join(format!("store-{index}"));
        fs::create_dir(&path)?;

        if guard {
            File::create(path.join(".mikrio.lock"))?;
        }

        fs::write(
            path.join(".store-00112233445566778899aabbccddeeff.tmp"),
            marker.encode()?,
        )?;

        if managed {
            fs::create_dir(path.join("objects"))?;
        }

        let before = snapshot(&path)?;

        let error = OpenedStore::open(&path, None)
            .err()
            .ok_or("unpublished identity must be refused")?;

        assert_eq!(error.failure(), OpenFailure::InvalidStoreDirectory);
        assert_eq!(error.can_recheck_initialization(), retryable);
        assert_eq!(snapshot(&path)?, before);
        assert!(!path.join("store.json").try_exists()?);
        assert!(!path.join("metadata.sqlite3").try_exists()?);
    }

    directory.finish()?;
    Ok(())
}

#[test]
fn ready_reacquisition_refuses_absent_or_regressed_marker_and_releases_guard() -> TestResult {
    let directory = TestDirectory::new()?.retain_on_failure();

    for (name, marker, expected) in [
        ("absent", false, OpenFailure::StoreCorrupt),
        ("regressed", true, OpenFailure::StoreBusy),
    ] {
        let path = directory.path().join(name);
        fs::create_dir(&path)?;
        File::create(path.join(".mikrio.lock"))?;

        if marker {
            fs::write(
                path.join("store.json"),
                IdentityMarker::initializing(StoreId::from_bytes([72; 16])).encode()?,
            )?;
        }

        let before = snapshot(&path)?;
        let manager = GuardManager::open(&path)?;

        let guard = manager
            .try_acquire(GuardTarget::Store, GuardMode::Shared)?
            .ok_or("fixture guard unavailable")?;

        // Exercise the internal post-acquisition recheck directly. This is
        // primitive refusal evidence, not a supported live-file-edit workflow.
        let error = open_ready(path.clone(), None, guard, &REGISTRY)
            .err()
            .ok_or("changed ready state must be refused")?;

        assert_eq!(error.failure(), expected);
        assert_eq!(snapshot(&path)?, before);

        let exclusive = manager
            .try_acquire(GuardTarget::Store, GuardMode::Exclusive)?
            .ok_or("failed recheck retained its shared guard")?;

        drop(exclusive);
    }

    directory.finish()?;
    Ok(())
}

#[test]
fn directory_creation_recheck_preserves_existing_file_and_symlink_targets() -> TestResult {
    let directory = TestDirectory::new()?.retain_on_failure();
    let file_path = directory.path().join("file");
    fs::write(&file_path, b"preserve existing file")?;
    let before = snapshot(directory.path())?;

    assert_eq!(
        ensure_directory(&file_path).unwrap_err().failure(),
        OpenFailure::StoreCorrupt
    );

    assert_eq!(snapshot(directory.path())?, before);

    #[cfg(unix)]
    {
        let target = directory.path().join("target");
        fs::create_dir(&target)?;
        fs::write(target.join("sentinel"), b"preserve link target")?;
        let link = directory.path().join("link");
        std::os::unix::fs::symlink(&target, &link)?;
        let before = snapshot(directory.path())?;

        assert_eq!(
            ensure_directory(&link).unwrap_err().failure(),
            OpenFailure::StoreCorrupt
        );

        assert_eq!(snapshot(directory.path())?, before);
    }

    directory.finish()?;
    Ok(())
}

#[test]
fn layout_creation_rechecks_nonempty_shard_before_guard_or_database_creation() -> TestResult {
    let directory = TestDirectory::new()?.retain_on_failure();
    let path = directory.path().join("store");

    for name in ["objects/00", "staging", "locks"] {
        fs::create_dir_all(path.join(name))?;
    }

    fs::write(path.join("objects/00/sentinel"), b"unproven payload")?;
    let before = snapshot(&path)?;

    assert_eq!(
        prepare_layout(&path).unwrap_err().failure(),
        OpenFailure::StoreCorrupt
    );

    assert_eq!(snapshot(&path)?, before);
    assert_eq!(fs::read_dir(path.join("locks"))?.count(), 0);
    assert!(!path.join("metadata.sqlite3").try_exists()?);
    directory.finish()?;
    Ok(())
}

#[test]
fn repeated_layout_creation_preserves_all_existing_guard_files() -> TestResult {
    let directory = TestDirectory::new()?.retain_on_failure();
    let path = directory.path().join("store");
    fs::create_dir(&path)?;
    prepare_layout(&path)?;
    let before = snapshot(&path)?;
    assert_eq!(before.len(), 771);

    let metadata = (0..=u8::MAX)
        .map(|shard| fs::metadata(path.join("locks").join(format!("{shard:02x}.lock"))))
        .collect::<io::Result<Vec<_>>>()?;

    prepare_layout(&path)?;
    assert_eq!(snapshot(&path)?, before);

    for (shard, prior) in (0..=u8::MAX).zip(metadata) {
        let current = fs::metadata(path.join("locks").join(format!("{shard:02x}.lock")))?;
        assert_eq!(current.len(), prior.len());
        assert_eq!(current.modified()?, prior.modified()?);

        #[cfg(unix)]
        {
            use std::os::unix::fs::MetadataExt;
            assert_eq!((current.dev(), current.ino()), (prior.dev(), prior.ino()));
        }
    }

    directory.finish()?;
    Ok(())
}
