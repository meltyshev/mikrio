use super::*;
use crate::object::Authority;
use rusqlite::types::Value;

type TestResult = Result<(), Box<dyn std::error::Error>>;
type Rows = (Vec<Vec<Value>>, Vec<Vec<Value>>);

fn state(database: &MetadataDatabase) -> Result<Rows, rusqlite::Error> {
    let mut objects = database.connection.prepare(
        "SELECT bucket_id, key, generation, blob_id, content_type, content_encoding,
         content_disposition, content_language, cache_control, expires_at_ms,
         user_metadata_json, tags_json, last_modified_at_ms, lifecycle_anchor_at_ms
         FROM objects ORDER BY bucket_id, key",
    )?;

    let mut blobs = database.connection.prepare(
        "SELECT blob_id, state, write_kind, target_bucket_id, target_key, byte_length,
         etag, md5_digest, checksum_type, checksums_json, gc_pending, gc_retry_at_ms,
         gc_attempt_count, gc_error_code FROM blobs ORDER BY blob_id",
    )?;

    let read_rows = |statement: &mut rusqlite::Statement<'_>| {
        let count = statement.column_count();

        statement
            .query_map([], |row| {
                (0..count)
                    .map(|column| row.get(column))
                    .collect::<Result<Vec<Value>, _>>()
            })?
            .collect::<Result<Vec<_>, rusqlite::Error>>()
    };

    Ok((read_rows(&mut objects)?, read_rows(&mut blobs)?))
}

fn publish(
    database: &mut MetadataDatabase,
    request: &PutRequest,
    id: u8,
    bytes: &[u8],
) -> TestResult {
    let blob = BlobId::from_bytes([id; 16]);

    let bucket = database
        .reserve_put(&Authority::Native, request, blob)?
        .unwrap();

    database.publish_put(
        &Authority::Native,
        request,
        bucket,
        blob,
        &facts(request, bytes)?,
    )?;

    Ok(())
}

fn reopen(
    database: MetadataDatabase,
    directory: &TestDirectory,
    expected: &Rows,
) -> Result<MetadataDatabase, Box<dyn std::error::Error>> {
    assert!(database.connection.is_autocommit());
    assert_eq!(&state(&database)?, expected);
    close(database)?;

    let reopened = MetadataDatabase::open(
        &directory.path().join("metadata.sqlite3"),
        MetadataOpen::Existing,
    )?;

    assert_eq!(&state(&reopened)?, expected);
    Ok(reopened)
}

fn candidate(database: &mut MetadataDatabase, request: &PutRequest) -> TestResult {
    let current = database.select_object(&Authority::Native, &read(request))?;
    assert_eq!(current.blob_id, BlobId::from_bytes([83; 16]));
    assert_eq!(current.etag, "\"d16fb36f0911f878998c136191af705e\"");
    assert_eq!(current.length.get(), 3);

    assert_eq!(
        database.connection.query_row::<i64, _, _>(
            "SELECT gc_pending FROM blobs WHERE blob_id = ?1",
            [current.blob_id.as_bytes().as_slice()],
            |row| row.get(0),
        )?,
        0
    );

    Ok(())
}

#[test]
fn wildcard_publication_accepts_replaced_destination_and_retires_current_blob() -> TestResult {
    let directory = TestDirectory::new()?.retain_on_failure();
    let (mut database, mut request) = setup(&directory)?;
    publish(&mut database, &request, 81, b"abc")?;
    request.conditions = WriteConditions::new(Some("*".into()), None)?;
    let output = BlobId::from_bytes([83; 16]);

    let bucket = database
        .reserve_put(&Authority::Native, &request, output)?
        .unwrap();

    request.conditions = WriteConditions::default();
    publish(&mut database, &request, 82, b"def")?;
    request.conditions = WriteConditions::new(Some("*".into()), None)?;

    database.publish_put(
        &Authority::Native,
        &request,
        bucket,
        output,
        &facts(&request, b"xyz")?,
    )?;

    candidate(&mut database, &request)?;

    for id in [81, 82] {
        assert_eq!(
            database.connection.query_row::<i64, _, _>(
                "SELECT gc_pending FROM blobs WHERE blob_id = ?1",
                [BlobId::from_bytes([id; 16]).as_bytes().as_slice()],
                |row| row.get(0),
            )?,
            1
        );
    }

    let after = state(&database)?;
    let mut database = reopen(database, &directory, &after)?;
    candidate(&mut database, &request)?;
    close(database)?;
    directory.finish()?;
    Ok(())
}

