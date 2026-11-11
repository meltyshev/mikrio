//! Real SQLite discovery errors and synthetic bootstrap-integrity rollback.

use super::*;
use crate::metadata::migration::{Format, Registry};

const DEFERRED_VIOLATION: &str = "
    CREATE TABLE invalid_bootstrap_reference (
        owner INTEGER REFERENCES store_metadata(singleton_id)
        DEFERRABLE INITIALLY DEFERRED
    ) STRICT;
    INSERT INTO invalid_bootstrap_reference (owner) VALUES (2);";

static INVALID_BOOTSTRAP: Registry = Registry {
    formats: &[Format {
        version: 1,
        schema: &[DDL, DEFERRED_VIOLATION],
    }],
    transitions: &[],
    checkpoint: None,
};

#[test]
fn exclusive_sqlite_lock_preserves_discovery_busy_cause_and_store_identity() -> TestResult {
    let directory = TestDirectory::new()?.retain_on_failure();
    let database = seed(&directory, false)?;
    let original = database.read_store()?;
    close(database)?;
    let path = directory.path().join("metadata.sqlite3");
    let writer = Connection::open(&path)?;
    // Deliberately use rollback journaling for this discovery-boundary fixture:
    // normal opened stores remain WAL-only, and that policy is restored below.
    writer.pragma_update(None, "journal_mode", "DELETE")?;
    let before = std::fs::read(&path)?;
    let reader = Connection::open(&path)?;
    reader.busy_timeout(std::time::Duration::ZERO)?;

    writer.execute_batch("BEGIN EXCLUSIVE")?;
    let result = crate::metadata::migration::version(&reader);
    writer.execute_batch("ROLLBACK")?;

    assert!(matches!(
        result,
        Err(MetadataError::Sqlite(rusqlite::Error::SqliteFailure(code, _)))
            if code.code == ErrorCode::DatabaseBusy && code.extended_code == 5
    ));

    assert_eq!(crate::metadata::migration::version(&reader)?, 1);
    reader.close().map_err(|(_, error)| error)?;
    writer.close().map_err(|(_, error)| error)?;
    assert_eq!(std::fs::read(&path)?, before);

    let restore = Connection::open(&path)?;
    restore.pragma_update(None, "journal_mode", "WAL")?;
    restore.close().map_err(|(_, error)| error)?;
    let reopened = MetadataDatabase::open(&path, MetadataOpen::Existing)?;
    assert_eq!(reopened.read_store()?, original);
    close(reopened)?;
    directory.finish()?;
    Ok(())
}

#[test]
fn deferred_bootstrap_foreign_key_violation_rolls_back_schema_and_singleton() -> TestResult {
    let directory = TestDirectory::new()?.retain_on_failure();
    let path = directory.path().join("metadata.sqlite3");
    std::fs::File::create_new(&path)?;
    let sentinel = directory.path().join("sentinel");
    std::fs::write(&sentinel, b"preserve unrelated sibling")?;

    let values = BootstrapValues::generate(
        StoreId::from_bytes([94; 16]),
        "eu-west-1".into(),
        EpochMillis::new(1234)?,
    )?;

    let mut database =
        MetadataDatabase::open_registered(&path, MetadataOpen::Initialize, &INVALID_BOOTSTRAP)?;

    assert!(database.is_empty()?);

    // The additional test-only DDL creates a real deferred violation inside
    // the ordinary bootstrap transaction; no callback fabricates its result.
    let result = database.initialize(&values);

    assert!(matches!(
        result,
        Err(MetadataError::Corrupt("bootstrap foreign-key violation"))
    ));

    assert!(database.connection.is_autocommit());
    assert!(database.is_empty()?);

    assert_eq!(
        database.connection.query_row(
            "SELECT count(*) FROM sqlite_schema WHERE name NOT GLOB 'sqlite_*'",
            [],
            |row| row.get::<_, i64>(0),
        )?,
        0
    );

    close(database)?;
    assert_eq!(std::fs::read(&sentinel)?, b"preserve unrelated sibling");

    let mut recovered = MetadataDatabase::open(&path, MetadataOpen::Initialize)?;
    assert!(recovered.is_empty()?);
    recovered.initialize(&values)?;
    assert_eq!(recovered.read_store()?, values.record);
    close(recovered)?;
    let reopened = MetadataDatabase::open(&path, MetadataOpen::Existing)?;
    assert_eq!(reopened.read_store()?, values.record);
    close(reopened)?;
    directory.finish()?;
    Ok(())
}
