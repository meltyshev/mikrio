//! Metadata transaction/clock fixtures; guarded payload behavior has separate connection tests.

use super::*;
use crate::access::BucketAccessRequest;
use crate::lifecycle::{LifecycleConfiguration, LifecycleFilter, LifecycleRule};
use crate::metadata::lifecycle::expiration::{Candidate, ExpirationPage};
use crate::object::Authority;

mod histories;

type TestResult = Result<(), Box<dyn std::error::Error>>;
const DUE: i64 = 172_800_000;

fn clock(value: i64) -> EpochMillis {
    EpochMillis::new(value).unwrap()
}

fn rules(prefix: &str, days: u32, enabled: bool) -> LifecycleConfiguration {
    LifecycleConfiguration::new(vec![LifecycleRule {
        id: Some("rule".into()),
        is_enabled: enabled,
        filter: LifecycleFilter::Prefix(prefix.into()),
        expiration_days: Some(days),
        abort_incomplete_multipart_days: None,
    }])
    .unwrap()
}

fn bucket(request: &PutRequest) -> BucketAccessRequest {
    BucketAccessRequest {
        bucket: request.bucket.clone(),
        expected_owner: None,
    }
}

fn publish(
    database: &mut MetadataDatabase,
    request: &PutRequest,
    anchor: i64,
) -> Result<BlobId, Box<dyn std::error::Error>> {
    let blob = BlobId::from_bytes(*StoreId::generate()?.as_bytes());

    let bucket = database
        .reserve_put(&Authority::Native, request, blob)?
        .unwrap();

    database.publish_put(
        &Authority::Native,
        request,
        bucket,
        blob,
        &facts(request, b"unchanged bytes")?,
    )?;

    database.connection.execute(
        "UPDATE objects SET lifecycle_anchor_at_ms = ?3 WHERE bucket_id = ?1 AND key = ?2",
        rusqlite::params![bucket.as_slice(), request.key.as_bytes(), anchor],
    )?;

    Ok(blob)
}

fn candidate(
    database: &mut MetadataDatabase,
    now: i64,
) -> Result<Candidate, Box<dyn std::error::Error>> {
    let page = database
        .object_expiration_page(clock(now))?
        .ok_or("no configured expiration")?;

    let mut selected = page
        .candidates
        .into_iter()
        .filter_map(|(_, result)| result.transpose());

    let selected = selected.next().ok_or("no eligible object")??;
    Ok(selected)
}

fn visible(database: &mut MetadataDatabase, request: &PutRequest) -> bool {
    match database.select_object(&Authority::Native, &read(request)) {
        Ok(_) => true,
        Err(error) => {
            assert_eq!(error.failure(), OperationFailure::NoSuchKey);
            false
        }
    }
}

#[test]
fn expiration_selection_does_not_wait_for_a_peer_writer() -> TestResult {
    let directory = TestDirectory::new()?.retain_on_failure();
    let (mut database, request) = setup(&directory)?;
    publish(&mut database, &request, 0)?;
    database.put_bucket_lifecycle(&Authority::Native, &bucket(&request), &rules("", 1, true))?;

    let peer = MetadataDatabase::open(
        &directory.path().join("metadata.sqlite3"),
        MetadataOpen::Existing,
    )?;

    database
        .connection
        .busy_timeout(std::time::Duration::ZERO)?;

    peer.connection.execute_batch("BEGIN IMMEDIATE")?;
    let before = database.connection.total_changes();
    let selected = candidate(&mut database, DUE)?;
    assert_eq!(database.connection.total_changes(), before);

    assert_eq!(
        database
            .expire_object(&selected, clock(DUE))
            .unwrap_err()
            .failure(),
        OperationFailure::StoreBusy
    );

    assert!(visible(&mut database, &request));
    peer.connection.execute_batch("ROLLBACK")?;
    database.expire_object(&selected, clock(DUE))?;
    assert!(!visible(&mut database, &request));
    close(peer)?;
    close(database)?;
    directory.finish()?;
    Ok(())
}

