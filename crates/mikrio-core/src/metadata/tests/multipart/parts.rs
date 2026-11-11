use super::*;
use crate::integrity::{ExpectedDigests, IntegrityError, PayloadFacts, PayloadVerifier};
use crate::multipart::{PART_MAX_BYTES, PartNumber, PartRequest, PartReservation};
use crate::object::ByteLength;
use crate::{BlobId, OperationError};
use rusqlite::params;

mod abort;
mod listing;

pub(super) fn part(upload_id: UploadId) -> PartRequest {
    PartRequest {
        bucket: request().bucket,
        key: request().key,
        upload_id,
        part_number: PartNumber::new(7).unwrap(),
        expected_owner: None,
        content_length: None,
        checksum_algorithm: None,
        expected: ExpectedDigests::default(),
    }
}

pub(super) fn facts(algorithm: ChecksumAlgorithm) -> PayloadFacts {
    let mut verifier =
        PayloadVerifier::new(None, PART_MAX_BYTES, algorithm, ExpectedDigests::default()).unwrap();

    verifier.update(b"abc").unwrap();
    verifier.finish(ExpectedDigests::default()).unwrap()
}

pub(super) fn reserve(
    database: &mut MetadataDatabase,
    request: &PartRequest,
    n: u8,
) -> Result<(PartReservation, BlobId), OperationError> {
    let blob_id = BlobId::from_bytes([n; 16]);

    let reservation = database
        .reserve_part(&Authority::Native, request, blob_id)?
        .unwrap();

    Ok((reservation, blob_id))
}

fn mapping(
    database: &MetadataDatabase,
    request: &PartRequest,
) -> rusqlite::Result<(Vec<u8>, Vec<u8>)> {
    database.connection.query_row(
        "SELECT generation, blob_id FROM multipart_parts WHERE upload_id = ?1 AND part_number = ?2",
        params![request.upload_id.to_string(), request.part_number.get()],
        |row| Ok((row.get(0)?, row.get(1)?)),
    )
}

#[test]
fn part_number_size_and_integrity_bounds_do_not_default_or_renegotiate() -> TestResult {
    for value in [0, 10_001, u32::MAX] {
        assert!(PartNumber::new(value).is_err(), "part_number={value}");
    }

    for value in [1, 9999, 10_000] {
        assert_eq!(PartNumber::new(value)?.get(), value);
    }

    let mut request = part(UploadId::new(EpochMillis::new(0)?, [1; 16]));
    let selection = MultipartChecksum::new(ChecksumAlgorithm::Sha256, ChecksumType::Composite)?;

    for length in [0, PART_MAX_BYTES - 1, PART_MAX_BYTES] {
        request.content_length = Some(ByteLength::new(length)?);
        assert!(request.verifier(selection).is_ok(), "length={length}");
    }

    request.content_length = Some(ByteLength::new(PART_MAX_BYTES + 1)?);

    assert!(matches!(
        request.verifier(selection),
        Err(IntegrityError::TooLarge)
    ));

    request.content_length = None;
    request.checksum_algorithm = Some(ChecksumAlgorithm::Crc64Nvme);

    assert!(matches!(
        request.verifier(selection),
        Err(IntegrityError::AlgorithmMismatch)
    ));

    request.checksum_algorithm = None;

    request.expected =
        ExpectedDigests::parse(None, Some(facts(ChecksumAlgorithm::Crc32).checksum()))?;

    assert!(matches!(
        request.verifier(selection),
        Err(IntegrityError::AlgorithmMismatch)
    ));

    Ok(())
}

