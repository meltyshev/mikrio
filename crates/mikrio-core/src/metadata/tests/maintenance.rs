use super::*;
use crate::{BlobId, UploadId};
use md5::Digest as _;
use rusqlite::params;
use std::fmt::Write as _;
use std::time::Duration;

type TestResult = Result<(), Box<dyn std::error::Error>>;

fn setup(directory: &TestDirectory) -> Result<MetadataDatabase, Box<dyn std::error::Error>> {
    let mut database = database(directory)?;

    database.initialize(&BootstrapValues::generate(
        StoreId::generate()?,
        "us-east-1".into(),
        EpochMillis::new(0)?,
    )?)?;

    Ok(database)
}

fn reserve(database: &MetadataDatabase, suffix: u8) -> Result<BlobId, rusqlite::Error> {
    let mut bytes = [7; 16];
    bytes[15] = suffix;
    let blob = BlobId::from_bytes(bytes);

    database.connection.execute(
        "INSERT INTO blobs (blob_id, shard, state, write_kind, target_bucket_id,
         target_key, created_at_ms, gc_pending, gc_attempt_count)
         VALUES (?1, 7, 'reserved', 'put_object', zeroblob(16), x'61', 0, 1, 0)",
        [blob.as_bytes().as_slice()],
    )?;

    Ok(blob)
}

fn gc_candidates(page: &crate::metadata::gc::GcPage) -> Vec<(i64, BlobId)> {
    page.candidates
        .iter()
        .map(|(row_id, candidate)| (*row_id, *candidate.as_ref().unwrap()))
        .collect()
}

#[test]
fn sweep_pages_read_through_a_peer_writer_and_retry_failed_advancement() -> TestResult {
    let directory = TestDirectory::new()?;
    let mut database = setup(&directory)?;
    reserve(&database, 1)?;
    receipt(&database, 1, false)?;

    let peer = MetadataDatabase::open(
        &directory.path().join("metadata.sqlite3"),
        MetadataOpen::Existing,
    )?;

    // A held writer, rather than a latency threshold, detects accidental write
    // admission in page selection. The test-only zero timeout bounds failure.
    database.connection.busy_timeout(Duration::ZERO)?;
    peer.connection.execute_batch("BEGIN IMMEDIATE")?;
    let before = database.connection.total_changes();
    let blobs = database.gc_page(7)?.unwrap();
    let receipts = database.receipt_page()?.unwrap();
    assert_eq!(blobs.candidates.len(), 1);
    assert_eq!(receipts.candidates.len(), 1);
    assert_eq!(database.connection.total_changes(), before);

    assert_eq!(
        database
            .advance_sweep(blobs.progress)
            .unwrap_err()
            .failure(),
        crate::OperationFailure::StoreBusy
    );

    assert_eq!(
        gc_candidates(&database.gc_page(7)?.unwrap()),
        gc_candidates(&blobs)
    );

    peer.connection.execute_batch("ROLLBACK")?;
    database.advance_sweep(blobs.progress)?;
    database.advance_sweep(receipts.progress)?;
    assert_eq!(database.connection.total_changes(), before + 2);
    close(peer)?;
    close(database)?;
    directory.finish()?;
    Ok(())
}

#[test]
fn competing_sweep_restarts_cannot_extend_or_erase_committed_progress() -> TestResult {
    let directory = TestDirectory::new()?;
    let mut database = setup(&directory)?;

    for suffix in 1..=17 {
        reserve(&database, suffix)?;
    }

    let interrupted = database.gc_page(7)?.unwrap();
    close(database)?;

    let mut database = MetadataDatabase::open(
        &directory.path().join("metadata.sqlite3"),
        MetadataOpen::Existing,
    )?;

    let first = database.gc_page(7)?.unwrap();
    assert_eq!(gc_candidates(&first), gc_candidates(&interrupted));

    let mut peer = MetadataDatabase::open(
        &directory.path().join("metadata.sqlite3"),
        MetadataOpen::Existing,
    )?;

    reserve(&database, 18)?;
    let competing = peer.gc_page(7)?.unwrap();
    assert_eq!(first.progress.upper, 17);
    assert_eq!(competing.progress.upper, 18);
    database.advance_sweep(first.progress)?;
    peer.advance_sweep(competing.progress)?;
    let tail = peer.gc_page(7)?.unwrap();
    assert_eq!(tail.progress.cursor, 16);
    assert_eq!(tail.progress.upper, 17);
    assert_eq!(tail.candidates.len(), 1);
    assert_eq!(tail.candidates[0].0, 17);
    peer.advance_sweep(tail.progress)?;
    let restarted = database.gc_page(7)?.unwrap();
    assert_eq!(restarted.progress.cursor, 0);
    assert_eq!(restarted.progress.upper, 18);
    let mut partial = restarted.progress;
    partial.examined = 3;
    database.advance_sweep(partial)?;
    // Neither a different reset token nor an earlier page may move the cursor
    // after the winning restart, even when its selected bounds happen to match.
    peer.advance_sweep(competing.progress)?;
    peer.advance_sweep(first.progress)?;
    let resumed = peer.gc_page(7)?.unwrap();
    assert_eq!(resumed.candidates.first().unwrap().0, 4);
    assert_eq!(resumed.candidates.last().unwrap().0, 18);
    close(peer)?;
    close(database)?;
    directory.finish()?;
    Ok(())
}

