use super::*;
use rusqlite::types::Value;

type TestResult = Result<(), Box<dyn std::error::Error>>;

fn initialized(directory: &TestDirectory) -> Result<MetadataDatabase, Box<dyn std::error::Error>> {
    let mut database = database(directory)?;

    database.initialize(&BootstrapValues::generate(
        StoreId::from_bytes([13; 16]),
        "us-east-1".into(),
        EpochMillis::new(0)?,
    )?)?;

    assert_eq!(
        database.read_store()?.store_id,
        StoreId::from_bytes([13; 16])
    );

    Ok(database)
}

fn unchanged_refusal(database: &MetadataDatabase, query: &str, reason: &'static str) -> TestResult {
    let before: Value = database.connection.query_row(query, [], |row| row.get(0))?;
    let changes = database.connection.total_changes();

    for _ in 0..2 {
        assert!(
            matches!(database.read_store(), Err(MetadataError::Corrupt(actual)) if actual == reason),
            "stored value must be rejected with its specific corruption reason: {query} = {before:?}"
        );

        let after: Value = database.connection.query_row(query, [], |row| row.get(0))?;

        assert_eq!(
            after, before,
            "a refused read must preserve the malformed value"
        );

        assert_eq!(database.connection.total_changes(), changes);
    }

    Ok(())
}

#[test]
fn malformed_store_identity_and_layout_are_preserved_across_reopen() -> TestResult {
    for (column, value, reason) in [
        (
            "owner_account_id",
            Value::Text("12345678901".into()),
            "invalid store configuration",
        ),
        (
            "owner_account_id",
            Value::Text("12345678901x".into()),
            "invalid store configuration",
        ),
        (
            "canonical_owner_id",
            Value::Text("a".repeat(63)),
            "invalid store configuration",
        ),
        (
            "canonical_owner_id",
            Value::Text("A".repeat(64)),
            "invalid store configuration",
        ),
        (
            "shard_count",
            Value::Integer(255),
            "invalid store configuration",
        ),
        (
            "shard_algorithm",
            Value::Text("last_byte".into()),
            "invalid store configuration",
        ),
        (
            "continuation_token_key",
            Value::Blob(vec![3; 31]),
            "invalid store configuration",
        ),
        (
            "store_id",
            Value::Blob(vec![13; 15]),
            "invalid store identity",
        ),
        (
            "created_at_ms",
            Value::Integer(i64::MAX),
            "invalid store creation time",
        ),
    ] {
        let directory = TestDirectory::new()?;
        let database = initialized(&directory)?;
        let sentinel = directory.path().join("unrelated-evidence");
        std::fs::write(&sentinel, b"preserved")?;

        // Only this fixture bypasses CHECK constraints to model malformed persisted data.
        // The exact schema and ordinary connection policy remain unchanged.
        database
            .connection
            .pragma_update(None, "ignore_check_constraints", true)?;

        let changed = database.connection.execute(
            &format!("UPDATE store_metadata SET {column} = ?1 WHERE singleton_id = 1"),
            [&value],
        )?;

        assert_eq!(changed, 1);

        database
            .connection
            .pragma_update(None, "ignore_check_constraints", false)?;

        let query = format!("SELECT {column} FROM store_metadata WHERE singleton_id = 1");
        unchanged_refusal(&database, &query, reason)?;
        close(database)?;

        let reopened = MetadataDatabase::open(
            &directory.path().join("metadata.sqlite3"),
            MetadataOpen::Existing,
        )?;

        unchanged_refusal(&reopened, &query, reason)?;
        assert_eq!(std::fs::read(&sentinel)?, b"preserved");
        close(reopened)?;
        directory.finish()?;
    }

    Ok(())
}

#[test]
fn malformed_sweep_bounds_are_preserved_across_reopen() -> TestResult {
    for (column, value) in [
        ("generation", Value::Blob(vec![1; 15])),
        ("cursor_row_id", Value::Integer(-1)),
        ("upper_row_id", Value::Integer(-1)),
        ("started_at_ms", Value::Integer(i64::MAX)),
    ] {
        let directory = TestDirectory::new()?;
        let database = initialized(&directory)?;

        database
            .connection
            .pragma_update(None, "ignore_check_constraints", true)?;

        let changed = database.connection.execute(
            &format!(
                "UPDATE maintenance_sweeps SET {column} = ?1 WHERE task = 'gc_blobs' AND shard = 7"
            ),
            [&value],
        )?;

        assert_eq!(changed, 1);

        database
            .connection
            .pragma_update(None, "ignore_check_constraints", false)?;

        let query = format!(
            "SELECT {column} FROM maintenance_sweeps WHERE task = 'gc_blobs' AND shard = 7"
        );

        unchanged_refusal(&database, &query, "invalid maintenance sweep")?;
        close(database)?;

        let reopened = MetadataDatabase::open(
            &directory.path().join("metadata.sqlite3"),
            MetadataOpen::Existing,
        )?;

        unchanged_refusal(&reopened, &query, "invalid maintenance sweep")?;
        close(reopened)?;
        directory.finish()?;
    }

    Ok(())
}

#[test]
fn extra_store_singleton_is_preserved_and_refused_after_reopen() -> TestResult {
    let directory = TestDirectory::new()?;
    let database = initialized(&directory)?;

    database.connection.execute_batch(
        "PRAGMA ignore_check_constraints = ON;
         INSERT INTO store_metadata (singleton_id, store_id, owner_account_id, canonical_owner_id,
          format_version, region, shard_count, shard_algorithm, continuation_token_key, created_at_ms)
         SELECT 2, store_id, owner_account_id, canonical_owner_id, format_version, region,
          shard_count, shard_algorithm, continuation_token_key, created_at_ms
          FROM store_metadata WHERE singleton_id = 1;
         PRAGMA ignore_check_constraints = OFF;",
    )?;

    let count: i64 =
        database
            .connection
            .query_row("SELECT count(*) FROM store_metadata", [], |row| row.get(0))?;

    assert_eq!(count, 2);

    unchanged_refusal(
        &database,
        "SELECT count(*) FROM store_metadata",
        "invalid store singleton count",
    )?;

    close(database)?;

    let reopened = MetadataDatabase::open(
        &directory.path().join("metadata.sqlite3"),
        MetadataOpen::Existing,
    )?;

    unchanged_refusal(
        &reopened,
        "SELECT count(*) FROM store_metadata",
        "invalid store singleton count",
    )?;

    close(reopened)?;
    directory.finish()?;
    Ok(())
}

#[test]
fn unsettled_metadata_read_preserves_transaction_until_explicit_rollback() -> TestResult {
    let directory = TestDirectory::new()?;
    let database = initialized(&directory)?;
    let before = database.read_store()?;

    database.connection.execute_batch(
        "BEGIN IMMEDIATE; UPDATE store_metadata SET region = 'eu-west-1' WHERE singleton_id = 1;",
    )?;

    assert!(!database.connection.is_autocommit());

    unchanged_refusal(
        &database,
        "SELECT region FROM store_metadata WHERE singleton_id = 1",
        "metadata transaction has not settled",
    )?;

    assert!(
        !database.connection.is_autocommit(),
        "a refused read cannot settle another operation's transaction"
    );

    database.connection.execute_batch("ROLLBACK")?;
    assert!(database.connection.is_autocommit());
    assert_eq!(database.read_store()?, before);
    close(database)?;

    let reopened = MetadataDatabase::open(
        &directory.path().join("metadata.sqlite3"),
        MetadataOpen::Existing,
    )?;

    assert_eq!(reopened.read_store()?, before);
    close(reopened)?;
    directory.finish()?;
    Ok(())
}