#[test]
fn part_reservation_checks_identity_negotiation_owner_and_collision_before_allocating() -> TestResult
{
    let directory = TestDirectory::new()?.retain_on_failure();
    let mut database = initialized(&directory)?;
    let created = database.create_multipart_upload(&Authority::Native, &request())?;
    let original = part(created.upload_id);

    for (case_index, (mutated, expected)) in [
        (
            PartRequest {
                key: ObjectKey::parse("wrong")?,
                ..original.clone()
            },
            OperationFailure::NoSuchUpload,
        ),
        (
            PartRequest {
                upload_id: UploadId::new(EpochMillis::new(0)?, [1; 16]),
                ..original.clone()
            },
            OperationFailure::NoSuchUpload,
        ),
        (
            PartRequest {
                expected_owner: Some("invalid".into()),
                ..original.clone()
            },
            OperationFailure::AccessDenied,
        ),
        (
            PartRequest {
                checksum_algorithm: Some(ChecksumAlgorithm::Crc32),
                ..original.clone()
            },
            OperationFailure::BadDigest,
        ),
        (
            PartRequest {
                content_length: Some(ByteLength::new(PART_MAX_BYTES + 1)?),
                ..original.clone()
            },
            OperationFailure::EntityTooLarge,
        ),
    ]
    .into_iter()
    .enumerate()
    {
        let error = reserve(&mut database, &mutated, 1).unwrap_err();
        assert_eq!(error.failure(), expected, "case {case_index}");

        assert_eq!(
            error.outcome(),
            Some(MutationOutcome::NotCommitted),
            "case {case_index}"
        );

        assert_eq!(
            database
                .connection
                .query_row("SELECT count(*) FROM blobs", [], |row| row.get::<_, i64>(0))?,
            0
        );
    }

    let (reservation, blob_id) = reserve(&mut database, &original, 1)?;
    assert_eq!(reservation.checksum, created.checksum);

    assert!(
        database
            .reserve_part(&Authority::Native, &original, blob_id)?
            .is_none()
    );

    let row: (String, String, Vec<u8>, Vec<u8>, String, i64, i64) = database.connection.query_row(
        "SELECT state, write_kind, target_bucket_id, target_key,
                target_upload_id, target_part_number, gc_pending FROM blobs",
        [],
        |row| {
            Ok((
                row.get(0)?,
                row.get(1)?,
                row.get(2)?,
                row.get(3)?,
                row.get(4)?,
                row.get(5)?,
                row.get(6)?,
            ))
        },
    )?;

    assert_eq!(
        row,
        (
            "reserved".into(),
            "upload_part".into(),
            reservation.bucket_id.to_vec(),
            original.key.as_bytes().to_vec(),
            original.upload_id.to_string(),
            7,
            1
        )
    );

    assert_eq!(
        database.connection.query_row(
            "SELECT (SELECT count(*) FROM multipart_parts) + (SELECT count(*) FROM objects)",
            [],
            |row| row.get::<_, i64>(0)
        )?,
        0
    );

    close(database)?;
    directory.finish()?;
    Ok(())
}

#[test]
fn acknowledged_part_replacement_changes_generation_and_retires_only_the_previous_payload()
-> TestResult {
    let directory = TestDirectory::new()?.retain_on_failure();
    let mut database = initialized(&directory)?;
    let created = database.create_multipart_upload(&Authority::Native, &request())?;
    let request = part(created.upload_id);
    let original_time = request.upload_id.initiated_at();
    let payload = facts(ChecksumAlgorithm::Sha256);
    let (reservation, old) = reserve(&mut database, &request, 1)?;
    database.publish_part(&Authority::Native, &request, reservation, old, &payload)?;
    let first = mapping(&database, &request)?;
    let (reservation, new) = reserve(&mut database, &request, 2)?;
    database.publish_part(&Authority::Native, &request, reservation, new, &payload)?;
    let second = mapping(&database, &request)?;
    assert_ne!(first.0, second.0);
    assert_eq!(first.1, old.as_bytes());
    assert_eq!(second.1, new.as_bytes());

    for (blob_id, pending) in [(old, 1), (new, 0)] {
        let row: (String, i64, String, String, String, i64, i64) = database.connection.query_row(
            "SELECT state, byte_length, etag, checksum_type, checksums_json, gc_pending,
             target_bucket_id IS NULL AND target_key IS NULL
             AND target_upload_id IS NULL AND target_part_number IS NULL
             FROM blobs WHERE blob_id = ?1",
            [blob_id.as_bytes().as_slice()],
            |row| {
                Ok((
                    row.get(0)?,
                    row.get(1)?,
                    row.get(2)?,
                    row.get(3)?,
                    row.get(4)?,
                    row.get(5)?,
                    row.get(6)?,
                ))
            },
        )?;

        assert_eq!(
            row,
            (
                "ready".into(),
                3,
                "\"900150983cd24fb0d6963f7d28e17f72\"".into(),
                "FULL_OBJECT".into(),
                "{\"SHA256\":\"ungWv48Bz+pBQUDeXa4iI7ADYaOWF3qctBD/YfIAFa0=\"}".into(),
                pending,
                1
            )
        );
    }

    assert_eq!(
        database
            .connection
            .query_row("SELECT count(*) FROM objects", [], |row| {
                row.get::<_, i64>(0)
            })?,
        0
    );

    close(database)?;

    let reopened = MetadataDatabase::open(
        &directory.path().join("metadata.sqlite3"),
        MetadataOpen::Existing,
    )?;

    assert_eq!(mapping(&reopened, &request)?, second);

    assert_eq!(
        reopened.connection.query_row(
            "SELECT initiated_at_ms FROM multipart_uploads",
            [],
            |row| row.get::<_, i64>(0)
        )?,
        original_time.get()
    );

    close(reopened)?;
    directory.finish()?;
    Ok(())
}

