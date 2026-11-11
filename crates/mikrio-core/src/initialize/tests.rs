use super::*;
use crate::test_directory::TestDirectory;

mod existing_only;
mod faults;
mod migration;
#[cfg(target_os = "macos")]
mod permission_refusal;
mod provenance;
#[cfg(target_os = "linux")]
mod raw_paths;
mod recheck;
mod refusal;
#[cfg(unix)]
mod reinspection;
#[cfg(target_os = "macos")]
mod sidecar_refusal;
mod transitions;

type TestResult = Result<(), Box<dyn std::error::Error>>;

#[allow(
    clippy::panic,
    reason = "negative test helper must reject an unexpected successful open"
)]
fn failure(path: &Path, region: Option<&str>) -> OpenFailure {
    match OpenedStore::open(path, region) {
        Err(error) => error.failure(),
        Ok(_) => panic!("unexpected successful open: {}", path.display()),
    }
}

fn close(store: OpenedStore) -> Result<(), MetadataError> {
    store.close().map_err(|(_, error)| error)
}

fn partial(path: &Path, marker: &IdentityMarker) -> TestResult {
    fs::create_dir(path)?;
    File::create(path.join(".mikrio.lock"))?;
    fs::write(path.join("store.json"), marker.encode()?)?;
    Ok(())
}

#[test]
fn independent_opens_preserve_identity_region_and_each_others_guard() -> TestResult {
    let directory = TestDirectory::new()?;
    let path = directory.path().join("parent/store");
    let first = OpenedStore::open(&path, Some("eu-west-1"))?;
    let record = first.record().clone();
    assert_eq!(first.directory(), path.canonicalize()?);

    assert_eq!(
        read_marker(&path.join("store.json"))?.state(),
        MarkerState::Ready
    );

    assert_eq!(fs::read_dir(path.join("locks"))?.count(), 256);
    let second = OpenedStore::open(&path, None)?;
    assert_eq!(second.record(), &record);

    assert_eq!(
        failure(&path, Some("us-east-1")),
        OpenFailure::RegionMismatch
    );

    let manager = GuardManager::open(&path)?;
    close(first)?;

    assert!(
        manager
            .try_acquire(GuardTarget::Store, GuardMode::Exclusive)?
            .is_none()
    );

    close(second)?;

    let guard = manager
        .try_acquire(GuardTarget::Store, GuardMode::Exclusive)?
        .unwrap();

    assert_eq!(failure(&path, None), OpenFailure::StoreBusy);
    drop(guard);

    let reopened = OpenedStore::open(&path, None)?;
    assert_eq!(reopened.record(), &record);
    close(reopened)?;
    directory.finish()?;
    Ok(())
}

#[test]
fn unrelated_and_unproven_partial_directories_are_preserved() -> TestResult {
    let directory = TestDirectory::new()?;
    let path = directory.path().join("store");
    fs::create_dir(&path)?;
    let sentinel = path.join("unrelated.txt");
    fs::write(&sentinel, b"keep this file")?;
    assert_eq!(failure(&path, None), OpenFailure::InvalidStoreDirectory);
    assert_eq!(fs::read(&sentinel)?, b"keep this file");
    assert_eq!(fs::read_dir(&path)?.count(), 1);
    fs::remove_file(sentinel)?;
    fs::write(path.join(".mikrio.lock"), b"existing guard")?;
    assert_eq!(failure(&path, None), OpenFailure::InvalidStoreDirectory);
    assert_eq!(fs::read(path.join(".mikrio.lock"))?, b"existing guard");
    assert_eq!(fs::read_dir(&path)?.count(), 1);
    directory.finish()?;
    Ok(())
}

#[test]
fn recognized_initialization_recovers_without_changing_committed_configuration() -> TestResult {
    let directory = TestDirectory::new()?;
    let path = directory.path().join("store");
    let marker = IdentityMarker::initializing(StoreId::from_bytes([17; 16]));
    partial(&path, &marker)?;
    // Interrupted creation before the first SQLite transaction.
    File::create(path.join("metadata.sqlite3"))?;
    let store = OpenedStore::open(&path, Some("ap-south-1"))?;
    let record = store.record().clone();
    assert_eq!(record.store_id, marker.store_id());
    close(store)?;

    // The database committed before the ready marker: database configuration wins.
    fs::write(path.join("store.json"), marker.encode()?)?;

    assert_eq!(
        failure(&path, Some("eu-west-1")),
        OpenFailure::RegionMismatch
    );

    assert_eq!(read_marker(&path.join("store.json"))?, marker);
    let reopened = OpenedStore::open(&path, None)?;
    assert_eq!(reopened.record(), &record);
    assert_eq!(read_marker(&path.join("store.json"))?, marker.ready());
    close(reopened)?;
    directory.finish()?;
    Ok(())
}

