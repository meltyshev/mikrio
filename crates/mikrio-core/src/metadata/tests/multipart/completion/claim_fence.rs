use super::*;
use rusqlite::types::Value;

fn rows(database: &MetadataDatabase, sql: &str) -> rusqlite::Result<Vec<Vec<Value>>> {
    let mut statement = database.connection.prepare(sql)?;
    let columns = statement.column_count();

    statement
        .query_map([], |row| {
            (0..columns).map(|column| row.get(column)).collect()
        })?
        .collect()
}

fn snapshot(database: &MetadataDatabase) -> rusqlite::Result<Vec<Vec<Value>>> {
    let mut result = Vec::new();

    for sql in [
        "SELECT row_id, upload_id, bucket_id, key, initiator_id, state,
         completion_claim_id, completion_blob_id, completion_parts_digest,
         checksum_algorithm, checksum_type, content_type, content_encoding,
         content_disposition, content_language, cache_control, expires_at_ms,
         user_metadata_json, tags_json, initiated_at_ms
         FROM multipart_uploads ORDER BY upload_id",
        "SELECT upload_id, part_number, generation, blob_id, last_modified_at_ms
         FROM multipart_parts ORDER BY upload_id, part_number",
        "SELECT row_id, blob_id, shard, state, byte_length, etag, md5_digest, part_count,
         checksum_type, checksums_json, write_kind, target_bucket_id, target_key,
         target_upload_id, target_part_number, created_at_ms, gc_pending, gc_retry_at_ms,
         gc_attempt_count, gc_error_code FROM blobs ORDER BY blob_id",
    ] {
        result.extend(rows(database, sql)?);
    }

    Ok(result)
}

#[test]
fn lost_abandoned_claim_reset_preserves_parts_and_refuses_deletion_across_reopen() -> TestResult {
    let directory = TestDirectory::new()?.retain_on_failure();
    let mut database = initialized(&directory)?;
    let (request, facts) = fixture(&mut database)?;
    let selected = database.select_completion(&Authority::Native, &request)?;
    let output = claim(60).blob_id;
    assert!(database.claim_completion(&Authority::Native, &request, &selected, claim(60))?);
    let before = snapshot(&database)?;

    // This connection-local fault makes the real claim-reset statement change no
    // rows. It does not change the persisted schema or replace the transaction.
    database.connection.execute_batch(
        "CREATE TEMP TRIGGER suppress_claim_reset BEFORE UPDATE OF state ON multipart_uploads
         WHEN OLD.state = 'completing' AND NEW.state = 'open'
         BEGIN SELECT RAISE(IGNORE); END",
    )?;

    let error = database.gc_mark(output).unwrap_err();
    assert_eq!(error.failure(), OperationFailure::StoreCorrupt);
    assert_eq!(error.outcome(), Some(MutationOutcome::NotCommitted));
    assert!(database.connection.is_autocommit());
    assert_eq!(snapshot(&database)?, before);

    database
        .connection
        .execute_batch("DROP TRIGGER suppress_claim_reset")?;

    close(database)?;

    let mut database = MetadataDatabase::open(
        &directory.path().join("metadata.sqlite3"),
        MetadataOpen::Existing,
    )?;

    assert_eq!(snapshot(&database)?, before);

    let acknowledged = rows(
        &database,
        "SELECT upload_id, part_number, generation, blob_id, last_modified_at_ms
         FROM multipart_parts ORDER BY upload_id, part_number",
    )?;

    assert_eq!(acknowledged.len(), 1);

    // SQL-only recovery: physical shard exclusion and unlink belong to the
    // existing process tests. Clearing the fault must permit normal recovery.
    assert!(database.gc_mark(output)?);

    let state = rows(
        &database,
        "SELECT state, completion_claim_id, completion_blob_id, completion_parts_digest
         FROM multipart_uploads ORDER BY upload_id",
    )?;

    assert_eq!(
        state,
        vec![vec![
            Value::Text("open".into()),
            Value::Null,
            Value::Null,
            Value::Null
        ]]
    );

    let deleting: (String, i64) = database.connection.query_row(
        "SELECT state, gc_pending FROM blobs WHERE blob_id = ?1",
        [output.as_bytes().as_slice()],
        |row| Ok((row.get(0)?, row.get(1)?)),
    )?;

    assert_eq!(deleting, ("deleting".into(), 1));
    database.gc_finish(output)?;

    assert_eq!(
        rows(
            &database,
            "SELECT upload_id, part_number, generation, blob_id, last_modified_at_ms
             FROM multipart_parts ORDER BY upload_id, part_number",
        )?,
        acknowledged
    );

    let recovered = database.select_completion(&Authority::Native, &request)?;
    assert_eq!(recovered.fingerprint, selected.fingerprint);
    assert!(database.claim_completion(&Authority::Native, &request, &recovered, claim(62))?);

    let completed =
        database.publish_completion(&Authority::Native, &request, &recovered, claim(62), &facts)?;

    assert_eq!(completed.etag, "\"af5da9f45af7a300e3aded972f8ff687-1\"");
    assert_eq!(completed.length.get(), 3);

    assert_eq!(
        completed.checksum.encode(),
        "T4tCwi3TcptRm6b2jS2nzFstYG0F2u1a1RKMwD5sY1g=-1"
    );

    assert_eq!(count(&database)?, 0);
    close(database)?;
    directory.finish()?;
    Ok(())
}