#[test]
fn part_commit_rollback_preserves_predecessor_and_ready_witness_resolves_lost_ack() -> TestResult {
    use super::super::super::transaction::Fault;
    let directory = TestDirectory::new()?.retain_on_failure();
    let mut database = initialized(&directory)?;
    let created = database.create_multipart_upload(&Authority::Native, &request())?;
    let request = part(created.upload_id);
    let payload = facts(ChecksumAlgorithm::Sha256);
    let (reservation, first) = reserve(&mut database, &request, 1)?;
    database.publish_part(&Authority::Native, &request, reservation, first, &payload)?;
    let previous = mapping(&database, &request)?;

    let (reservation, rolled_back) = reserve(&mut database, &request, 2)?;
    database.transaction_fault = Some(Fault::BeforeCommit);

    let error = database
        .publish_part(
            &Authority::Native,
            &request,
            reservation,
            rolled_back,
            &payload,
        )
        .unwrap_err();

    assert_eq!(error.outcome(), Some(MutationOutcome::NotCommitted));
    assert_eq!(mapping(&database, &request)?, previous);

    assert_eq!(
        database.connection.query_row(
            "SELECT gc_pending FROM blobs WHERE blob_id = ?1",
            [first.as_bytes().as_slice()],
            |row| row.get::<_, i64>(0)
        )?,
        0
    );

    let (reservation, committed) = reserve(&mut database, &request, 3)?;
    database.transaction_fault = Some(Fault::AfterCommit);

    database.publish_part(
        &Authority::Native,
        &request,
        reservation,
        committed,
        &payload,
    )?;

    assert_eq!(mapping(&database, &request)?.1, committed.as_bytes());
    close(database)?;
    directory.finish()?;
    Ok(())
}

#[test]
fn claims_retirement_and_reused_bucket_names_fence_reserved_part_publication() -> TestResult {
    let directory = TestDirectory::new()?.retain_on_failure();
    let mut database = initialized(&directory)?;
    let created = database.create_multipart_upload(&Authority::Native, &request())?;
    let request = part(created.upload_id);
    let payload = facts(ChecksumAlgorithm::Sha256);
    let (reservation, first) = reserve(&mut database, &request, 1)?;
    database.publish_part(&Authority::Native, &request, reservation, first, &payload)?;
    let previous = mapping(&database, &request)?;
    let (reservation, losing) = reserve(&mut database, &request, 2)?;

    // Seed the completing state to exercise fencing of a reserved part.
    database.connection.execute(
        "INSERT INTO blobs (blob_id, shard, state, write_kind, target_bucket_id, target_key,
         target_upload_id, created_at_ms, gc_pending, gc_attempt_count)
         VALUES (?1, 3, 'reserved', 'complete_multipart_upload', ?2, ?3, ?4, 0, 1, 0)",
        params![
            [3_u8; 16].as_slice(),
            reservation.bucket_id.as_slice(),
            request.key.as_bytes(),
            request.upload_id.to_string()
        ],
    )?;

    database.connection.execute(
        "UPDATE multipart_uploads SET state = 'completing', completion_claim_id = ?1, completion_blob_id = ?2, completion_parts_digest = ?3",
        params![
            [4_u8; 16].as_slice(),
            [3_u8; 16].as_slice(),
            [5_u8; 32].as_slice()
        ],
    )?;

    for error in [
        database
            .publish_part(&Authority::Native, &request, reservation, losing, &payload)
            .unwrap_err(),
        database
            .reserve_part(&Authority::Native, &request, BlobId::from_bytes([6; 16]))
            .unwrap_err(),
    ] {
        assert_eq!(error.failure(), OperationFailure::OperationAborted);
        assert_eq!(error.outcome(), Some(MutationOutcome::NotCommitted));
    }

    assert_eq!(mapping(&database, &request)?, previous);
    database.delete_bucket(&request.bucket, None)?;
    database.create_bucket(&request.bucket)?;

    let error = database
        .publish_part(&Authority::Native, &request, reservation, losing, &payload)
        .unwrap_err();

    assert_eq!(error.failure(), OperationFailure::NoSuchUpload);
    assert_eq!(mapping(&database, &request)?, previous);
    database.retire_namespace(reservation.bucket_id)?;

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
            "SELECT completion_blob_id FROM multipart_receipts",
            [],
            |row| row.get::<_, Vec<u8>>(0)
        )?,
        [3_u8; 16]
    );

    assert_eq!(
        database
            .publish_part(&Authority::Native, &request, reservation, losing, &payload)
            .unwrap_err()
            .failure(),
        OperationFailure::NoSuchUpload
    );

    close(database)?;
    directory.finish()?;
    Ok(())
}