#[test]
fn bounded_gc_pages_preserve_high_water_and_reject_stale_progress() -> TestResult {
    let directory = TestDirectory::new()?;
    let mut database = setup(&directory)?;

    for suffix in 1..=17 {
        reserve(&database, suffix)?;
    }

    let first = database.gc_page(7)?.unwrap();
    assert_eq!(first.candidates.len(), 16);
    assert_eq!(first.progress.examined, 16);
    database.advance_sweep(first.progress)?;

    database
        .connection
        .execute("DELETE FROM blobs WHERE row_id = 17", [])?;

    let last = database.gc_page(7)?.unwrap();
    assert!(last.candidates.is_empty());
    assert_eq!(last.progress.examined, 17);
    database.advance_sweep(last.progress)?;
    reserve(&database, 18)?;
    let next = database.gc_page(7)?.unwrap();
    assert_eq!(next.progress.cursor, 0);
    database.advance_sweep(first.progress)?;

    let cursor: i64 = database.connection.query_row(
        "SELECT cursor_row_id FROM maintenance_sweeps WHERE task = 'gc_blobs' AND shard = 7",
        [],
        |row| row.get(0),
    )?;

    assert_eq!(cursor, 17, "selection must not publish the proposed reset");
    // A budget yield reports only the rows actually attempted.
    let mut partial = next.progress;
    partial.examined = 3;
    database.advance_sweep(partial)?;
    let resumed = database.gc_page(7)?.unwrap();
    assert_eq!(resumed.candidates.first().unwrap().0, 4);
    assert_eq!(resumed.candidates.last().unwrap().0, 18);
    close(database)?;
    directory.finish()?;
    Ok(())
}

#[test]
fn diagnostic_pages_are_bounded_and_rotate_without_mutating_sweep_authority() -> TestResult {
    let directory = TestDirectory::new()?;
    let mut database = setup(&directory)?;

    let blobs = (1..=17)
        .map(|suffix| reserve(&database, suffix))
        .collect::<Result<Vec<_>, _>>()?;

    let before = database.connection.total_changes();
    let first = database.cleanup_sample(None)?;
    assert_eq!(first.blobs, blobs[..16]);
    assert_eq!(first.oldest_created_at_ms, Some(0));
    assert!(first.more_pending);
    let after = first.after.ok_or("missing bounded sample cursor")?;
    let last = database.cleanup_sample(Some(after))?;
    assert_eq!(last.blobs, blobs[16..]);
    assert!(!last.more_pending);
    assert!(last.after.is_none());
    assert_eq!(database.connection.total_changes(), before);

    // Deleting all rows after a captured position must reset the local hint,
    // allowing pending rows behind it to be sampled on the next pass.
    database
        .connection
        .execute("DELETE FROM blobs WHERE row_id = 17", [])?;

    let empty = database.cleanup_sample(Some(after))?;
    assert_eq!(empty.blobs, Vec::<BlobId>::new());
    assert_eq!(empty.oldest_created_at_ms, None);
    assert!(empty.after.is_none());
    assert_eq!(database.cleanup_sample(empty.after)?.blobs, blobs[..16]);
    close(database)?;
    directory.finish()?;
    Ok(())
}