#[test]
fn absent_disabled_abort_only_and_nonmatching_rules_leave_objects_live() -> TestResult {
    let directory = TestDirectory::new()?.retain_on_failure();
    let (mut database, request) = setup(&directory)?;
    publish(&mut database, &request, 0)?;

    assert!(
        database
            .object_expiration_page(clock(253_402_300_799_999))?
            .is_none()
    );

    let mut abort_only = rules("", 1, true).into_rules();
    abort_only[0].expiration_days = None;
    abort_only[0].abort_incomplete_multipart_days = Some(1);

    for configuration in [
        rules("", 1, false),
        LifecycleConfiguration::new(abort_only)?,
    ] {
        database.put_bucket_lifecycle(&Authority::Native, &bucket(&request), &configuration)?;
        assert!(database.object_expiration_page(clock(DUE))?.is_none());
        assert!(visible(&mut database, &request));
    }

    database.put_bucket_lifecycle(
        &Authority::Native,
        &bucket(&request),
        &rules("different/", 1, true),
    )?;

    let page = database.object_expiration_page(clock(DUE))?.unwrap();
    assert_eq!(page.candidates.len(), 1);

    assert!(
        page.candidates
            .into_iter()
            .all(|(_, result)| result.unwrap().is_none())
    );

    assert!(visible(&mut database, &request));
    close(database)?;
    directory.finish()?;
    Ok(())
}

#[test]
fn current_clock_and_earliest_matching_rule_control_a_single_logical_deletion() -> TestResult {
    let directory = TestDirectory::new()?.retain_on_failure();
    let (mut database, request) = setup(&directory)?;
    let blob = publish(&mut database, &request, 0)?;
    let mut configured = Vec::new();

    for (index, (prefix, days, enabled)) in [
        ("", 8, true),
        ("literal/", 1, true),
        ("literal/key", 2, false),
    ]
    .into_iter()
    .enumerate()
    {
        let mut rule = rules(prefix, days, enabled).into_rules().remove(0);
        rule.id = Some(index.to_string());
        configured.push(rule);
    }

    database.put_bucket_lifecycle(
        &Authority::Native,
        &bucket(&request),
        &LifecycleConfiguration::new(configured)?,
    )?;

    let before = database.object_expiration_page(clock(DUE - 1))?.unwrap();

    assert!(
        before
            .candidates
            .into_iter()
            .all(|(_, result)| result.unwrap().is_none())
    );

    let selected = candidate(&mut database, DUE)?;
    // A backward jump after selection delays the action; a later forward jump permits it.
    database.expire_object(&selected, clock(DUE - 1))?;
    assert!(visible(&mut database, &request));
    database.expire_object(&selected, clock(DUE))?;
    database.expire_object(&selected, clock(DUE + 1))?;
    assert!(!visible(&mut database, &request));

    let pending: i64 = database.connection.query_row(
        "SELECT gc_pending FROM blobs WHERE blob_id = ?1",
        [blob.as_bytes().as_slice()],
        |row| row.get(0),
    )?;

    assert_eq!(pending, 1);
    database.delete_bucket_lifecycle(&Authority::Native, &bucket(&request))?;
    assert!(!visible(&mut database, &request));
    close(database)?;
    directory.finish()?;
    Ok(())
}

#[test]
fn replacing_disabling_or_deleting_policy_invalidates_selected_candidates() -> TestResult {
    let directory = TestDirectory::new()?.retain_on_failure();
    let (mut database, request) = setup(&directory)?;
    publish(&mut database, &request, 0)?;

    for replacement in [
        Some(rules("", 1, true)),
        Some(rules("", 1, false)),
        Some(rules("", 99, true)),
        Some(rules("elsewhere", 1, true)),
        None,
    ] {
        database.put_bucket_lifecycle(
            &Authority::Native,
            &bucket(&request),
            &rules("", 1, true),
        )?;

        let selected = candidate(&mut database, DUE)?;

        if let Some(replacement) = replacement {
            database.put_bucket_lifecycle(&Authority::Native, &bucket(&request), &replacement)?;
        } else {
            database.delete_bucket_lifecycle(&Authority::Native, &bucket(&request))?;
        }

        database.expire_object(&selected, clock(DUE))?;
        assert!(visible(&mut database, &request));
    }

    close(database)?;
    directory.finish()?;
    Ok(())
}

