use super::*;

#[test]
fn recheck_refuses_regressed_marker_without_republishing_it() -> TestResult {
    let directory = TestDirectory::new()?;
    let path = directory.path().join("store");
    let store = OpenedStore::open(&path, Some("eu-west-1"))?;
    let original = store.record().clone();
    let marker_path = path.join("store.json");
    let ready = fs::read(&marker_path)?;
    let regressed = IdentityMarker::initializing(original.store_id).encode()?;
    fs::write(&marker_path, &regressed)?;

    assert_eq!(
        store.recheck(None).unwrap_err().failure(),
        OpenFailure::StoreCorrupt
    );

    assert_eq!(fs::read(&marker_path)?, regressed);
    assert_eq!(store.database.read_store()?, original);
    assert_eq!(store.record(), &original);

    fs::write(&marker_path, &ready)?;
    store.recheck(Some("eu-west-1"))?;
    close(store)?;
    let reopened = OpenedStore::open(&path, None)?;
    assert_eq!(reopened.record(), &original);
    assert_eq!(fs::read(&marker_path)?, ready);
    close(reopened)?;
    directory.finish()?;
    Ok(())
}

#[test]
fn recheck_refuses_changed_immutable_record_without_resetting_it() -> TestResult {
    let directory = TestDirectory::new()?;
    let path = directory.path().join("store");
    let store = OpenedStore::open(&path, Some("eu-west-1"))?;
    let original = store.record().clone();
    let marker = fs::read(path.join("store.json"))?;
    let connection = rusqlite::Connection::open(path.join("metadata.sqlite3"))?;

    let timestamp: i64 = connection.query_row(
        "SELECT created_at_ms FROM store_metadata WHERE singleton_id = 1",
        [],
        |row| row.get(0),
    )?;

    // Deliberate corruption of an owned fixture, never a supported live-edit API.
    // Each changed value is independently valid; only cached identity disagrees.
    for statement in [
        "UPDATE store_metadata SET region = 'ap-south-1' WHERE singleton_id = 1",
        "UPDATE store_metadata SET created_at_ms = 1 WHERE singleton_id = 1",
    ] {
        assert_eq!(connection.execute(statement, [])?, 1);
        let changed = store.database.read_store()?;
        assert_ne!(changed, original);

        assert_eq!(
            store.recheck(None).unwrap_err().failure(),
            OpenFailure::StoreCorrupt,
            "{statement}"
        );

        assert_eq!(store.database.read_store()?, changed);
        assert_eq!(store.record(), &original);
        assert_eq!(fs::read(path.join("store.json"))?, marker);

        assert_eq!(
            connection.execute(
                "UPDATE store_metadata SET region = ?1, created_at_ms = ?2 WHERE singleton_id = 1",
                rusqlite::params![original.region, timestamp],
            )?,
            1
        );

        store.recheck(Some("eu-west-1"))?;
    }

    connection.close().map_err(|(_, error)| error)?;
    close(store)?;
    let reopened = OpenedStore::open(&path, None)?;
    assert_eq!(reopened.record(), &original);
    close(reopened)?;
    directory.finish()?;
    Ok(())
}
