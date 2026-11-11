//! Deterministic refusal when initialization provenance disappears at a boundary.

use std::cell::RefCell;

use super::*;

thread_local! {
    static RACING_GUARD: RefCell<Option<PathBuf>> = const { RefCell::new(None) };
}

fn create_foreign_guard(point: &str) -> io::Result<()> {
    if point != "initialize.guard.create.before" {
        return Ok(());
    }

    RACING_GUARD.with(|selected| {
        let selected = selected.borrow();

        let path = selected
            .as_ref()
            .ok_or_else(|| io::Error::other("missing owned racing guard path"))?;

        let mut file = fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(path)?;

        file.write_all(b"unproven guard created by another opener")?;
        file.sync_all()
    })
}

#[test]
fn guard_created_after_empty_inspection_is_preserved_without_adoption() -> TestResult {
    let directory = TestDirectory::new()?.retain_on_failure();
    let path = directory.path().join("store");
    let guard = path.join(".mikrio.lock");
    RACING_GUARD.with(|selected| selected.replace(Some(guard.clone())));

    {
        let _fault = fault::install(create_foreign_guard);
        assert_eq!(failure(&path, None), OpenFailure::InvalidStoreDirectory);
    }

    RACING_GUARD.with(RefCell::take);

    assert_eq!(
        fs::read(&guard)?,
        b"unproven guard created by another opener"
    );

    assert_eq!(fs::read_dir(&path)?.count(), 1);
    assert!(!path.join("store.json").try_exists()?);
    assert!(!path.join("metadata.sqlite3").try_exists()?);
    assert_eq!(failure(&path, None), OpenFailure::InvalidStoreDirectory);

    assert_eq!(
        fs::read(&guard)?,
        b"unproven guard created by another opener"
    );

    directory.finish()?;
    Ok(())
}

#[cfg(unix)]
#[test]
fn recheck_refuses_guard_only_layout_without_recreating_moved_state() -> TestResult {
    let directory = TestDirectory::new()?.retain_on_failure();
    let path = directory.path().join("store");
    let store = OpenedStore::open(&path, Some("eu-west-1"))?;
    let identity = store.record().clone();
    let saved = directory.path().join("saved-state");
    fs::create_dir(&saved)?;

    let names = fs::read_dir(&path)?
        .map(|entry| entry.map(|entry| entry.file_name()))
        .collect::<io::Result<Vec<_>>>()?;

    let guard_before = fs::read(path.join(".mikrio.lock"))?;

    for name in &names {
        if name != ".mikrio.lock" {
            fs::rename(path.join(name), saved.join(name))?;
        }
    }

    assert_eq!(
        store.recheck(None).unwrap_err().failure(),
        OpenFailure::StoreCorrupt
    );

    assert_eq!(store.record(), &identity);
    assert_eq!(fs::read_dir(&path)?.count(), 1);
    assert_eq!(fs::read(path.join(".mikrio.lock"))?, guard_before);
    let manager = GuardManager::open(&path)?;

    assert!(
        manager
            .try_acquire(GuardTarget::Store, GuardMode::Exclusive)?
            .is_none()
    );

    for name in &names {
        if name != ".mikrio.lock" {
            assert!(saved.join(name).try_exists()?);
            assert!(!path.join(name).try_exists()?);
            fs::rename(saved.join(name), path.join(name))?;
        }
    }

    store.recheck(Some("eu-west-1"))?;
    close(store)?;
    let reopened = OpenedStore::open(&path, Some("eu-west-1"))?;
    assert_eq!(reopened.record(), &identity);
    close(reopened)?;
    directory.finish()?;
    Ok(())
}

#[cfg(windows)]
#[test]
fn opened_metadata_refuses_path_replacement_and_keeps_the_store_usable() -> TestResult {
    let directory = TestDirectory::new()?;
    let path = directory.path().join("store");
    let store = OpenedStore::open(&path, Some("eu-west-1"))?;
    let identity = store.record().clone();
    let database = path.join("metadata.sqlite3");
    let moved = path.join("moved.sqlite3");

    assert_eq!(
        fs::rename(&database, &moved).unwrap_err().raw_os_error(),
        Some(32)
    );

    assert!(database.is_file());
    assert!(!moved.exists());
    store.recheck(Some("eu-west-1"))?;
    close(store)?;
    let reopened = OpenedStore::open(&path, None)?;
    assert_eq!(reopened.record(), &identity);
    close(reopened)?;
    directory.finish()?;
    Ok(())
}

#[cfg(unix)]
thread_local! {
    static REMOVED_ROOT: RefCell<Option<PathBuf>> = const { RefCell::new(None) };
}

#[cfg(unix)]
fn remove_empty_root(point: &str) -> io::Result<()> {
    if point != "initialize.guard.create.before" {
        return Ok(());
    }

    REMOVED_ROOT.with(|selected| {
        let path = selected
            .take()
            .ok_or_else(|| io::Error::other("missing owned empty root"))?;

        fs::remove_dir(path)
    })
}

#[cfg(unix)]
#[test]
fn removed_empty_root_reports_io_error_without_creating_identity() -> TestResult {
    let directory = TestDirectory::new()?.retain_on_failure();
    let path = directory.path().join("store");
    let sentinel = directory.path().join("sibling-sentinel");
    fs::write(&sentinel, b"unrelated sibling bytes")?;

    let entries = || {
        let mut names = fs::read_dir(directory.path())?
            .map(|entry| entry.map(|entry| entry.file_name()))
            .collect::<io::Result<Vec<_>>>()?;

        names.sort();
        Ok::<_, io::Error>(names)
    };

    let siblings = entries()?;
    REMOVED_ROOT.with(|selected| selected.replace(Some(path.clone())));

    {
        let _fault = fault::install(remove_empty_root);
        assert_eq!(failure(&path, None), OpenFailure::IoError);
    }

    assert!(REMOVED_ROOT.with(RefCell::take).is_none());
    assert!(!path.try_exists()?);
    assert_eq!(fs::read(&sentinel)?, b"unrelated sibling bytes");
    assert_eq!(entries()?, siblings);

    let reopened = OpenedStore::open(&path, Some("eu-west-1"))?;
    assert_eq!(reopened.record().region, "eu-west-1");
    close(reopened)?;
    assert_eq!(fs::read(&sentinel)?, b"unrelated sibling bytes");
    directory.finish()?;
    Ok(())
}