#[test]
fn earlier_same_etag_replacement_survives_stale_expiration_then_expires_when_reselected()
-> TestResult {
    let directory = TestDirectory::new()?.retain_on_failure();
    let (mut database, request) = setup(&directory)?;
    let original_blob = publish(&mut database, &request, 1000)?;

    database.connection.execute(
        "UPDATE objects SET last_modified_at_ms = 1000 WHERE blob_id = ?1",
        [original_blob.as_bytes().as_slice()],
    )?;

    database.put_bucket_lifecycle(&Authority::Native, &bucket(&request), &rules("", 1, true))?;
    let stale = candidate(&mut database, DUE)?;
    let original = database.select_object(&Authority::Native, &read(&request))?;
    let replacement_blob = publish(&mut database, &request, 0)?;

    database.connection.execute(
        "UPDATE objects SET last_modified_at_ms = 0 WHERE blob_id = ?1",
        [replacement_blob.as_bytes().as_slice()],
    )?;

    let replacement = database.select_object(&Authority::Native, &read(&request))?;
    assert_eq!(replacement.etag, original.etag);
    assert_ne!(replacement.blob_id, original.blob_id);
    assert_ne!(replacement.generation, original.generation);
    assert!(replacement.last_modified < original.last_modified);

    // Both generations are due. Only the identity fence can reject this stale
    // candidate; recomputing age or comparing the unchanged ETag is insufficient.
    for _ in 0..2 {
        database.expire_object(&stale, clock(DUE))?;
        let current = database.select_object(&Authority::Native, &read(&request))?;
        assert_eq!(current.blob_id, replacement_blob);
        assert_eq!(current.generation, replacement.generation);
    }

    close(database)?;

    let mut database = MetadataDatabase::open(
        &directory.path().join("metadata.sqlite3"),
        MetadataOpen::Existing,
    )?;

    database.expire_object(&stale, clock(DUE))?;

    assert_eq!(
        database
            .select_object(&Authority::Native, &read(&request))?
            .blob_id,
        replacement_blob
    );

    let current = candidate(&mut database, DUE)?;
    database.expire_object(&current, clock(DUE))?;
    assert!(!visible(&mut database, &request));
    close(database)?;
    directory.finish()?;
    Ok(())
}

#[test]
fn identical_byte_replacement_retagging_and_bucket_recreation_fence_old_candidates() -> TestResult {
    let directory = TestDirectory::new()?.retain_on_failure();
    let (mut database, request) = setup(&directory)?;
    publish(&mut database, &request, 0)?;
    database.put_bucket_lifecycle(&Authority::Native, &bucket(&request), &rules("", 1, true))?;
    let selected = candidate(&mut database, DUE)?;
    publish(&mut database, &request, 0)?;
    database.expire_object(&selected, clock(DUE))?;
    assert!(visible(&mut database, &request));
    let selected = candidate(&mut database, DUE)?;

    database.replace_object_tags(
        &read(&request),
        &crate::object::ObjectTags::new([("state".into(), "attached".into())])?,
    )?;

    database.expire_object(&selected, clock(DUE))?;
    assert!(visible(&mut database, &request));
    let selected = candidate(&mut database, DUE)?;
    database.delete_object(&Authority::Native, &read(&request), None)?;
    database.delete_bucket(&request.bucket, None)?;
    database.create_bucket(&request.bucket)?;
    publish(&mut database, &request, 0)?;
    database.put_bucket_lifecycle(&Authority::Native, &bucket(&request), &rules("", 1, true))?;
    database.expire_object(&selected, clock(DUE))?;
    assert!(visible(&mut database, &request));
    close(database)?;
    directory.finish()?;
    Ok(())
}