fn receipt(
    database: &MetadataDatabase,
    suffix: u8,
    claimed: bool,
) -> Result<(), Box<dyn std::error::Error>> {
    let upload = UploadId::new(EpochMillis::new(0)?, [suffix; 16]).to_string();
    let claim = claimed.then_some([suffix; 16]);
    let digest = claimed.then_some([suffix; 32]);

    database.connection.execute(
        "INSERT INTO multipart_receipts (upload_id, bucket_id, key, outcome,
        completion_claim_id, completion_blob_id, completion_parts_digest, finished_at_ms)
        VALUES (?1, zeroblob(16), x'61', 'aborted', ?2, ?2, ?3, 0)",
        params![
            upload,
            claim.as_ref().map(<[u8; 16]>::as_slice),
            digest.as_ref().map(<[u8; 32]>::as_slice)
        ],
    )?;

    Ok(())
}

#[test]
fn receipt_pruning_has_no_ttl_and_rechecks_the_selected_terminal_identity() -> TestResult {
    let directory = TestDirectory::new()?;
    let mut database = setup(&directory)?;
    receipt(&database, 1, false)?;
    receipt(&database, 2, true)?;
    let page = database.receipt_page()?.unwrap();
    assert_eq!(page.candidates.len(), 2);
    let plain = page.candidates[0].1.as_ref().unwrap();
    let claimed = page.candidates[1].1.as_ref().unwrap();
    assert!(plain.output.is_none());
    assert!(claimed.output.is_some());
    database.prune_receipt(plain)?;

    database.connection.execute(
        "UPDATE multipart_receipts SET completion_claim_id = zeroblob(16)",
        [],
    )?;

    assert_eq!(
        database.prune_receipt(claimed).unwrap_err().failure(),
        crate::OperationFailure::StoreCorrupt
    );

    database.connection.execute(
        "UPDATE multipart_receipts SET completion_claim_id = ?1",
        [[2u8; 16].as_slice()],
    )?;

    // SQL-only qualification: the guarded scheduler test owns exclusion before
    // invoking this method against an actual context.
    database.prune_receipt(claimed)?;
    database.advance_sweep(page.progress)?;
    assert!(database.receipt_page()?.is_none());

    database.connection.execute(
        "INSERT INTO multipart_receipts (upload_id, bucket_id, key, outcome, finished_at_ms)
        VALUES (?1, zeroblob(16), x'63', 'aborted', 0)",
        ["x".repeat(51)],
    )?;

    receipt(&database, 3, false)?;
    let page = database.receipt_page()?.unwrap();
    assert!(page.candidates[0].1.is_err());
    database.prune_receipt(page.candidates[1].1.as_ref().unwrap())?;
    database.advance_sweep(page.progress)?;

    assert_eq!(
        database
            .connection
            .query_row("SELECT count(*) FROM multipart_receipts", [], |row| {
                row.get::<_, i64>(0)
            })?,
        1
    );

    close(database)?;
    directory.finish()?;
    Ok(())
}

#[test]
fn passive_checkpoint_defers_held_snapshot_and_preserves_wal_recovery_state() -> TestResult {
    let directory = TestDirectory::new()?;
    let database = setup(&directory)?;
    database.checkpoint()?;

    let peer = MetadataDatabase::open(
        &directory.path().join("metadata.sqlite3"),
        MetadataOpen::Existing,
    )?;

    peer.connection.execute_batch("BEGIN")?;

    let _: i64 = peer
        .connection
        .query_row("SELECT count(*) FROM blobs", [], |row| row.get(0))?;

    reserve(&database, 1)?;
    database.checkpoint()?;
    assert!(directory.path().join("metadata.sqlite3-wal").exists());

    assert_eq!(
        peer.connection
            .query_row("SELECT count(*) FROM blobs", [], |row| row.get::<_, i64>(0))?,
        0
    );

    peer.connection.execute_batch("ROLLBACK")?;
    database.checkpoint()?;

    assert_eq!(
        peer.connection
            .query_row("SELECT count(*) FROM blobs", [], |row| row.get::<_, i64>(0))?,
        1
    );

    close(peer)?;
    close(database)?;
    directory.finish()?;
    Ok(())
}