#[test]
fn exact_etag_publication_preserves_replacement_until_current_etag_matches() -> TestResult {
    let directory = TestDirectory::new()?.retain_on_failure();
    let (mut database, mut request) = setup(&directory)?;
    publish(&mut database, &request, 81, b"abc")?;

    request.conditions =
        WriteConditions::new(Some("\"900150983cd24fb0d6963f7d28e17f72\"".into()), None)?;

    let output = BlobId::from_bytes([83; 16]);

    let bucket = database
        .reserve_put(&Authority::Native, &request, output)?
        .unwrap();

    request.conditions = WriteConditions::default();
    publish(&mut database, &request, 82, b"def")?;

    request.conditions =
        WriteConditions::new(Some("\"900150983cd24fb0d6963f7d28e17f72\"".into()), None)?;

    let before = state(&database)?;

    let error = database
        .publish_put(
            &Authority::Native,
            &request,
            bucket,
            output,
            &facts(&request, b"xyz")?,
        )
        .unwrap_err();

    assert_eq!(error.failure(), OperationFailure::PreconditionFailed);
    assert_eq!(error.outcome(), Some(MutationOutcome::NotCommitted));

    let mut database = reopen(database, &directory, &before)?;
    let current = database.select_object(&Authority::Native, &read(&request))?;
    assert_eq!(current.blob_id, BlobId::from_bytes([82; 16]));
    assert_eq!(current.etag, "\"4ed9407630eb1000c0f6b63842defa7d\"");
    request.conditions = WriteConditions::new(Some(current.etag), None)?;

    database.publish_put(
        &Authority::Native,
        &request,
        bucket,
        output,
        &facts(&request, b"xyz")?,
    )?;

    candidate(&mut database, &request)?;
    close(database)?;
    directory.finish()?;
    Ok(())
}

#[test]
fn wildcard_publication_preserves_deleted_destination_and_its_reservation() -> TestResult {
    let directory = TestDirectory::new()?.retain_on_failure();
    let (mut database, mut request) = setup(&directory)?;
    publish(&mut database, &request, 81, b"abc")?;
    request.conditions = WriteConditions::new(Some("*".into()), None)?;
    let output = BlobId::from_bytes([83; 16]);

    let bucket = database
        .reserve_put(&Authority::Native, &request, output)?
        .unwrap();

    database.delete_object(&Authority::Native, &read(&request), None)?;
    let before = state(&database)?;

    let error = database
        .publish_put(
            &Authority::Native,
            &request,
            bucket,
            output,
            &facts(&request, b"xyz")?,
        )
        .unwrap_err();

    assert_eq!(error.failure(), OperationFailure::NoSuchKey);
    assert_eq!(error.outcome(), Some(MutationOutcome::NotCommitted));
    let mut database = reopen(database, &directory, &before)?;

    assert_eq!(
        database
            .select_object(&Authority::Native, &read(&request))
            .unwrap_err()
            .failure(),
        OperationFailure::NoSuchKey
    );

    request.conditions = WriteConditions::default();
    publish(&mut database, &request, 82, b"def")?;
    request.conditions = WriteConditions::new(Some("*".into()), None)?;

    database.publish_put(
        &Authority::Native,
        &request,
        bucket,
        output,
        &facts(&request, b"xyz")?,
    )?;

    candidate(&mut database, &request)?;
    close(database)?;
    directory.finish()?;
    Ok(())
}