#[test]
fn part_publication_rechecks_current_permissions_without_initiator_or_tag_shortcuts() -> TestResult
{
    let directory = TestDirectory::new()?.retain_on_failure();
    let mut database = initialized(&directory)?;
    let created = database.create_multipart_upload(&Authority::Native, &request())?;
    let request = part(created.upload_id);
    let principal = database.create_principal(PrincipalName::new("part-writer".into())?)?;
    let key = database.create_access_key(principal.principal_id)?;

    let identity = Authority::Http(Arc::new(
        database
            .resolve_application_key(key.access_key.access_key_id.as_str())?
            .verified(),
    ));

    let payload = facts(ChecksumAlgorithm::Sha256);
    let (reservation, first) = reserve(&mut database, &request, 1)?;
    database.publish_part(&Authority::Native, &request, reservation, first, &payload)?;
    let previous = mapping(&database, &request)?;
    let mut missing = request.clone();
    missing.key = ObjectKey::parse("missing")?;

    assert_eq!(
        database
            .reserve_part(&identity, &missing, BlobId::from_bytes([2; 16]))
            .unwrap_err()
            .failure(),
        OperationFailure::AccessDenied
    );

    let policy = r#"{"Version":"2012-10-17","Statement":{"Effect":"Allow","Action":"s3:PutObject","Resource":"*","Condition":{"Bool":{"s3:ObjectCreationOperation":"false"},"Null":{"s3:if-match":"true","s3:if-none-match":"true","s3:ExistingObjectTag/topic":"true","s3:RequestObjectTag/topic":"true"}}}}"#;
    database.put_principal_policy(principal.principal_id, policy)?;

    assert_eq!(
        database
            .reserve_part(&identity, &missing, BlobId::from_bytes([2; 16]))
            .unwrap_err()
            .failure(),
        OperationFailure::NoSuchUpload
    );

    for (n, change) in [(2, "deny"), (3, "disable"), (4, "revoke")] {
        let blob_id = BlobId::from_bytes([n; 16]);

        let reservation = database
            .reserve_part(&identity, &request, blob_id)?
            .unwrap();

        match change {
            "deny" => database.put_principal_policy(
                principal.principal_id,
                r#"{"Version":"2012-10-17","Statement":{"Effect":"Deny","Action":"s3:PutObject","Resource":"*"}}"#,
            )?,
            "disable" => {
                database.set_principal_enabled(principal.principal_id, false)?;
            }
            "revoke" => {
                database.delete_access_key(principal.principal_id, &key.access_key.access_key_id)?;
            }
            _ => return Err("invalid owned authority change".into()),
        }

        let error = database
            .publish_part(&identity, &request, reservation, blob_id, &payload)
            .unwrap_err();

        assert_eq!(
            error.failure(),
            OperationFailure::AccessDenied,
            "change={change}"
        );

        assert_eq!(
            error.outcome(),
            Some(MutationOutcome::NotCommitted),
            "change={change}"
        );

        assert_eq!(mapping(&database, &request)?, previous, "change={change}");
        database.put_principal_policy(principal.principal_id, policy)?;
        database.set_principal_enabled(principal.principal_id, true)?;
    }

    close(database)?;
    directory.finish()?;
    Ok(())
}

