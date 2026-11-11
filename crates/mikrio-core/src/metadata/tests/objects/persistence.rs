use super::*;
use crate::object::Authority;
use rusqlite::types::Value;

type TestResult = Result<(), Box<dyn std::error::Error>>;

fn ledger(database: &MetadataDatabase) -> Result<Vec<Value>, rusqlite::Error> {
    database.connection.query_row(
        "SELECT state, shard, md5_digest, part_count, etag, checksum_type, checksums_json,
         target_key, gc_pending, gc_attempt_count FROM blobs ORDER BY row_id LIMIT 1",
        [],
        |row| (0..10).map(|index| row.get(index)).collect(),
    )
}

fn corrupt_read_refusal(
    database: &mut MetadataDatabase,
    request: &PutRequest,
    name: &str,
) -> TestResult {
    let before = ledger(database)?;
    let changes = database.connection.total_changes();

    let error = database
        .select_object(&Authority::Native, &read(request))
        .expect_err(name);

    assert_eq!(error.failure(), OperationFailure::StoreCorrupt, "{name}");
    assert_eq!(error.outcome(), None, "{name}");

    let error = database
        .delete_object(&Authority::Native, &read(request), None)
        .expect_err(name);

    assert_eq!(error.failure(), OperationFailure::StoreCorrupt, "{name}");

    assert_eq!(
        error.outcome(),
        Some(MutationOutcome::NotCommitted),
        "{name}"
    );

    assert_eq!(ledger(database)?, before);
    assert_eq!(database.connection.total_changes(), changes);

    assert_eq!(
        database
            .connection
            .query_row::<i64, _, _>("SELECT count(*) FROM objects", [], |row| row.get(0))?,
        1
    );

    Ok(())
}

#[test]
fn reserved_stored_tags_are_refused_without_mutation_after_reopen() -> TestResult {
    let directory = TestDirectory::new()?;
    let (mut database, request) = setup(&directory)?;
    let blob = BlobId::from_bytes([3; 16]);

    let bucket = database
        .reserve_put(&Authority::Native, &request, blob)?
        .unwrap();

    database.publish_put(
        &Authority::Native,
        &request,
        bucket,
        blob,
        &facts(&request, b"original")?,
    )?;

    // The SQL envelope permits this JSON; the format's typed decoder must
    // reject the forbidden key without rewriting or deleting its evidence.
    let encoded = r#"{"AWS:reserved":"preserve"}"#;
    database
        .connection
        .execute("UPDATE objects SET tags_json = ?1", [encoded])?;
    close(database)?;

    let mut reopened = MetadataDatabase::open(
        &directory.path().join("metadata.sqlite3"),
        MetadataOpen::Existing,
    )?;

    corrupt_read_refusal(&mut reopened, &request, "reserved tag prefix")?;

    let stored: String =
        reopened
            .connection
            .query_row("SELECT tags_json FROM objects", [], |row| row.get(0))?;

    assert_eq!(stored, encoded);
    close(reopened)?;
    directory.finish()?;
    Ok(())
}

#[test]
fn malformed_object_facts_remain_refused_and_preserved_after_reopen() -> TestResult {
    for (name, assignment) in [
        ("non-ready blob", "state = 'deleting'"),
        ("wrong shard", "shard = 9"),
        ("short digest", "md5_digest = zeroblob(15)"),
        ("zero multipart count", "part_count = 0"),
        ("excessive multipart count", "part_count = 10001"),
        (
            "mismatched multipart ETag",
            "part_count = 2, etag = '\"00000000000000000000000000000000-3\"'",
        ),
        ("composite without parts", "checksum_type = 'COMPOSITE'"),
        (
            "oversized checksum document",
            "checksums_json = '{}' || replace(hex(zeroblob(2048)), '0', ' ')",
        ),
        (
            "mismatched checksum parts",
            "part_count = 2, etag = '\"00000000000000000000000000000000-2\"', checksum_type = 'COMPOSITE', checksums_json = '{\"CRC32\":\"AAAAAA==-3\"}'",
        ),
    ] {
        let directory = TestDirectory::new()?;
        let (mut database, request) = setup(&directory)?;
        let blob = BlobId::from_bytes([3; 16]);

        let bucket = database
            .reserve_put(&Authority::Native, &request, blob)?
            .unwrap();

        database.publish_put(
            &Authority::Native,
            &request,
            bucket,
            blob,
            &facts(&request, b"original")?,
        )?;

        let sentinel = directory.path().join("unrelated");
        std::fs::write(&sentinel, b"preserved")?;

        // Only the fixture bypasses constraints to model malformed persisted facts.
        database.connection.execute_batch(&format!(
            "PRAGMA ignore_check_constraints = ON; UPDATE blobs SET {assignment};
             PRAGMA ignore_check_constraints = OFF;"
        ))?;

        corrupt_read_refusal(&mut database, &request, name)?;
        let before = ledger(&database)?;
        close(database)?;

        let mut reopened = MetadataDatabase::open(
            &directory.path().join("metadata.sqlite3"),
            MetadataOpen::Existing,
        )?;

        assert_eq!(ledger(&reopened)?, before, "{name}");
        corrupt_read_refusal(&mut reopened, &request, name)?;
        assert_eq!(std::fs::read(&sentinel)?, b"preserved", "{name}");
        close(reopened)?;
        directory.finish()?;
    }

    Ok(())
}

