//! Refuse lost initialization provenance at the second layout inspection.

use std::os::unix::fs::MetadataExt as _;

use super::*;
use crate::test_backup::manifest;

#[test]
fn recognized_marker_removed_after_inspection_never_authorizes_guard_only_recovery() -> TestResult {
    let directory = TestDirectory::new()?.retain_on_failure();
    let path = directory.path().join("store");
    let marker = IdentityMarker::initializing(StoreId::from_bytes([93; 16]));
    partial(&path, &marker)?;
    let path = OpenedStore::prepare(&path, None)?;
    let root = DurableDirectory::open(&path)?;
    let manager = GuardManager::open(&path)?;
    let initial = inspect(&path)?;
    assert!(matches!(&initial, Layout::Recognized(found) if found == &marker));
    let guard_path = path.join(".mikrio.lock");
    let guard_inode = guard_path.metadata()?.ino();

    // Model external loss between the two real inspections. Cooperating
    // openers never remove a recognized marker; this is a defensive refusal.
    let saved_marker = directory.path().join("saved-marker");
    fs::rename(path.join("store.json"), &saved_marker)?;
    let before = manifest(&path)?;

    let result =
        OpenedStore::open_prepared(path.clone(), None, &root, &initial, &manager, &REGISTRY);

    let error = result.err().unwrap();
    assert_eq!(error.failure(), OpenFailure::InvalidStoreDirectory);
    assert!(error.can_recheck_initialization());
    assert_eq!(manifest(&path)?, before);
    assert_eq!(guard_path.metadata()?.ino(), guard_inode);
    assert_eq!(fs::read(&saved_marker)?, marker.encode()?);
    assert!(!path.join("store.json").try_exists()?);
    assert!(!path.join("metadata.sqlite3").try_exists()?);

    fs::rename(&saved_marker, path.join("store.json"))?;
    let recovered = OpenedStore::open(&path, None)?;
    assert_eq!(recovered.record().store_id, marker.store_id());
    close(recovered)?;
    assert_eq!(guard_path.metadata()?.ino(), guard_inode);
    drop(manager);
    drop(root);
    directory.finish()?;
    Ok(())
}
