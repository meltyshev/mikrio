use super::*;
use rusqlite::types::Value;

fn snapshot(database: &MetadataDatabase) -> rusqlite::Result<Vec<Vec<Value>>> {
    let mut result = Vec::new();

    for sql in [
        "SELECT upload_id, bucket_id, key, state, completion_claim_id, completion_blob_id,
         completion_parts_digest FROM multipart_uploads ORDER BY upload_id",
        "SELECT upload_id, part_number, blob_id, generation FROM multipart_parts
         ORDER BY upload_id, part_number",
        "SELECT blob_id, state, shard, write_kind, target_bucket_id, target_key,
         target_upload_id, target_part_number, gc_pending FROM blobs ORDER BY blob_id",
    ] {
        let mut statement = database.connection.prepare(sql)?;
        let columns = statement.column_count();

        let rows = statement.query_map([], |row| {
            (0..columns).map(|column| row.get(column)).collect()
        })?;

        result.extend(rows.collect::<rusqlite::Result<Vec<Vec<Value>>>>()?);
    }

    Ok(result)
}

fn rejected_claim_preserves_parts(update: &str) -> TestResult {
    let directory = TestDirectory::new()?.retain_on_failure();
    let mut database = initialized(&directory)?;
    let (request, _) = fixture(&mut database)?;
    let selected = database.select_completion(&Authority::Native, &request)?;
    let output = claim(60).blob_id;
    assert!(database.claim_completion(&Authority::Native, &request, &selected, claim(60))?);

    database
        .connection
        .execute_batch("PRAGMA ignore_check_constraints = ON")?;

    assert_eq!(database.connection.execute(update, [])?, 1, "{update}");

    database
        .connection
        .execute_batch("PRAGMA ignore_check_constraints = OFF")?;

    let before = snapshot(&database)?;

    for _ in 0..2 {
        let error = database.gc_mark(output).unwrap_err();
        assert_eq!(error.failure(), OperationFailure::StoreCorrupt, "{update}");

        assert_eq!(
            error.outcome(),
            Some(MutationOutcome::NotCommitted),
            "{update}"
        );

        assert_eq!(snapshot(&database)?, before, "{update}");
        assert!(database.connection.is_autocommit());
    }

    close(database)?;

    let mut reopened = MetadataDatabase::open(
        &directory.path().join("metadata.sqlite3"),
        MetadataOpen::Existing,
    )?;

    let error = reopened.gc_mark(output).unwrap_err();
    assert_eq!(error.failure(), OperationFailure::StoreCorrupt, "{update}");

    assert_eq!(
        error.outcome(),
        Some(MutationOutcome::NotCommitted),
        "{update}"
    );

    assert_eq!(snapshot(&reopened)?, before, "{update}");

    let count: i64 =
        reopened
            .connection
            .query_row("SELECT count(*) FROM multipart_parts", [], |row| row.get(0))?;

    assert_eq!(count, 1, "{update}");
    close(reopened)?;
    directory.finish()?;
    Ok(())
}

#[test]
fn abandoned_completion_rejects_inconsistent_states_and_preserves_acknowledged_parts() -> TestResult
{
    for update in [
        "UPDATE blobs SET state = 'deleting' WHERE write_kind = 'complete_multipart_upload'",
        "UPDATE multipart_uploads SET state = 'open'",
    ] {
        rejected_claim_preserves_parts(update)?;
    }

    Ok(())
}

#[test]
fn abandoned_completion_rejects_every_mismatched_target_without_resetting_the_claim() -> TestResult
{
    for update in [
        "UPDATE blobs SET write_kind = 'put_object' WHERE write_kind = 'complete_multipart_upload'",
        "UPDATE blobs SET target_bucket_id = zeroblob(16) WHERE write_kind = 'complete_multipart_upload'",
        "UPDATE blobs SET target_key = X'646966666572656e74' WHERE write_kind = 'complete_multipart_upload'",
        "UPDATE blobs SET target_upload_id = NULL WHERE write_kind = 'complete_multipart_upload'",
        "UPDATE blobs SET target_part_number = 1 WHERE write_kind = 'complete_multipart_upload'",
    ] {
        rejected_claim_preserves_parts(update)?;
    }

    Ok(())
}