#[test]
fn ready_store_never_recreates_missing_database_guard_or_directory() -> TestResult {
    let directory = TestDirectory::new()?;
    let path = directory.path().join("store");
    close(OpenedStore::open(&path, None)?)?;

    for name in [
        "metadata.sqlite3",
        ".mikrio.lock",
        "locks/ab.lock",
        "objects/ab",
    ] {
        let original = path.join(name);
        let saved = directory.path().join("saved");
        fs::rename(&original, &saved)?;
        assert_eq!(failure(&path, None), OpenFailure::StoreCorrupt, "{name}");
        assert!(!original.try_exists()?, "{name} was recreated");
        fs::rename(saved, original)?;
    }

    close(OpenedStore::open(&path, None)?)?;
    directory.finish()?;
    Ok(())
}

#[test]
fn unknown_partial_artifacts_are_preserved_before_any_recovery_writes() -> TestResult {
    let directory = TestDirectory::new()?;

    for (index, name) in [
        "objects/00/payload",
        "staging/unrelated",
        "locks/unknown.lock",
    ]
    .iter()
    .enumerate()
    {
        let path = directory.path().join(format!("store-{index}"));

        partial(
            &path,
            &IdentityMarker::initializing(StoreId::from_bytes([18; 16])),
        )?;

        let sentinel = path.join(name);
        fs::create_dir_all(sentinel.parent().unwrap())?;
        fs::write(&sentinel, b"unproven contents")?;
        assert_eq!(failure(&path, None), OpenFailure::StoreCorrupt, "{name}");
        assert_eq!(fs::read(sentinel)?, b"unproven contents");
        assert!(!path.join("metadata.sqlite3").try_exists()?);
        assert_eq!(fs::read_dir(&path)?.count(), 3);
    }

    directory.finish()?;
    Ok(())
}

#[test]
fn marker_temporary_files_require_valid_content_and_matching_identity() -> TestResult {
    let directory = TestDirectory::new()?;
    let path = directory.path().join("store");
    let marker = IdentityMarker::initializing(StoreId::from_bytes([19; 16]));
    partial(&path, &marker)?;
    let temporary = path.join(".store-00112233445566778899aabbccddeeff.tmp");

    for bytes in [
        b"partial marker".to_vec(),
        IdentityMarker::initializing(StoreId::from_bytes([20; 16])).encode()?,
    ] {
        fs::write(&temporary, &bytes)?;
        assert_eq!(failure(&path, None), OpenFailure::StoreCorrupt);
        assert_eq!(fs::read(&temporary)?, bytes);
        assert!(!path.join("objects").try_exists()?);
    }

    fs::write(&temporary, marker.ready().encode()?)?;
    let store = OpenedStore::open(&path, None)?;
    assert_eq!(store.record().store_id, marker.store_id());
    close(store)?;
    directory.finish()?;
    Ok(())
}

#[test]
fn invalid_inputs_and_future_markers_fail_before_creating_artifacts() -> TestResult {
    let directory = TestDirectory::new()?;
    let path = directory.path().join("store");

    for region in ["", "Uppercase", "-invalid", "has space"] {
        assert_eq!(failure(&path, Some(region)), OpenFailure::InvalidArgument);
        assert!(!path.try_exists()?);
    }

    assert_eq!(failure(Path::new(""), None), OpenFailure::InvalidArgument);

    fs::create_dir(&path)?;
    let future = br#"{"format":"mikrio","marker_version":2,"unknown":true}"#;
    fs::write(path.join("store.json"), future)?;
    assert_eq!(failure(&path, None), OpenFailure::UnsupportedStoreVersion);
    assert_eq!(fs::read_dir(&path)?.count(), 1);
    assert_eq!(fs::read(path.join("store.json"))?, future);
    directory.finish()?;
    Ok(())
}

#[test]
fn mismatched_identity_and_newer_database_version_are_never_overwritten() -> TestResult {
    let directory = TestDirectory::new()?;
    let path = directory.path().join("store");
    let store = OpenedStore::open(&path, None)?;
    let record = store.record().clone();
    close(store)?;
    let wrong = IdentityMarker::initializing(StoreId::from_bytes([21; 16])).ready();
    fs::write(path.join("store.json"), wrong.encode()?)?;
    assert_eq!(failure(&path, None), OpenFailure::StoreCorrupt);
    assert_eq!(read_marker(&path.join("store.json"))?, wrong);

    fs::write(
        path.join("store.json"),
        IdentityMarker::initializing(record.store_id)
            .ready()
            .encode()?,
    )?;

    let database = rusqlite::Connection::open(path.join("metadata.sqlite3"))?;
    database.execute("UPDATE store_metadata SET format_version = 99", [])?;
    database.close().map_err(|(_, error)| error)?;
    assert_eq!(failure(&path, None), OpenFailure::UnsupportedStoreVersion);
    directory.finish()?;
    Ok(())
}