#[test]
fn invalid_eligibility_or_corrupt_blob_preserves_mapping_and_retirement_hint() -> TestResult {
    let directory = TestDirectory::new()?.retain_on_failure();
    let (mut database, request) = setup(&directory)?;
    let blob = publish(&mut database, &request, 0)?;
    let mut configured = rules("", 2_147_483_647, true).into_rules();
    let mut valid = rules("literal/", 1, true).into_rules().remove(0);
    valid.id = Some("valid".into());
    configured.push(valid);

    database.put_bucket_lifecycle(
        &Authority::Native,
        &bucket(&request),
        &LifecycleConfiguration::new(configured)?,
    )?;

    let page = database.object_expiration_page(clock(DUE))?.unwrap();
    let error = page.candidates.into_iter().next().unwrap().1.unwrap_err();
    assert_eq!(error.failure(), OperationFailure::InvalidArgument);
    assert!(visible(&mut database, &request));
    database.put_bucket_lifecycle(&Authority::Native, &bucket(&request), &rules("", 1, true))?;
    let selected = candidate(&mut database, DUE)?;

    database.connection.execute(
        "UPDATE blobs SET checksums_json = 'invalid' WHERE blob_id = ?1",
        [blob.as_bytes().as_slice()],
    )?;

    let error = database.expire_object(&selected, clock(DUE)).unwrap_err();
    assert_eq!(error.failure(), OperationFailure::StoreCorrupt);
    assert_eq!(error.outcome(), Some(MutationOutcome::NotCommitted));

    let facts: (i64, i64) = database.connection.query_row(
        "SELECT (SELECT count(*) FROM objects), gc_pending FROM blobs WHERE blob_id = ?1",
        [blob.as_bytes().as_slice()],
        |row| Ok((row.get(0)?, row.get(1)?)),
    )?;

    assert_eq!(facts, (1, 0));
    close(database)?;
    directory.finish()?;
    Ok(())
}

fn ready_candidates(page: &ExpirationPage) -> usize {
    page.candidates
        .iter()
        .filter(|(_, result)| matches!(result, Ok(Some(_))))
        .count()
}

#[test]
fn expiration_rollback_and_unknown_commit_keep_mapping_and_pending_hint_atomic() -> TestResult {
    use super::super::super::transaction::Fault;

    let directory = TestDirectory::new()?.retain_on_failure();
    let (mut database, request) = setup(&directory)?;
    let blob = publish(&mut database, &request, 0)?;
    database.put_bucket_lifecycle(&Authority::Native, &bucket(&request), &rules("", 1, true))?;
    let selected = candidate(&mut database, DUE)?;

    let mut peer = MetadataDatabase::open(
        &directory.path().join("metadata.sqlite3"),
        MetadataOpen::Existing,
    )?;

    for (fault, outcome, committed) in [
        (Fault::BeforeCommit, MutationOutcome::NotCommitted, false),
        (Fault::AfterCommit, MutationOutcome::Unknown, true),
    ] {
        database.transaction_fault = Some(fault);
        let error = database.expire_object(&selected, clock(DUE)).unwrap_err();
        assert_eq!(error.outcome(), Some(outcome));
        assert_eq!(visible(&mut peer, &request), !committed);

        let pending: bool = peer.connection.query_row(
            "SELECT gc_pending FROM blobs WHERE blob_id = ?1",
            [blob.as_bytes().as_slice()],
            |row| row.get(0),
        )?;

        assert_eq!(pending, committed);
    }

    close(peer)?;
    close(database)?;

    let mut reopened = MetadataDatabase::open(
        &directory.path().join("metadata.sqlite3"),
        MetadataOpen::Existing,
    )?;

    assert!(!visible(&mut reopened, &request));
    close(reopened)?;
    directory.finish()?;
    Ok(())
}

fn object_request(bucket: &BucketName, key: &str) -> PutRequest {
    PutRequest {
        bucket: bucket.clone(),
        key: ObjectKey::parse(key).unwrap(),
        expected_owner: None,
        content_length: None,
        representation: Representation::default(),
        metadata: UserMetadata::default(),
        tags: None,
        integrity: PutIntegrity::new(PutIntegrityInput::default()).unwrap(),
        conditions: WriteConditions::default(),
    }
}

