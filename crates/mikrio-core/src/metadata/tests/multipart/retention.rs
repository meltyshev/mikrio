//! Local provisional abort timing and transaction-owned cleanup authority.

use super::*;
use crate::lifecycle::{LifecycleConfiguration, LifecycleFilter, LifecycleRule};
use crate::metadata::lifecycle::uploads::Candidate;
use crate::multipart::UploadRequest;

pub(super) fn configure(
    database: &mut MetadataDatabase,
    enabled: bool,
    prefix: &str,
) -> TestResult {
    database.put_bucket_lifecycle(
        &Authority::Native,
        &BucketAccessRequest {
            bucket: request().bucket,
            expected_owner: None,
        },
        &LifecycleConfiguration::new(vec![LifecycleRule {
            id: Some("abort".into()),
            is_enabled: enabled,
            filter: LifecycleFilter::Prefix(prefix.into()),
            expiration_days: None,
            abort_incomplete_multipart_days: Some(1),
        }])?,
    )?;

    Ok(())
}

pub(super) fn candidate(
    database: &mut MetadataDatabase,
    now: EpochMillis,
) -> Result<Option<Candidate>, Box<dyn std::error::Error>> {
    let Some(page) = database.upload_expiration_page(now)? else {
        return Ok(None);
    };

    for (_, entry) in page.candidates {
        if let Some(value) = entry? {
            return Ok(Some(value));
        }
    }

    Ok(None)
}

#[test]
fn lifecycle_upload_retirement_requires_current_rules_identity_and_original_age() -> TestResult {
    let directory = TestDirectory::new()?.retain_on_failure();
    let mut database = initialized(&directory)?;

    database.initiate_multipart(
        &Authority::Native,
        &request(),
        UploadId::new(EpochMillis::new(0)?, [1; 16]),
    )?;

    let due = EpochMillis::new(172_800_000)?;
    assert!(database.upload_expiration_page(due)?.is_none());
    configure(&mut database, false, "")?;
    assert!(database.upload_expiration_page(due)?.is_none());
    configure(&mut database, true, "elsewhere/")?;
    assert!(candidate(&mut database, due)?.is_none());
    configure(&mut database, true, "literal/")?;
    assert!(candidate(&mut database, EpochMillis::new(due.get() - 1)?)?.is_none());
    let selected = candidate(&mut database, due)?.unwrap();

    // A page cannot carry an old rule's destructive authority across deletion
    // or an identical replacement with a new generation.
    database.delete_bucket_lifecycle(
        &Authority::Native,
        &BucketAccessRequest {
            bucket: request().bucket,
            expected_owner: None,
        },
    )?;

    database.expire_upload(&selected, due)?;
    assert_eq!(count(&database)?, 1);
    configure(&mut database, true, "literal/")?;
    database.expire_upload(&selected, due)?;
    assert_eq!(count(&database)?, 1);
    let current = candidate(&mut database, due)?.unwrap();
    database.expire_upload(&current, due)?;
    assert_eq!(count(&database)?, 0);

    assert_eq!(
        database
            .connection
            .query_row("SELECT outcome FROM multipart_receipts", [], |row| {
                row.get::<_, String>(0)
            })?,
        "aborted"
    );

    close(database)?;

    let mut reopened = MetadataDatabase::open(
        &directory.path().join("metadata.sqlite3"),
        MetadataOpen::Existing,
    )?;

    reopened.expire_upload(&current, due)?;
    assert_eq!(count(&reopened)?, 0);
    close(reopened)?;
    directory.finish()?;

    Ok(())
}

#[test]
fn lifecycle_upload_pages_bound_raw_work_and_survive_deleted_upper_rows_and_stale_cursors()
-> TestResult {
    let directory = TestDirectory::new()?.retain_on_failure();
    let mut database = initialized(&directory)?;
    let mut uploads = Vec::new();

    for suffix in 1..=17 {
        let upload_id = UploadId::new(EpochMillis::new(0)?, [suffix; 16]);
        database.initiate_multipart(&Authority::Native, &request(), upload_id.clone())?;
        uploads.push(upload_id);
    }

    configure(&mut database, true, "no-match/")?;
    let now = EpochMillis::new(172_800_000)?;
    let first = database.upload_expiration_page(now)?.unwrap();
    assert_eq!(first.candidates.len(), 16);

    assert!(
        first
            .candidates
            .iter()
            .all(|(_, entry)| matches!(entry, Ok(None)))
    );

    assert_eq!(
        (
            first.progress.cursor,
            first.progress.examined,
            first.progress.upper
        ),
        (0, 16, 17)
    );

    let stale = first.progress;
    database.advance_sweep(first.progress)?;

    database.abort_multipart_upload(
        &Authority::Native,
        &UploadRequest {
            bucket: request().bucket,
            key: request().key,
            expected_owner: None,
            upload_id: uploads.pop().unwrap(),
        },
    )?;

    database.initiate_multipart(
        &Authority::Native,
        &request(),
        UploadId::new(EpochMillis::new(0)?, [18; 16]),
    )?;

    let final_page = database.upload_expiration_page(now)?.unwrap();
    assert!(final_page.candidates.is_empty());
    assert_eq!(final_page.progress.examined, 17);
    database.advance_sweep(final_page.progress)?;
    database.advance_sweep(stale)?;

    assert_eq!(
        database.connection.query_row(
            "SELECT cursor_row_id FROM maintenance_sweeps WHERE task='abort_uploads'",
            [],
            |row| row.get::<_, i64>(0)
        )?,
        17
    );

    let reset = database.upload_expiration_page(now)?.unwrap();
    assert_eq!((reset.progress.cursor, reset.progress.upper), (0, 18));
    let steps = crate::metadata::lifecycle::uploads::indexed_work(&database.connection)?;

    assert!(
        steps > 0 && steps < 2_000,
        "bounded presence/raw page work: {steps}"
    );

    close(database)?;
    directory.finish()?;

    Ok(())
}

