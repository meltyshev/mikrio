use super::*;
mod claim_fence;
mod gc_ledger;
mod histories;
mod limits;
mod retention;
mod selection;
use crate::BlobId;
use crate::integrity::{Checksum, PayloadFacts};
use crate::multipart::{
    CompleteRequest, CompletionClaim, CompletionParts, PartNumber, SelectedPart, UploadRequest,
};

use crate::object::{ByteLength, WriteConditions};

fn fixture(
    database: &mut MetadataDatabase,
) -> Result<(CompleteRequest, PayloadFacts), Box<dyn std::error::Error>> {
    fixture_at(database, EpochMillis::new(1000)?)
}

fn fixture_at(
    database: &mut MetadataDatabase,
    initiated: EpochMillis,
) -> Result<(CompleteRequest, PayloadFacts), Box<dyn std::error::Error>> {
    let upload = database.initiate_multipart(
        &Authority::Native,
        &request(),
        UploadId::new(initiated, [1; 16]),
    )?;

    let mut part = super::parts::part(upload.upload_id.clone());
    part.part_number = PartNumber::new(1)?;
    let facts = super::parts::facts(ChecksumAlgorithm::Sha256);
    let (reservation, blob) = super::parts::reserve(database, &part, 34)?;
    database.publish_part(&Authority::Native, &part, reservation, blob, &facts)?;

    database.connection.execute(
        "UPDATE multipart_parts SET generation = ?1",
        [[17_u8; 16].as_slice()],
    )?;

    Ok((
        CompleteRequest {
            upload: UploadRequest {
                bucket: part.bucket,
                key: part.key,
                upload_id: upload.upload_id,
                expected_owner: None,
            },
            parts: CompletionParts::new([SelectedPart {
                part_number: part.part_number,
                etag: facts.etag(),
                checksum: Some(facts.checksum()),
            }])?,
            conditions: WriteConditions::default(),
            content_length: None,
            checksum_type: None,
            checksum: None,
        },
        facts,
    ))
}

fn claim(number: u8) -> CompletionClaim {
    CompletionClaim {
        claim_id: [number; 16],
        blob_id: BlobId::from_bytes([number + 1; 16]),
    }
}

fn selected(number: u32) -> SelectedPart {
    SelectedPart {
        part_number: PartNumber::new(number).unwrap(),
        etag: "\"900150983cd24fb0d6963f7d28e17f72\"".into(),
        checksum: None,
    }
}

#[test]
fn selection_shape_is_bounded_and_rejects_empty_duplicate_and_reordered_parts() -> TestResult {
    assert_eq!(
        CompletionParts::new([]).unwrap_err().failure(),
        OperationFailure::InvalidArgument
    );

    for numbers in [[1, 1], [2, 1]] {
        assert_eq!(
            CompletionParts::new(numbers.map(selected))
                .unwrap_err()
                .failure(),
            OperationFailure::InvalidPartOrder,
            "numbers={numbers:?}"
        );
    }

    assert_eq!(
        CompletionParts::new((1..=10_000).map(selected))?
            .parts()
            .len(),
        10_000
    );

    let mut consumed = 0;

    let overlong = (1..=10_000).chain(std::iter::repeat(10_000)).map(|number| {
        consumed += 1;
        selected(number)
    });

    assert_eq!(
        CompletionParts::new(overlong).unwrap_err().failure(),
        OperationFailure::InvalidArgument
    );

    assert_eq!(consumed, 10_001);

    for etag in [
        "",
        "*",
        "900150983cd24fb0d6963f7d28e17f72",
        "\"900150983cd24fb0d6963f7d28e17f72-1\"",
    ] {
        let mut part = selected(1);
        part.etag = etag.into();

        assert_eq!(
            CompletionParts::new([part]).unwrap_err().failure(),
            OperationFailure::InvalidPart,
            "etag={etag:?}"
        );
    }

    Ok(())
}