#[test]
fn a_corrupt_bucket_configuration_does_not_suppress_other_buckets_in_the_raw_page() -> TestResult {
    let directory = TestDirectory::new()?.retain_on_failure();
    let (mut database, first) = setup(&directory)?;
    let second_bucket = BucketName::parse("second-bucket")?;
    database.create_bucket(&second_bucket)?;

    for name in [&first.bucket, &second_bucket] {
        let request = object_request(name, "first");

        database.put_bucket_lifecycle(
            &Authority::Native,
            &bucket(&request),
            &rules("", 1, true),
        )?;

        publish(&mut database, &request, 0)?;
        publish(&mut database, &object_request(name, "second"), 0)?;
    }

    database.connection.execute(
        "UPDATE bucket_lifecycle_rules SET filter_prefix = x'ff' WHERE bucket_id IN
         (SELECT bucket_id FROM buckets WHERE name = 'attachments')",
        [],
    )?;

    let page = database.object_expiration_page(clock(DUE))?.unwrap();
    assert_eq!(page.candidates.len(), 4);
    assert_eq!(ready_candidates(&page), 2);

    assert_eq!(
        page.candidates
            .iter()
            .filter(|(_, result)| result.is_err())
            .count(),
        1
    );

    for (_, result) in &page.candidates {
        if let Ok(Some(candidate)) = result {
            database.expire_object(candidate, clock(DUE))?;
        }
    }

    database.advance_sweep(page.progress)?;

    assert!(visible(
        &mut database,
        &object_request(&first.bucket, "first")
    ));

    assert!(visible(
        &mut database,
        &object_request(&first.bucket, "second")
    ));

    assert!(!visible(
        &mut database,
        &object_request(&second_bucket, "first")
    ));

    assert!(!visible(
        &mut database,
        &object_request(&second_bucket, "second")
    ));

    close(database)?;
    directory.finish()?;
    Ok(())
}

#[test]
fn raw_pages_finish_the_captured_bound_and_revisit_replacements_and_new_rows() -> TestResult {
    let directory = TestDirectory::new()?.retain_on_failure();
    let (mut database, request) = setup(&directory)?;
    database.put_bucket_lifecycle(&Authority::Native, &bucket(&request), &rules("", 1, true))?;
    let empty = database.object_expiration_page(clock(DUE))?.unwrap();
    assert_eq!(empty.candidates.len(), 0);
    assert_eq!(empty.progress.upper, 0);
    database.advance_sweep(empty.progress)?;
    let mut requests = Vec::new();

    for index in 0..32 {
        let item = object_request(&request.bucket, &format!("key/{index:03}"));
        publish(&mut database, &item, 0)?;
        requests.push(item);
    }

    let first = database.object_expiration_page(clock(DUE))?.unwrap();
    assert_eq!(first.candidates.len(), 16);
    assert_eq!(ready_candidates(&first), 16);
    let upper = first.progress.upper;
    let stale = first.progress;
    publish(&mut database, &requests[0], 0)?;
    let inserted = object_request(&request.bucket, "aaa/before-existing-keys");
    publish(&mut database, &inserted, 0)?;
    database.advance_sweep(first.progress)?;
    let second = database.object_expiration_page(clock(DUE))?.unwrap();
    assert_eq!(second.candidates.len(), 16);
    assert_eq!(second.progress.upper, upper);
    database.advance_sweep(second.progress)?;
    let restarted = database.object_expiration_page(clock(DUE))?.unwrap();
    assert!(restarted.progress.upper > upper);
    assert_eq!(restarted.progress.cursor, 0);
    database.advance_sweep(restarted.progress)?;
    database.advance_sweep(stale)?;
    let resumed = database.object_expiration_page(clock(DUE))?.unwrap();
    assert_eq!(resumed.progress.cursor, restarted.progress.examined);
    assert_eq!(resumed.candidates.len(), 16);
    database.advance_sweep(resumed.progress)?;
    let final_page = database.object_expiration_page(clock(DUE))?.unwrap();
    assert_eq!(final_page.candidates.len(), 1);
    assert_eq!(ready_candidates(&final_page), 1);
    // Remove the selected upper row; the following empty tail still finishes.
    database.delete_object(&Authority::Native, &read(&inserted), None)?;
    let empty_tail = database.object_expiration_page(clock(DUE))?.unwrap();
    assert_eq!(empty_tail.candidates.len(), 0);
    assert_eq!(empty_tail.progress.examined, empty_tail.progress.upper);
    database.advance_sweep(empty_tail.progress)?;
    close(database)?;
    directory.finish()?;
    Ok(())
}