#[test]
fn corrupt_upload_or_previous_part_facts_fail_closed_without_publishing_a_replacement() -> TestResult
{
    let directory = TestDirectory::new()?.retain_on_failure();
    let mut database = initialized(&directory)?;
    let created = database.create_multipart_upload(&Authority::Native, &request())?;
    let request = part(created.upload_id);
    let payload = facts(ChecksumAlgorithm::Sha256);
    let (reservation, first) = reserve(&mut database, &request, 1)?;
    database.publish_part(&Authority::Native, &request, reservation, first, &payload)?;
    let previous = mapping(&database, &request)?;
    let (reservation, next) = reserve(&mut database, &request, 2)?;

    for corruption in [
        "UPDATE multipart_uploads SET checksum_type = 'FULL_OBJECT'",
        "UPDATE multipart_uploads SET checksum_algorithm = 'CRC32'",
        "UPDATE multipart_uploads SET initiated_at_ms = initiated_at_ms - 1",
        "UPDATE blobs SET byte_length = 5368709121 WHERE state = 'ready'",
        "UPDATE blobs SET etag = '\"d41d8cd98f00b204e9800998ecf8427e\"' WHERE state = 'ready'",
        "UPDATE blobs SET checksums_json = '{}' WHERE state = 'ready'",
        "UPDATE blobs SET checksums_json = '{\"SHA256\":null}' WHERE state = 'ready'",
        "UPDATE blobs SET checksums_json = '{\"SHA256\":\"ungWv48Bz+pBQUDeXa4iI7ADYaOWF3qctBD/YfIAFa0=\",\"SHA256\":\"ungWv48Bz+pBQUDeXa4iI7ADYaOWF3qctBD/YfIAFa0=\"}' WHERE state = 'ready'",
        "UPDATE blobs SET checksums_json = '{\"CRC32\":\"NSRBwg==\"}' WHERE state = 'ready'",
        "UPDATE blobs SET checksum_type = 'COMPOSITE' WHERE state = 'ready'",
    ] {
        // Preserve exact restoration facts without a SQL transaction across
        // the operation under test. No storage constraints are disabled.
        database.connection.execute_batch(
            "CREATE TEMP TABLE saved_upload AS SELECT checksum_algorithm, checksum_type, initiated_at_ms FROM multipart_uploads; CREATE TEMP TABLE saved_blob AS SELECT blob_id, byte_length, etag, checksums_json, checksum_type FROM blobs;",
        )?;

        database.connection.execute_batch(corruption)?;

        let error = database
            .publish_part(&Authority::Native, &request, reservation, next, &payload)
            .unwrap_err();

        assert_eq!(
            error.failure(),
            OperationFailure::StoreCorrupt,
            "{corruption}"
        );

        assert_eq!(error.outcome(), Some(MutationOutcome::NotCommitted));
        assert_eq!(mapping(&database, &request)?, previous);

        assert_eq!(
            database.connection.query_row(
                "SELECT state FROM blobs WHERE blob_id = ?1",
                [next.as_bytes().as_slice()],
                |row| row.get::<_, String>(0)
            )?,
            "reserved"
        );

        database.connection.execute_batch(
            "UPDATE multipart_uploads SET
                checksum_algorithm = (SELECT checksum_algorithm FROM saved_upload),
                checksum_type = (SELECT checksum_type FROM saved_upload),
                initiated_at_ms = (SELECT initiated_at_ms FROM saved_upload);
             UPDATE blobs SET byte_length = s.byte_length, etag = s.etag,
                checksums_json = s.checksums_json, checksum_type = s.checksum_type
             FROM saved_blob AS s WHERE blobs.blob_id = s.blob_id;
             DROP TABLE saved_upload; DROP TABLE saved_blob;",
        )?;
    }

    let wrong = PartRequest {
        part_number: PartNumber::new(8)?,
        ..request.clone()
    };

    let error = database
        .publish_part(&Authority::Native, &wrong, reservation, next, &payload)
        .unwrap_err();

    assert_eq!(error.failure(), OperationFailure::StoreCorrupt);
    assert_eq!(mapping(&database, &request)?, previous);
    close(database)?;
    directory.finish()?;
    Ok(())
}
