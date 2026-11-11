use super::*;
use rusqlite::types::Value;

type TestResult = Result<(), Box<dyn std::error::Error>>;

fn snapshot(database: &MetadataDatabase) -> rusqlite::Result<Vec<Vec<Value>>> {
    let mut result = Vec::new();

    for sql in [
        "SELECT row_id, blob_id, shard, state, gc_pending, gc_retry_at_ms, gc_attempt_count,
         gc_error_code, target_bucket_id, target_key, target_upload_id, target_part_number
         FROM blobs ORDER BY row_id",
        "SELECT task, shard, generation, cursor_row_id, upper_row_id, started_at_ms
         FROM maintenance_sweeps ORDER BY task, shard",
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

#[test]
fn cleanup_sample_rejects_mismatched_shard_and_nonpositive_position_without_writes() -> TestResult {
    for update in ["shard = 44", "row_id = 0", "row_id = -1"] {
        let directory = TestDirectory::new()?.retain_on_failure();
        let (mut database, request) = setup(&directory)?;
        let blob = BlobId::from_bytes([43; 16]);
        database.reserve_put(&crate::object::Authority::Native, &request, blob)?;

        database
            .connection
            .execute_batch("PRAGMA ignore_check_constraints = ON")?;

        assert_eq!(
            database
                .connection
                .execute(&format!("UPDATE blobs SET {update}"), [])?,
            1
        );

        database
            .connection
            .execute_batch("PRAGMA ignore_check_constraints = OFF")?;

        let before = snapshot(&database)?;
        let changes = database.connection.total_changes();

        for _ in 0..2 {
            let error = database
                .cleanup_sample(None)
                .err()
                .expect("malformed sample must fail");

            assert_eq!(
                error.failure(),
                OperationFailure::StoreCorrupt,
                "update={update}"
            );

            assert_eq!(error.outcome(), None, "update={update}");
            assert_eq!(snapshot(&database)?, before, "update={update}");

            assert_eq!(
                database.connection.total_changes(),
                changes,
                "update={update}"
            );
        }

        close(database)?;
        directory.finish()?;
    }

    Ok(())
}

#[test]
fn cleanup_page_rejects_wrong_shard_without_mutating_sweep_progress() -> TestResult {
    let directory = TestDirectory::new()?.retain_on_failure();
    let (mut database, request) = setup(&directory)?;
    let blob = BlobId::from_bytes([43; 16]);
    database.reserve_put(&crate::object::Authority::Native, &request, blob)?;

    database.connection.execute_batch(
        "PRAGMA ignore_check_constraints = ON; UPDATE blobs SET shard = 44;
         PRAGMA ignore_check_constraints = OFF;",
    )?;

    let before = snapshot(&database)?;

    for _ in 0..2 {
        let page = database.gc_page(44)?.unwrap();
        assert_eq!(page.candidates.len(), 1);

        let error = page.candidates[0]
            .1
            .as_ref()
            .expect_err("wrong shard must fail");

        assert_eq!(error.failure(), OperationFailure::StoreCorrupt);
        assert_eq!(error.outcome(), None);
        assert_eq!(snapshot(&database)?, before);
        assert!(database.connection.is_autocommit());
    }

    close(database)?;
    directory.finish()?;
    Ok(())
}

#[test]
fn repeated_gc_finalization_keeps_an_unrelated_reservation_after_reopen() -> TestResult {
    let directory = TestDirectory::new()?.retain_on_failure();
    let (mut database, request) = setup(&directory)?;
    let finished = BlobId::from_bytes([43; 16]);
    let survivor = BlobId::from_bytes([44; 16]);

    for blob in [finished, survivor] {
        database.reserve_put(&crate::object::Authority::Native, &request, blob)?;
    }

    assert!(database.gc_mark(finished)?);
    database.gc_finish(finished)?;
    let before = snapshot(&database)?;
    close(database)?;

    let mut reopened = MetadataDatabase::open(
        &directory.path().join("metadata.sqlite3"),
        MetadataOpen::Existing,
    )?;

    for _ in 0..2 {
        assert!(!reopened.gc_mark(finished)?);
        reopened.gc_finish(finished)?;
        assert_eq!(snapshot(&reopened)?, before);
    }

    let remaining: Vec<u8> =
        reopened
            .connection
            .query_row("SELECT blob_id FROM blobs", [], |row| row.get(0))?;

    assert_eq!(remaining, survivor.as_bytes());
    close(reopened)?;
    directory.finish()?;
    Ok(())
}

#[test]
fn gc_finalization_refuses_reserved_and_ready_blobs_without_erasing_them() -> TestResult {
    for ready in [false, true] {
        let directory = TestDirectory::new()?.retain_on_failure();
        let (mut database, request) = setup(&directory)?;
        let blob = BlobId::from_bytes([43; 16]);

        let bucket = database
            .reserve_put(&crate::object::Authority::Native, &request, blob)?
            .unwrap();

        if ready {
            database.publish_put(
                &crate::object::Authority::Native,
                &request,
                bucket,
                blob,
                &facts(&request, b"preserved object")?,
            )?;
        }

        let before = snapshot(&database)?;

        for _ in 0..2 {
            let error = database.gc_finish(blob).unwrap_err();

            assert_eq!(
                error.failure(),
                OperationFailure::StoreCorrupt,
                "ready={ready}"
            );

            assert_eq!(
                error.outcome(),
                Some(MutationOutcome::NotCommitted),
                "ready={ready}"
            );

            assert_eq!(snapshot(&database)?, before, "ready={ready}");
        }

        if ready {
            let selected =
                database.select_object(&crate::object::Authority::Native, &read(&request))?;

            assert_eq!(selected.blob_id, blob);
        }

        close(database)?;
        directory.finish()?;
    }

    Ok(())
}