#[test]
fn invalid_partial_database_is_not_reset_and_missing_ready_singleton_is_corrupt() -> TestResult {
    let directory = TestDirectory::new()?;
    let path = directory.path().join("partial");

    partial(
        &path,
        &IdentityMarker::initializing(StoreId::from_bytes([22; 16])),
    )?;

    fs::write(
        path.join("metadata.sqlite3"),
        b"unrelated database contents",
    )?;

    assert_eq!(failure(&path, None), OpenFailure::StoreCorrupt);

    assert_eq!(
        fs::read(path.join("metadata.sqlite3"))?,
        b"unrelated database contents"
    );

    assert!(!path.join("objects").try_exists()?);

    let path = directory.path().join("ready");
    close(OpenedStore::open(&path, None)?)?;
    let database = rusqlite::Connection::open(path.join("metadata.sqlite3"))?;
    database.execute("DELETE FROM store_metadata", [])?;
    database.close().map_err(|(_, error)| error)?;
    assert_eq!(failure(&path, None), OpenFailure::StoreCorrupt);
    let database = rusqlite::Connection::open(path.join("metadata.sqlite3"))?;

    assert_eq!(
        database.query_row("SELECT count(*) FROM store_metadata", [], |row| {
            row.get::<_, i64>(0)
        })?,
        0
    );

    database.close().map_err(|(_, error)| error)?;
    directory.finish()?;
    Ok(())
}

#[test]
fn active_initializer_temporary_content_is_not_read_without_its_guard() -> TestResult {
    let directory = TestDirectory::new()?;
    let path = directory.path().join("store");

    partial(
        &path,
        &IdentityMarker::initializing(StoreId::from_bytes([23; 16])),
    )?;

    let manager = GuardManager::open(&path)?;

    let guard = manager
        .try_acquire(GuardTarget::Store, GuardMode::Exclusive)?
        .unwrap();

    let temporary = path.join(".store-00112233445566778899aabbccddeeff.tmp");
    fs::write(&temporary, b"a marker still being written")?;
    assert_eq!(failure(&path, None), OpenFailure::StoreBusy);
    drop(guard);
    assert_eq!(failure(&path, None), OpenFailure::StoreCorrupt);
    assert_eq!(fs::read(temporary)?, b"a marker still being written");
    directory.finish()?;
    Ok(())
}

#[test]
fn bootstrap_journal_waits_for_initializer_and_is_never_manually_removed() -> TestResult {
    let directory = TestDirectory::new()?;
    let path = directory.path().join("store");

    partial(
        &path,
        &IdentityMarker::initializing(StoreId::from_bytes([24; 16])),
    )?;

    let manager = GuardManager::open(&path)?;

    let guard = manager
        .try_acquire(GuardTarget::Store, GuardMode::Exclusive)?
        .unwrap();

    let journal = path.join("metadata.sqlite3-journal");
    fs::write(&journal, b"SQLite bootstrap journal")?;
    assert_eq!(failure(&path, None), OpenFailure::StoreBusy);
    drop(guard);
    // An orphan sidecar without its database is not permission to create one.
    assert_eq!(failure(&path, None), OpenFailure::StoreCorrupt);
    assert_eq!(fs::read(journal)?, b"SQLite bootstrap journal");
    assert!(!path.join("metadata.sqlite3").try_exists()?);
    directory.finish()?;
    Ok(())
}

#[cfg(any(unix, windows))]
#[test]
fn symlink_aliases_share_identity_but_managed_symlinks_and_hardlinks_fail() -> TestResult {
    let directory = TestDirectory::new()?;
    let path = directory.path().join("store");
    let first = OpenedStore::open(&path, None)?;
    let alias = directory.path().join("alias");
    #[cfg(unix)]
    std::os::unix::fs::symlink(&path, &alias)?;
    #[cfg(windows)]
    std::os::windows::fs::symlink_dir(&path, &alias)?;
    let second = OpenedStore::open(&alias, None)?;
    assert_eq!(first.record(), second.record());
    assert_eq!(first.directory(), second.directory());
    close(first)?;
    close(second)?;
    let guard = path.join("locks/00.lock");
    let saved = directory.path().join("saved");
    fs::rename(&guard, &saved)?;
    #[cfg(unix)]
    std::os::unix::fs::symlink(&saved, &guard)?;
    #[cfg(windows)]
    std::os::windows::fs::symlink_file(&saved, &guard)?;
    assert_eq!(failure(&path, None), OpenFailure::StoreCorrupt);
    fs::remove_file(&guard)?;
    fs::hard_link(&saved, &guard)?;
    assert_eq!(failure(&path, None), OpenFailure::StoreCorrupt);
    assert_eq!(fs::read(&saved)?, b"");
    directory.finish()?;
    Ok(())
}