#[test]
fn claim_revalidates_generations_and_fences_competing_completion_and_part_publication() -> TestResult
{
    let directory = TestDirectory::new()?.retain_on_failure();
    let mut database = initialized(&directory)?;
    let (request, facts) = fixture(&mut database)?;
    let selected = database.select_completion(&Authority::Native, &request)?;

    // Literal independently encoded with Python hashlib/struct; format bytes are documented.
    assert_eq!(
        selected.fingerprint,
        crate::values::decode_hex::<32>(
            b"e9273f37ad4db34dee219e2667eda72db4e8572aa3e180de088cbab8a676443b"
        )?
    );

    let mut part = super::parts::part(request.upload.upload_id.clone());
    part.part_number = PartNumber::new(1)?;
    let (reservation, blob) = super::parts::reserve(&mut database, &part, 40)?;
    database.publish_part(&Authority::Native, &part, reservation, blob, &facts)?;

    assert_eq!(
        database
            .claim_completion(&Authority::Native, &request, &selected, claim(60))
            .unwrap_err()
            .failure(),
        OperationFailure::InvalidPart
    );

    assert_eq!(
        database.connection.query_row(
            "SELECT count(*) FROM blobs WHERE write_kind = 'complete_multipart_upload'",
            [],
            |row| row.get::<_, i64>(0)
        )?,
        0
    );

    let current = database.select_completion(&Authority::Native, &request)?;
    assert_ne!(current.fingerprint, selected.fingerprint);
    let (late, late_blob) = super::parts::reserve(&mut database, &part, 41)?;
    assert!(database.claim_completion(&Authority::Native, &request, &current, claim(60))?);

    assert_eq!(
        database
            .claim_completion(&Authority::Native, &request, &current, claim(62))
            .unwrap_err()
            .failure(),
        OperationFailure::OperationAborted
    );

    assert_eq!(
        database
            .publish_part(&Authority::Native, &part, late, late_blob, &facts)
            .unwrap_err()
            .failure(),
        OperationFailure::OperationAborted
    );

    database.abort_multipart_upload(&Authority::Native, &request.upload)?;

    assert_eq!(
        database
            .publish_completion(&Authority::Native, &request, &current, claim(60), &facts)
            .unwrap_err()
            .failure(),
        OperationFailure::NoSuchUpload
    );

    let receipt: (Vec<u8>, Vec<u8>, Vec<u8>) = database.connection.query_row(
        "SELECT completion_claim_id, completion_blob_id, completion_parts_digest FROM multipart_receipts",
        [],
        |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
    )?;

    assert_eq!(
        receipt,
        (vec![60; 16], vec![61; 16], current.fingerprint.to_vec())
    );

    close(database)?;
    directory.finish()?;

    Ok(())
}

