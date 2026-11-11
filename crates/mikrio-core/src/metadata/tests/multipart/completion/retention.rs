use super::super::retention::{candidate, configure};
use super::*;

#[test]
fn completion_and_retry_preserve_initiation_age_across_utc_days_and_reopen() -> TestResult {
    use crate::lifecycle::{LifecycleConfiguration, LifecycleFilter, LifecycleRule};

    // AWS metadata and lifecycle documentation: initiation is the completed
    // object's age basis; one day after 2020-02-28 23:59:59.999 rounds to March 1.
    // This is an injected initiation/maintenance clock fixture, not real aging.
    let initiated = EpochMillis::new(1_582_934_399_999)?;
    let due = EpochMillis::new(1_583_020_800_000)?;
    let directory = TestDirectory::new()?.retain_on_failure();
    let mut database = initialized(&directory)?;
    let (request, facts) = fixture_at(&mut database, initiated)?;
    let selected = database.select_completion(&Authority::Native, &request)?;
    let claim = claim(60);
    assert!(database.claim_completion(&Authority::Native, &request, &selected, claim)?);
    database.publish_completion(&Authority::Native, &request, &selected, claim, &facts)?;

    let finished: i64 = database.connection.query_row(
        "SELECT finished_at_ms FROM multipart_receipts WHERE upload_id = ?1",
        [request.upload.upload_id.to_string()],
        |row| row.get(0),
    )?;

    assert!(
        finished > due.get(),
        "completion must occur on a later UTC day"
    );

    assert_eq!(
        database
            .select_completion(&Authority::Native, &request)
            .unwrap_err()
            .failure(),
        OperationFailure::NoSuchUpload
    );

    database.put_bucket_lifecycle(
        &Authority::Native,
        &BucketAccessRequest {
            bucket: request.upload.bucket,
            expected_owner: None,
        },
        &LifecycleConfiguration::new(vec![LifecycleRule {
            id: Some("completed-age".into()),
            is_enabled: true,
            filter: LifecycleFilter::All,
            expiration_days: Some(1),
            abort_incomplete_multipart_days: None,
        }])?,
    )?;

    close(database)?;

    let mut database = MetadataDatabase::open(
        &directory.path().join("metadata.sqlite3"),
        MetadataOpen::Existing,
    )?;

    let timestamps: (i64, i64) = database.connection.query_row(
        "SELECT last_modified_at_ms, lifecycle_anchor_at_ms FROM objects",
        [],
        |row| Ok((row.get(0)?, row.get(1)?)),
    )?;

    assert_eq!(timestamps, (initiated.get(), initiated.get()));
    let before = EpochMillis::new(due.get() - 1)?;
    let page = database.object_expiration_page(before)?.unwrap();

    assert!(
        page.candidates
            .into_iter()
            .all(|(_, entry)| entry.unwrap().is_none())
    );

    let page = database.object_expiration_page(due)?.unwrap();

    let candidates = page
        .candidates
        .into_iter()
        .filter_map(|(_, entry)| entry.transpose())
        .collect::<Result<Vec<_>, _>>()?;

    assert_eq!(candidates.len(), 1);
    database.expire_object(&candidates[0], before)?;

    let objects = || {
        database
            .connection
            .query_row::<i64, _, _>("SELECT count(*) FROM objects", [], |row| row.get(0))
    };

    assert_eq!(objects()?, 1);
    database.expire_object(&candidates[0], due)?;

    assert_eq!(
        database
            .connection
            .query_row::<i64, _, _>("SELECT count(*) FROM objects", [], |row| row.get(0))?,
        0
    );

    close(database)?;
    directory.finish()?;
    Ok(())
}

