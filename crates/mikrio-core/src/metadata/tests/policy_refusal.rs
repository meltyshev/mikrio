//! Real SQLite authorizer restrictions must survive policy readback as refusals.

use std::sync::{Arc, Mutex};

use rusqlite::hooks::{AuthAction, AuthContext, Authorization};

use super::*;
use crate::OpenedStore;
use crate::object::BucketName;

type TestResult = Result<(), Box<dyn std::error::Error>>;

fn ignored_assignment_preserves_store(setting: &'static str) -> TestResult {
    let directory = TestDirectory::new()?.retain_on_failure();
    let path = directory.path().join("store");
    let opened = OpenedStore::open(&path, Some("eu-west-1"))?;
    let original = opened.record().clone();
    opened.close().map_err(|(_, error)| error)?;
    let database_path = path.join("metadata.sqlite3");
    let mut seed = MetadataDatabase::open(&database_path, MetadataOpen::Existing)?;
    seed.create_bucket(&BucketName::parse("preserved")?)?;
    close(seed)?;
    let original_bytes = std::fs::read(&database_path)?;
    let original_marker = std::fs::read(path.join("store.json"))?;
    let mut database = MetadataDatabase::open(&database_path, MetadataOpen::Existing)?;

    let initial = if setting == "locking_mode" {
        "EXCLUSIVE"
    } else {
        "OFF"
    };

    database.connection.pragma_update(None, setting, initial)?;
    let ignored = Arc::new(Mutex::new(Vec::new()));
    let observations = Arc::clone(&ignored);

    database
        .connection
        .authorizer(Some(move |context: AuthContext<'_>| {
            let AuthAction::Pragma {
                pragma_name,
                pragma_value: Some(value),
            } = context.action
            else {
                return Authorization::Allow;
            };

            if pragma_name != setting {
                return Authorization::Allow;
            }

            observations.lock().unwrap().push(value.to_owned());
            Authorization::Ignore
        }))?;

    // SQLite itself ignores only this assignment. Reads still return the real
    // connection setting; the production policy checker must reject it.
    let result = configure(&database.connection, MetadataOpen::Existing);

    if setting == "locking_mode" {
        assert!(matches!(
            result,
            Err(MetadataError::Corrupt(
                "unexpected SQLite journal or locking mode"
            ))
        ));

        assert_eq!(ignored.lock().unwrap().as_slice(), ["NORMAL"]);

        assert_eq!(
            database
                .connection
                .pragma_query_value(None, "journal_mode", |row| row.get::<_, String>(0))?,
            "wal"
        );

        assert_eq!(
            database
                .connection
                .pragma_query_value(None, setting, |row| row.get::<_, String>(0))?,
            "exclusive"
        );
    } else {
        assert!(matches!(
            result,
            Err(MetadataError::Incompatible(
                "SQLite full synchronization unavailable"
            ))
        ));

        assert_eq!(ignored.lock().unwrap().as_slice(), ["ON"]);

        assert_eq!(
            database
                .connection
                .pragma_query_value(None, setting, |row| row.get::<_, i64>(0))?,
            0
        );
    }

    assert!(database.connection.is_autocommit());
    assert_eq!(database.read_store()?, original);

    assert_eq!(
        database.connection.query_row(
            "SELECT count(*) FROM buckets WHERE name='preserved'",
            [],
            |row| row.get::<_, i64>(0)
        )?,
        1
    );

    database
        .connection
        .authorizer(None::<fn(AuthContext<'_>) -> Authorization>)?;

    configure(&database.connection, MetadataOpen::Existing)?;

    assert_eq!(
        database
            .connection
            .pragma_query_value(None, "locking_mode", |row| row.get::<_, String>(0))?,
        "normal"
    );

    for name in ["fullfsync", "checkpoint_fullfsync"] {
        assert_eq!(
            database
                .connection
                .pragma_query_value(None, name, |row| row.get::<_, i64>(0))?,
            1
        );
    }

    close(database)?;
    assert_eq!(std::fs::read(&database_path)?, original_bytes);
    assert_eq!(std::fs::read(path.join("store.json"))?, original_marker);
    let reopened = OpenedStore::open(&path, None)?;
    assert_eq!(reopened.record(), &original);
    reopened.close().map_err(|(_, error)| error)?;
    directory.finish()?;
    Ok(())
}

#[test]
fn ignored_normal_locking_assignment_is_refused_without_changing_store_bytes() -> TestResult {
    ignored_assignment_preserves_store("locking_mode")
}

#[test]
fn ignored_fullfsync_assignment_is_refused_without_changing_store_bytes() -> TestResult {
    ignored_assignment_preserves_store("fullfsync")
}

#[test]
fn ignored_checkpoint_fullfsync_assignment_is_refused_without_changing_store_bytes() -> TestResult {
    ignored_assignment_preserves_store("checkpoint_fullfsync")
}