#[test]
fn completion_modes_validate_missing_checksums_numbers_lengths_and_nonfinal_sizes() -> TestResult {
    let directory = TestDirectory::new()?.retain_on_failure();
    let mut database = initialized(&directory)?;
    let (request, _) = fixture(&mut database)?;
    let mut changed = request.clone();
    changed.parts = CompletionParts::new([selected(1)])?;

    assert_eq!(
        database
            .select_completion(&Authority::Native, &changed)
            .unwrap_err()
            .failure(),
        OperationFailure::InvalidPart
    );

    changed.parts = CompletionParts::new([selected(2)])?;

    assert_eq!(
        database
            .select_completion(&Authority::Native, &changed)
            .unwrap_err()
            .failure(),
        OperationFailure::InvalidPartOrder
    );

    changed = request.clone();
    changed.content_length = Some(ByteLength::new(4)?);

    assert_eq!(
        database
            .select_completion(&Authority::Native, &changed)
            .unwrap_err()
            .failure(),
        OperationFailure::InvalidRequest
    );

    let mut wrong = request.parts.parts()[0].clone();
    wrong.checksum = Some(Checksum::Sha256([0; 32]));
    changed = request.clone();
    changed.parts = CompletionParts::new([wrong])?;

    assert_eq!(
        database
            .select_completion(&Authority::Native, &changed)
            .unwrap_err()
            .failure(),
        OperationFailure::InvalidPart
    );

    changed.parts = CompletionParts::new([request.parts.parts()[0].clone(), selected(2)])?;

    assert_eq!(
        database
            .select_completion(&Authority::Native, &changed)
            .unwrap_err()
            .failure(),
        OperationFailure::EntityTooSmall
    );

    database.abort_multipart_upload(&Authority::Native, &request.upload)?;
    let mut initiation = super::request();

    initiation.checksum =
        MultipartChecksum::new(ChecksumAlgorithm::Crc64Nvme, ChecksumType::FullObject)?;

    let upload = database.create_multipart_upload(&Authority::Native, &initiation)?;
    let part = super::parts::part(upload.upload_id.clone());
    let facts = super::parts::facts(ChecksumAlgorithm::Crc64Nvme);
    let (reservation, blob) = super::parts::reserve(&mut database, &part, 55)?;
    database.publish_part(&Authority::Native, &part, reservation, blob, &facts)?;
    changed.upload.upload_id = upload.upload_id;

    changed.parts = CompletionParts::new([SelectedPart {
        part_number: PartNumber::new(7)?,
        etag: facts.etag(),
        checksum: None,
    }])?;

    let selected = database.select_completion(&Authority::Native, &changed)?;
    assert_eq!(selected.layout.parts()[0].part_number, 7);
    close(database)?;
    directory.finish()?;

    Ok(())
}

#[test]
fn publication_is_atomic_preserves_initiation_facts_and_resolves_exact_receipt_after_lost_acknowledgment()
-> TestResult {
    use super::super::super::transaction::Fault;
    let directory = TestDirectory::new()?.retain_on_failure();
    let mut database = initialized(&directory)?;
    let (request, facts) = fixture(&mut database)?;
    let selected = database.select_completion(&Authority::Native, &request)?;
    assert!(database.claim_completion(&Authority::Native, &request, &selected, claim(60))?);
    database.transaction_fault = Some(Fault::BeforeCommit);

    assert_eq!(
        database
            .publish_completion(&Authority::Native, &request, &selected, claim(60), &facts)
            .unwrap_err()
            .outcome(),
        Some(MutationOutcome::NotCommitted)
    );

    assert_eq!(
        database.connection.query_row(
            "SELECT (SELECT count(*) FROM objects) + (SELECT count(*) FROM blob_parts)
             + (SELECT count(*) FROM multipart_receipts)",
            [],
            |row| row.get::<_, i64>(0),
        )?,
        0
    );

    assert_eq!(count(&database)?, 1);

    // Metadata-only recovery probe; physical exclusion is covered by process tests.
    assert!(database.gc_mark(claim(60).blob_id)?);
    assert!(database.claim_completion(&Authority::Native, &request, &selected, claim(62))?);
    database.transaction_fault = Some(Fault::AfterCommit);

    let completed =
        database.publish_completion(&Authority::Native, &request, &selected, claim(62), &facts)?;

    assert_eq!(completed.etag, "\"af5da9f45af7a300e3aded972f8ff687-1\"");

    assert_eq!(
        completed.checksum.encode(),
        "T4tCwi3TcptRm6b2jS2nzFstYG0F2u1a1RKMwD5sY1g=-1"
    );

    assert_eq!(completed.length.get(), 3);
    assert_eq!(count(&database)?, 0);

    let fields: (i64, i64, String, String) = database.connection.query_row(
        "SELECT last_modified_at_ms, lifecycle_anchor_at_ms, user_metadata_json, tags_json FROM objects",
        [],
        |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?)),
    )?;

    assert_eq!(
        fields,
        (
            1000,
            1000,
            r#"{"mixed":"value é"}"#.into(),
            r#"{"topic":"history"}"#.into()
        )
    );

    assert_eq!(
        database
            .connection
            .query_row("SELECT count(*) FROM multipart_parts", [], |row| {
                row.get::<_, i64>(0)
            })?,
        0
    );

    let layout: (i64, i64, i64) = database.connection.query_row(
        "SELECT part_number,byte_offset,byte_length FROM blob_parts",
        [],
        |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
    )?;

    assert_eq!(layout, (1, 0, 3));
    close(database)?;
    directory.finish()?;

    Ok(())
}