#[test]
fn lost_publication_reservation_preserves_previous_mapping_across_reopen() -> TestResult {
    let directory = TestDirectory::new()?;
    let (mut database, request) = setup(&directory)?;
    let original_blob = BlobId::from_bytes([3; 16]);

    let bucket = database
        .reserve_put(&Authority::Native, &request, original_blob)?
        .unwrap();

    database.publish_put(
        &Authority::Native,
        &request,
        bucket,
        original_blob,
        &facts(&request, b"original")?,
    )?;

    let original = database.select_object(&Authority::Native, &read(&request))?;
    let replacement = BlobId::from_bytes([4; 16]);

    assert_eq!(
        database.reserve_put(&Authority::Native, &request, replacement)?,
        Some(bucket)
    );

    database.connection.execute(
        "UPDATE blobs SET target_key = x'6f74686572' WHERE blob_id = ?1",
        [replacement.as_bytes().as_slice()],
    )?;

    let error = database
        .publish_put(
            &Authority::Native,
            &request,
            bucket,
            replacement,
            &facts(&request, b"replacement")?,
        )
        .unwrap_err();

    assert_eq!(error.failure(), OperationFailure::StoreCorrupt);
    assert_eq!(error.outcome(), Some(MutationOutcome::NotCommitted));
    let selected = database.select_object(&Authority::Native, &read(&request))?;
    assert_eq!(selected.blob_id, original_blob);
    assert_eq!(selected.generation, original.generation);
    assert_eq!(selected.etag, original.etag);

    let reservation: (String, Vec<u8>) = database.connection.query_row(
        "SELECT state, target_key FROM blobs WHERE blob_id = ?1",
        [replacement.as_bytes().as_slice()],
        |row| Ok((row.get(0)?, row.get(1)?)),
    )?;

    assert_eq!(reservation, ("reserved".into(), b"other".to_vec()));
    close(database)?;

    let mut reopened = MetadataDatabase::open(
        &directory.path().join("metadata.sqlite3"),
        MetadataOpen::Existing,
    )?;

    let selected = reopened.select_object(&Authority::Native, &read(&request))?;
    assert_eq!(selected.blob_id, original_blob);
    assert_eq!(selected.generation, original.generation);
    assert_eq!(selected.etag, original.etag);
    assert!(reopened.gc_mark(replacement)?);
    reopened.gc_finish(replacement)?;

    assert_eq!(
        reopened
            .select_object(&Authority::Native, &read(&request))?
            .blob_id,
        original_blob
    );

    close(reopened)?;
    directory.finish()?;

    Ok(())
}

#[test]
fn duplicate_blob_reservation_cannot_retarget_existing_work() -> TestResult {
    let directory = TestDirectory::new()?;
    let (mut database, mut request) = setup(&directory)?;
    let blob = BlobId::from_bytes([7; 16]);

    let bucket = database
        .reserve_put(&Authority::Native, &request, blob)?
        .unwrap();

    let original_key = request.key.clone();
    let before = ledger(&database)?;
    request.key = ObjectKey::parse("another-key")?;

    assert_eq!(
        database.reserve_put(&Authority::Native, &request, blob)?,
        None
    );

    assert_eq!(ledger(&database)?, before);
    request.key = original_key;

    database.publish_put(
        &Authority::Native,
        &request,
        bucket,
        blob,
        &facts(&request, b"original")?,
    )?;

    let original = database.select_object(&Authority::Native, &read(&request))?;
    close(database)?;

    let mut reopened = MetadataDatabase::open(
        &directory.path().join("metadata.sqlite3"),
        MetadataOpen::Existing,
    )?;

    let selected = reopened.select_object(&Authority::Native, &read(&request))?;
    assert_eq!(selected.blob_id, blob);
    assert_eq!(selected.generation, original.generation);
    request.key = ObjectKey::parse("another-key")?;

    assert_eq!(
        reopened
            .select_object(&Authority::Native, &read(&request))
            .unwrap_err()
            .failure(),
        OperationFailure::NoSuchKey
    );

    close(reopened)?;
    directory.finish()?;

    Ok(())
}