#[test]
fn retired_claims_and_parts_remain_ledger_jobs_until_bounded_finalization() -> TestResult {
    let directory = TestDirectory::new()?;
    let mut database = setup(&directory)?;
    let upload = UploadId::new(EpochMillis::new(0)?, [1; 16]).to_string();
    let output = BlobId::from_bytes([9; 16]);
    let part = BlobId::from_bytes([8; 16]);

    database.connection.execute_batch(
        "INSERT INTO buckets (bucket_id, name, created_at_ms) VALUES (zeroblob(16), NULL, 0);",
    )?;

    database.connection.execute(
        "INSERT INTO blobs (blob_id, shard, state, write_kind, target_bucket_id,
         target_key, target_upload_id, created_at_ms, gc_pending, gc_attempt_count)
         VALUES (?1, 9, 'reserved', 'complete_multipart_upload', zeroblob(16), x'61', ?2, 0, 1, 0)",
        params![output.as_bytes().as_slice(), upload],
    )?;

    database.connection.execute(
        "INSERT INTO blobs (blob_id, shard, state, write_kind, byte_length, etag,
         md5_digest, checksums_json, created_at_ms, gc_pending, gc_attempt_count)
         VALUES (?1, 8, 'ready', 'upload_part', 0, '\"d41d8cd98f00b204e9800998ecf8427e\"',
         x'd41d8cd98f00b204e9800998ecf8427e', '{}', 0, 0, 0)",
        [part.as_bytes().as_slice()],
    )?;

    database.connection.execute(
        "INSERT INTO multipart_uploads (upload_id, bucket_id, key, initiator_id, state,
         completion_claim_id, completion_blob_id, completion_parts_digest, user_metadata_json,
         tags_json, initiated_at_ms)
         VALUES (?1, zeroblob(16), x'61', 'historical-owner', 'completing', zeroblob(16),
         ?2, zeroblob(32), '{}', '{}', 0)",
        params![upload, output.as_bytes().as_slice()],
    )?;

    database.connection.execute(
        "INSERT INTO multipart_parts (upload_id, part_number, generation, blob_id, last_modified_at_ms)
         VALUES (?1, 1, zeroblob(16), ?2, 0)",
        params![upload, part.as_bytes().as_slice()],
    )?;

    assert!(!database.gc_mark(part)?, "acknowledged part is still live");
    database.retire_namespace([0; 16])?;

    assert_eq!(
        database
            .connection
            .query_row("SELECT count(*) FROM buckets", [], |row| {
                row.get::<_, i64>(0)
            })?,
        0
    );

    assert_eq!(
        database.connection.query_row(
            "SELECT completion_blob_id FROM multipart_receipts",
            [],
            |row| row.get::<_, Vec<u8>>(0)
        )?,
        output.as_bytes()
    );

    assert!(database.gc_mark(part)?);
    assert!(database.gc_mark(output)?);
    database.gc_finish(output)?;
    database.gc_finish(part)?;
    // A separate, previously published then retired completed output owns these
    // immutable facts. An abandoned reservation must never have part facts.
    let completed = BlobId::from_bytes([10; 16]);
    let digest = md5::Md5::digest([0; 257 * 16]);
    let mut etag = String::from("\"");

    for byte in &digest {
        write!(etag, "{byte:02x}")?;
    }

    etag.push_str("-257\"");

    database.connection.execute(
        "INSERT INTO blobs (blob_id, shard, state, write_kind, byte_length, part_count,
        etag, md5_digest, checksums_json, created_at_ms, gc_pending, gc_attempt_count)
        VALUES (?1, 10, 'ready', 'complete_multipart_upload', ?2, 257, ?3, ?4, '{}', 0, 1, 0)",
        params![
            completed.as_bytes().as_slice(),
            256_i64 * 5 * 1024 * 1024 + 1,
            etag,
            digest.as_slice()
        ],
    )?;

    for number in 1..=257 {
        database.connection.execute(
            "INSERT INTO blob_parts (blob_id, part_number, byte_offset, byte_length, checksums_json, part_index)
             VALUES (?1, ?2, ?3, ?4, '{}', ?2)",
            params![
                completed.as_bytes().as_slice(),
                number,
                (number - 1) * 5 * 1024 * 1024,
                if number == 257 { 1 } else { 5 * 1024 * 1024 }
            ],
        )?;
    }

    assert!(database.gc_mark(completed)?);
    database.gc_finish(completed)?;

    assert_eq!(
        database
            .connection
            .query_row("SELECT count(*) FROM blob_parts", [], |row| {
                row.get::<_, i64>(0)
            })?,
        1
    );

    database.gc_finish(completed)?;

    assert_eq!(
        database
            .connection
            .query_row("SELECT count(*) FROM blobs", [], |row| row.get::<_, i64>(0))?,
        0
    );

    close(database)?;
    directory.finish()?;
    Ok(())
}