#[test]
fn lifecycle_upload_retirement_does_not_cross_recreated_bucket_names() -> TestResult {
    let directory = TestDirectory::new()?.retain_on_failure();
    let mut database = initialized(&directory)?;
    let first = UploadId::new(EpochMillis::new(0)?, [1; 16]);
    database.initiate_multipart(&Authority::Native, &request(), first)?;
    configure(&mut database, true, "")?;
    let now = EpochMillis::new(172_800_000)?;
    let selected = candidate(&mut database, now)?.unwrap();
    database.delete_bucket(&request().bucket, None)?;
    database.create_bucket(&request().bucket)?;

    database.initiate_multipart(
        &Authority::Native,
        &request(),
        UploadId::new(EpochMillis::new(0)?, [2; 16]),
    )?;

    database.expire_upload(&selected, now)?;

    assert_eq!(
        count(&database)?,
        2,
        "namespace retirement owns the hidden bucket; the new bucket has no rule"
    );

    assert!(candidate(&mut database, now)?.is_none());
    close(database)?;
    directory.finish()?;

    Ok(())
}

#[test]
fn corrupt_upload_rules_preserve_their_bucket_and_do_not_block_other_candidates() -> TestResult {
    let directory = TestDirectory::new()?.retain_on_failure();
    let mut database = initialized(&directory)?;
    let mut second = request();
    second.bucket = BucketName::parse("second-bucket")?;
    database.create_bucket(&second.bucket)?;
    configure(&mut database, true, "")?;

    let rules = database
        .get_bucket_lifecycle(
            &Authority::Native,
            &BucketAccessRequest {
                bucket: request().bucket,
                expected_owner: None,
            },
        )?
        .unwrap();

    database.put_bucket_lifecycle(
        &Authority::Native,
        &BucketAccessRequest {
            bucket: second.bucket.clone(),
            expected_owner: None,
        },
        &rules,
    )?;

    // Interleave buckets to verify that grouping preserves the raw page order.
    let first = request();

    for (index, input) in [&first, &second, &first, &second].into_iter().enumerate() {
        database.initiate_multipart(
            &Authority::Native,
            input,
            UploadId::new(EpochMillis::new(0)?, [u8::try_from(index)?; 16]),
        )?;
    }

    database.connection.execute(
        "UPDATE bucket_lifecycle_rules SET filter_prefix = x'ff' WHERE bucket_id IN
         (SELECT bucket_id FROM buckets WHERE name = 'attachments')",
        [],
    )?;

    let due = EpochMillis::new(172_800_000)?;
    let page = database.upload_expiration_page(due)?.unwrap();
    assert_eq!(page.candidates.len(), 4);

    for ((row_id, candidate), expected_row) in page.candidates.iter().zip(1..=4) {
        assert_eq!(*row_id, expected_row);

        match expected_row {
            1 => assert_eq!(
                candidate.as_ref().unwrap_err().failure(),
                OperationFailure::StoreCorrupt
            ),
            3 => assert!(candidate.as_ref().unwrap().is_none()),
            _ => database.expire_upload(candidate.as_ref().unwrap().as_ref().unwrap(), due)?,
        }
    }

    database.advance_sweep(page.progress)?;
    assert_eq!(count(&database)?, 2);

    let remaining: i64 = database.connection.query_row(
        "SELECT count(*) FROM multipart_uploads AS u JOIN buckets AS b
         ON b.bucket_id = u.bucket_id WHERE b.name = 'attachments'",
        [],
        |row| row.get(0),
    )?;

    assert_eq!(remaining, 2);
    close(database)?;
    directory.finish()?;
    Ok(())
}