#[test]
fn lifecycle_abort_and_completion_commit_orders_preserve_claim_receipts_and_fence_late_parts()
-> TestResult {
    for complete_first in [false, true] {
        let directory = TestDirectory::new()?.retain_on_failure();
        let mut database = initialized(&directory)?;
        let (request, facts) = fixture(&mut database)?;
        configure(&mut database, true, "")?;
        let now = EpochMillis::new(172_800_000)?;
        let expiration = candidate(&mut database, now)?.unwrap();
        let part = super::super::parts::part(request.upload.upload_id.clone());
        let (late, late_blob) = super::super::parts::reserve(&mut database, &part, 40)?;
        let selected = database.select_completion(&Authority::Native, &request)?;
        let claim = claim(60);
        assert!(database.claim_completion(&Authority::Native, &request, &selected, claim)?);

        if complete_first {
            database.publish_completion(&Authority::Native, &request, &selected, claim, &facts)?;
        }

        database.expire_upload(&expiration, now)?;

        if !complete_first {
            assert_eq!(
                database
                    .publish_completion(&Authority::Native, &request, &selected, claim, &facts)
                    .unwrap_err()
                    .failure(),
                OperationFailure::NoSuchUpload
            );
        }

        assert_eq!(
            database
                .publish_part(&Authority::Native, &part, late, late_blob, &facts)
                .unwrap_err()
                .failure(),
            OperationFailure::NoSuchUpload
        );

        let receipt: (String, Vec<u8>, Vec<u8>) = database.connection.query_row(
            "SELECT outcome, completion_claim_id, completion_parts_digest FROM multipart_receipts",
            [],
            |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
        )?;

        assert_eq!(
            receipt,
            (
                if complete_first {
                    "completed"
                } else {
                    "aborted"
                }
                .into(),
                vec![60; 16],
                selected.fingerprint.to_vec()
            )
        );

        assert_eq!(
            database
                .connection
                .query_row("SELECT count(*) FROM objects", [], |row| {
                    row.get::<_, i64>(0)
                })?,
            i64::from(complete_first)
        );

        assert_eq!(
            database
                .connection
                .query_row("SELECT count(*) FROM multipart_parts", [], |row| {
                    row.get::<_, i64>(0)
                })?,
            0
        );

        assert_eq!(
            database.connection.query_row(
                "SELECT gc_pending FROM blobs WHERE blob_id=?1",
                [claim.blob_id.as_bytes().as_slice()],
                |row| row.get::<_, bool>(0)
            )?,
            !complete_first
        );

        close(database)?;
        directory.finish()?;
    }

    Ok(())
}

#[test]
fn lifecycle_abort_rollback_and_lost_acknowledgment_reopen_with_one_terminal_state() -> TestResult {
    use crate::metadata::transaction::Fault;
    let directory = TestDirectory::new()?.retain_on_failure();
    let mut database = initialized(&directory)?;
    let (request, facts) = fixture(&mut database)?;
    configure(&mut database, true, "")?;
    let now = EpochMillis::new(172_800_000)?;
    let expiration = candidate(&mut database, now)?.unwrap();
    let selected = database.select_completion(&Authority::Native, &request)?;
    assert!(database.claim_completion(&Authority::Native, &request, &selected, claim(60))?);
    database.transaction_fault = Some(Fault::BeforeCommit);

    assert_eq!(
        database
            .expire_upload(&expiration, now)
            .unwrap_err()
            .outcome(),
        Some(MutationOutcome::NotCommitted)
    );

    assert_eq!(count(&database)?, 1);

    assert_eq!(
        database
            .connection
            .query_row("SELECT count(*) FROM multipart_parts", [], |row| {
                row.get::<_, i64>(0)
            })?,
        1
    );

    database.transaction_fault = Some(Fault::AfterCommit);

    assert_eq!(
        database
            .expire_upload(&expiration, now)
            .unwrap_err()
            .outcome(),
        Some(MutationOutcome::Unknown)
    );

    close(database)?;

    let mut reopened = MetadataDatabase::open(
        &directory.path().join("metadata.sqlite3"),
        MetadataOpen::Existing,
    )?;

    assert_eq!(count(&reopened)?, 0);
    reopened.expire_upload(&expiration, now)?;

    assert_eq!(
        reopened
            .publish_completion(&Authority::Native, &request, &selected, claim(60), &facts)
            .unwrap_err()
            .failure(),
        OperationFailure::NoSuchUpload
    );

    close(reopened)?;
    directory.finish()?;

    Ok(())
}
