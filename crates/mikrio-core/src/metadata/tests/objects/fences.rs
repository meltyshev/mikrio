use super::*;
use rusqlite::types::Value;
use std::collections::BTreeMap;

type TestResult = Result<(), Box<dyn std::error::Error>>;
type Rows = BTreeMap<String, Vec<Vec<Value>>>;

fn inventory(database: &MetadataDatabase) -> Result<Rows, rusqlite::Error> {
    let mut names = database
        .connection
        .prepare("SELECT name FROM sqlite_schema WHERE type = 'table' ORDER BY name")?;

    let names = names
        .query_map([], |row| row.get::<_, String>(0))?
        .collect::<Result<Vec<_>, _>>()?;

    assert_eq!(names.len(), 13);
    assert!(names.iter().any(|name| name == "sqlite_sequence"));

    let mut result = Rows::new();

    for name in names {
        assert!(
            name.chars()
                .all(|character| character.is_ascii_alphanumeric() || character == '_')
        );

        let mut statement = database
            .connection
            .prepare(&format!("SELECT * FROM {name}"))?;

        let columns = statement.column_count();

        let mut rows = statement
            .query_map([], |row| {
                (0..columns)
                    .map(|index| row.get(index))
                    .collect::<Result<Vec<Value>, _>>()
            })?
            .collect::<Result<Vec<_>, _>>()?;

        rows.sort_by_cached_key(|row| format!("{row:?}"));
        result.insert(name, rows);
    }

    Ok(result)
}

fn published(
    directory: &TestDirectory,
) -> Result<(MetadataDatabase, PutRequest), Box<dyn std::error::Error>> {
    let (mut database, request) = setup(directory)?;
    let blob = BlobId::from_bytes([73; 16]);

    let bucket = database
        .reserve_put(&crate::object::Authority::Native, &request, blob)?
        .unwrap();

    database.publish_put(
        &crate::object::Authority::Native,
        &request,
        bucket,
        blob,
        &facts(&request, b"unchanged predecessor")?,
    )?;

    Ok((database, request))
}

fn refusal(error: &crate::OperationError) {
    assert_eq!(error.failure(), OperationFailure::StoreCorrupt);
    assert_eq!(error.outcome(), Some(MutationOutcome::NotCommitted));
}

fn reopen_unchanged(
    database: MetadataDatabase,
    directory: &TestDirectory,
    before: &Rows,
) -> Result<MetadataDatabase, Box<dyn std::error::Error>> {
    assert!(database.connection.is_autocommit());
    assert_eq!(&inventory(&database)?, before);

    database
        .connection
        .execute_batch("DROP TRIGGER suppress_change")?;

    close(database)?;

    let reopened = MetadataDatabase::open(
        &directory.path().join("metadata.sqlite3"),
        MetadataOpen::Existing,
    )?;

    assert_eq!(&inventory(&reopened)?, before);
    Ok(reopened)
}

#[test]
fn lost_tag_generation_update_refuses_success_and_preserves_reopened_object() -> TestResult {
    let directory = TestDirectory::new()?;
    let (mut database, request) = published(&directory)?;
    let before = inventory(&database)?;
    let tags = crate::object::ObjectTags::new([("Status".into(), "Ready".into())])?;

    // An owned connection-local fault makes real SQLite report zero changed rows.
    // It does not alter the persisted schema or replace the transaction engine.
    database.connection.execute_batch(
        "CREATE TEMP TRIGGER suppress_change BEFORE UPDATE OF tags_json ON objects
         BEGIN SELECT RAISE(IGNORE); END",
    )?;

    refusal(
        &database
            .replace_object_tags(&read(&request), &tags)
            .unwrap_err(),
    );

    let mut database = reopen_unchanged(database, &directory, &before)?;
    let original = database.select_object(&crate::object::Authority::Native, &read(&request))?;
    database.replace_object_tags(&read(&request), &tags)?;
    let changed = database.select_object(&crate::object::Authority::Native, &read(&request))?;
    assert_eq!(changed.tags, tags);
    assert_ne!(changed.generation, original.generation);
    assert_eq!(changed.blob_id, original.blob_id);
    assert_eq!(changed.etag, original.etag);
    assert_eq!(changed.checksums, original.checksums);
    close(database)?;
    directory.finish()?;
    Ok(())
}

#[test]
fn lost_delete_generation_refuses_success_and_preserves_reopened_object() -> TestResult {
    let directory = TestDirectory::new()?;
    let (mut database, request) = published(&directory)?;
    let before = inventory(&database)?;

    database.connection.execute_batch(
        "CREATE TEMP TRIGGER suppress_change BEFORE DELETE ON objects
         BEGIN SELECT RAISE(IGNORE); END",
    )?;

    refusal(
        &database
            .delete_object(&crate::object::Authority::Native, &read(&request), None)
            .unwrap_err(),
    );

    let mut database = reopen_unchanged(database, &directory, &before)?;
    database.delete_object(&crate::object::Authority::Native, &read(&request), None)?;

    assert_eq!(
        database
            .select_object(&crate::object::Authority::Native, &read(&request))
            .unwrap_err()
            .failure(),
        OperationFailure::NoSuchKey
    );

    assert_eq!(
        database
            .connection
            .query_row::<i64, _, _>("SELECT gc_pending FROM blobs", [], |row| row.get(0))?,
        1
    );

    close(database)?;
    directory.finish()?;
    Ok(())
}

#[test]
fn lost_blob_retirement_rolls_back_prior_object_deletion_across_reopen() -> TestResult {
    let directory = TestDirectory::new()?;
    let (mut database, request) = published(&directory)?;
    let before = inventory(&database)?;

    database.connection.execute_batch(
        "CREATE TEMP TRIGGER suppress_change BEFORE UPDATE OF gc_pending ON blobs
         BEGIN SELECT RAISE(IGNORE); END",
    )?;

    refusal(
        &database
            .delete_object(&crate::object::Authority::Native, &read(&request), None)
            .unwrap_err(),
    );

    let mut database = reopen_unchanged(database, &directory, &before)?;
    database.delete_object(&crate::object::Authority::Native, &read(&request), None)?;

    assert_eq!(
        database
            .select_object(&crate::object::Authority::Native, &read(&request))
            .unwrap_err()
            .failure(),
        OperationFailure::NoSuchKey
    );

    assert_eq!(
        database
            .connection
            .query_row::<i64, _, _>("SELECT gc_pending FROM blobs", [], |row| row.get(0))?,
        1
    );

    close(database)?;
    directory.finish()?;
    Ok(())
}