#[test]
fn gc_refuses_malformed_ledger_and_preserves_it_across_reopen() -> TestResult {
    for (name, assignment, retry) in [
        ("wrong shard", "shard = 9", false),
        ("unknown state", "state = 'unknown'", false),
        ("negative retry count", "gc_attempt_count = -1", true),
    ] {
        let directory = TestDirectory::new()?;
        let (mut database, request) = setup(&directory)?;
        let blob = BlobId::from_bytes([3; 16]);

        database
            .reserve_put(&Authority::Native, &request, blob)?
            .unwrap();

        database.connection.execute_batch(&format!(
            "PRAGMA ignore_check_constraints = ON; UPDATE blobs SET {assignment};
             PRAGMA ignore_check_constraints = OFF;"
        ))?;

        let before = ledger(&database)?;

        let error = if retry {
            database.gc_failed(blob).unwrap_err()
        } else {
            database.gc_mark(blob).unwrap_err()
        };

        assert_eq!(error.failure(), OperationFailure::StoreCorrupt, "{name}");

        assert_eq!(
            error.outcome(),
            Some(MutationOutcome::NotCommitted),
            "{name}"
        );

        assert_eq!(ledger(&database)?, before, "{name}");
        close(database)?;

        let mut reopened = MetadataDatabase::open(
            &directory.path().join("metadata.sqlite3"),
            MetadataOpen::Existing,
        )?;

        assert_eq!(ledger(&reopened)?, before, "{name}");

        let error = if retry {
            reopened.gc_failed(blob).unwrap_err()
        } else {
            reopened.gc_mark(blob).unwrap_err()
        };

        assert_eq!(
            error.failure(),
            OperationFailure::StoreCorrupt,
            "reopened {name}"
        );

        assert_eq!(ledger(&reopened)?, before, "reopened {name}");
        close(reopened)?;
        directory.finish()?;
    }

    Ok(())
}

#[test]
fn missing_blob_record_refuses_read_and_delete_without_erasing_object_mapping() -> TestResult {
    fn mapping(database: &MetadataDatabase) -> rusqlite::Result<Vec<Value>> {
        let mut statement = database.connection.prepare("SELECT * FROM objects")?;
        let columns = statement.column_count();
        statement.query_row([], |row| (0..columns).map(|index| row.get(index)).collect())
    }

    let directory = TestDirectory::new()?.retain_on_failure();
    let (mut database, request) = setup(&directory)?;
    let blob = BlobId::from_bytes([51; 16]);

    let bucket = database
        .reserve_put(&Authority::Native, &request, blob)?
        .ok_or("fixture reservation missing")?;

    database.publish_put(
        &Authority::Native,
        &request,
        bucket,
        blob,
        &facts(&request, b"preserved logical mapping")?,
    )?;

    let expected_mapping = mapping(&database)?;

    // Only the owned fixture bypasses the foreign key to model missing
    // persisted blob metadata. Ordinary writes must keep this reference valid.
    database
        .connection
        .execute_batch("PRAGMA foreign_keys = OFF")?;

    assert_eq!(
        database.connection.execute(
            "DELETE FROM blobs WHERE blob_id = ?1",
            [blob.as_bytes().as_slice()],
        )?,
        1
    );

    database
        .connection
        .execute_batch("PRAGMA foreign_keys = ON")?;

    let refuses = |database: &mut MetadataDatabase| -> TestResult {
        let changes = database.connection.total_changes();

        let read_error = database
            .select_object(&Authority::Native, &read(&request))
            .unwrap_err();

        assert_eq!(read_error.failure(), OperationFailure::StoreCorrupt);
        assert_eq!(read_error.outcome(), None);

        let delete_error = database
            .delete_object(&Authority::Native, &read(&request), None)
            .unwrap_err();

        assert_eq!(delete_error.failure(), OperationFailure::StoreCorrupt);
        assert_eq!(delete_error.outcome(), Some(MutationOutcome::NotCommitted));
        assert_eq!(database.connection.total_changes(), changes);
        assert_eq!(mapping(database)?, expected_mapping);

        let blobs: i64 =
            database
                .connection
                .query_row("SELECT count(*) FROM blobs", [], |row| row.get(0))?;

        assert_eq!(blobs, 0);

        assert!(
            database
                .connection
                .prepare("PRAGMA foreign_key_check")?
                .exists([])?
        );

        Ok(())
    };

    refuses(&mut database)?;
    close(database)?;

    let mut reopened = MetadataDatabase::open(
        &directory.path().join("metadata.sqlite3"),
        MetadataOpen::Existing,
    )?;

    refuses(&mut reopened)?;
    close(reopened)?;
    directory.finish()?;
    Ok(())
}
