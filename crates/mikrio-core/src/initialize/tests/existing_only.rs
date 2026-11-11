use super::*;

#[test]
fn existing_only_recovers_a_recognized_identity_and_preserves_its_region() -> TestResult {
    let directory = TestDirectory::new()?;
    let path = directory.path().join("store");
    let marker = IdentityMarker::initializing(StoreId::from_bytes([41; 16]));
    partial(&path, &marker)?;
    let store = OpenedStore::open_with_mode(&path, Some("eu-west-1"), OpenMode::ExistingOnly)?;
    assert_eq!(store.record().store_id, marker.store_id());
    let record = store.record().clone();
    close(store)?;
    fs::write(path.join("store.json"), marker.encode()?)?;

    let error = OpenedStore::open_with_mode(&path, Some("us-east-1"), OpenMode::ExistingOnly)
        .err()
        .unwrap();

    assert_eq!(error.failure(), OpenFailure::RegionMismatch);
    assert_eq!(read_marker(&path.join("store.json"))?, marker);

    let store = OpenedStore::open_with_mode(&path, None, OpenMode::ExistingOnly)?;
    assert_eq!(store.record(), &record);
    close(store)?;
    directory.finish()?;
    Ok(())
}

#[test]
fn existing_only_never_adopts_an_empty_or_unproven_directory() -> TestResult {
    let directory = TestDirectory::new()?;
    let path = directory.path().join("store");

    let error = OpenedStore::open_with_mode(&path, None, OpenMode::ExistingOnly)
        .err()
        .unwrap();

    assert_eq!(error.failure(), OpenFailure::InvalidStoreDirectory);
    assert!(!path.try_exists()?);
    fs::create_dir(&path)?;

    let error = OpenedStore::open_with_mode(&path, None, OpenMode::ExistingOnly)
        .err()
        .unwrap();

    assert_eq!(error.failure(), OpenFailure::InvalidStoreDirectory);
    assert_eq!(fs::read_dir(&path)?.count(), 0);
    fs::write(path.join(".mikrio.lock"), b"preserve")?;

    let error = OpenedStore::open_with_mode(&path, None, OpenMode::ExistingOnly)
        .err()
        .unwrap();

    assert_eq!(error.failure(), OpenFailure::InvalidStoreDirectory);
    assert!(error.can_recheck_initialization());
    assert_eq!(fs::read(path.join(".mikrio.lock"))?, b"preserve");
    assert_eq!(fs::read_dir(&path)?.count(), 1);
    directory.finish()?;
    Ok(())
}

#[test]
fn existing_only_contender_waits_for_recognized_initialization_authority() -> TestResult {
    let directory = TestDirectory::new()?;
    let path = directory.path().join("store");
    let marker = IdentityMarker::initializing(StoreId::from_bytes([42; 16]));
    partial(&path, &marker)?;
    let manager = GuardManager::open(&path)?;
    let guard = manager
        .try_acquire(GuardTarget::Store, GuardMode::Exclusive)?
        .unwrap();

    let error = OpenedStore::open_with_mode(&path, None, OpenMode::ExistingOnly)
        .err()
        .unwrap();

    assert_eq!(error.failure(), OpenFailure::StoreBusy);
    assert_eq!(fs::read_dir(&path)?.count(), 2);
    drop(guard);

    let store = OpenedStore::open_with_mode(&path, None, OpenMode::ExistingOnly)?;
    assert_eq!(store.record().store_id, marker.store_id());
    close(store)?;
    directory.finish()?;
    Ok(())
}