#[test]
fn current_completion_permissions_conditions_and_tag_context_revalidate_after_claim() -> TestResult
{
    let directory = TestDirectory::new()?.retain_on_failure();
    let mut database = initialized(&directory)?;
    let (mut request, facts) = fixture(&mut database)?;
    let principal = database.create_principal(PrincipalName::new("completer".into())?)?;
    let key = database.create_access_key(principal.principal_id)?;

    let authority = Authority::Http(Arc::new(
        database
            .resolve_application_key(key.access_key.access_key_id.as_str())?
            .verified(),
    ));

    database.put_principal_policy(
        principal.principal_id,
        r#"{"Version":"2012-10-17","Statement":{"Effect":"Allow","Action":"s3:PutObject","Resource":"*"}}"#,
    )?;

    database.select_completion(&authority, &request)?;

    database.put_principal_policy(
        principal.principal_id,
        r#"{"Version":"2012-10-17","Statement":{"Effect":"Allow","Action":"s3:PutObject","Resource":"*","Condition":{"Bool":{"s3:ObjectCreationOperation":"true"},"Null":{"s3:RequestObjectTag/topic":"true","s3:RequestObjectTagKeys":"true","s3:ExistingObjectTag/topic":"true"}}}}"#,
    )?;

    request.conditions = WriteConditions::new(None, Some("*".into()))?;
    let selected = database.select_completion(&authority, &request)?;
    request.conditions = WriteConditions::new(Some("*".into()), None)?;

    assert_eq!(
        database
            .select_completion(&authority, &request)
            .unwrap_err()
            .failure(),
        OperationFailure::AccessDenied
    );

    request.conditions = WriteConditions::new(None, Some("*".into()))?;
    assert!(database.claim_completion(&authority, &request, &selected, claim(60))?);
    database.set_principal_enabled(principal.principal_id, false)?;

    assert_eq!(
        database
            .publish_completion(&authority, &request, &selected, claim(60), &facts)
            .unwrap_err()
            .failure(),
        OperationFailure::AccessDenied
    );

    assert!(database.gc_mark(claim(60).blob_id)?);

    assert_eq!(
        database
            .connection
            .query_row("SELECT count(*) FROM multipart_parts", [], |row| {
                row.get::<_, i64>(0)
            })?,
        1
    );

    database.set_principal_enabled(principal.principal_id, true)?;
    assert!(database.claim_completion(&authority, &request, &selected, claim(62))?);
    database.delete_access_key(principal.principal_id, &key.access_key.access_key_id)?;

    assert_eq!(
        database
            .publish_completion(&authority, &request, &selected, claim(62), &facts)
            .unwrap_err()
            .failure(),
        OperationFailure::AccessDenied
    );

    assert!(database.gc_mark(claim(62).blob_id)?);
    let replacement = database.create_access_key(principal.principal_id)?;

    let authority = Authority::Http(Arc::new(
        database
            .resolve_application_key(replacement.access_key.access_key_id.as_str())?
            .verified(),
    ));

    assert!(database.claim_completion(&authority, &request, &selected, claim(64))?);
    database.publish_completion(&authority, &request, &selected, claim(64), &facts)?;
    close(database)?;
    directory.finish()?;

    Ok(())
}
