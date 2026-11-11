use super::*;
mod gc_ledger;
use crate::integrity::{ExpectedDigests, PayloadFacts, PutIntegrity, PutIntegrityInput};
use crate::object::{
    BucketName, ObjectKey, PutRequest, ReadConditions, ReadRequest, Representation,
    RepresentationInput, UserMetadata, WriteConditions,
};
use crate::{BlobId, MutationOutcome, OperationFailure};

mod access;
mod attributes;
mod conditions;
mod expiration;
mod fences;
mod persistence;
mod settlement;

fn setup(
    directory: &TestDirectory,
) -> Result<(MetadataDatabase, PutRequest), Box<dyn std::error::Error>> {
    let mut database = database(directory)?;

    database.initialize(&BootstrapValues::generate(
        StoreId::generate()?,
        "us-east-1".into(),
        EpochMillis::new(0)?,
    )?)?;

    let request = PutRequest {
        bucket: BucketName::parse("attachments")?,
        key: ObjectKey::parse("literal/key")?,
        expected_owner: None,
        content_length: None,
        representation: Representation::new(RepresentationInput::default())?,
        metadata: UserMetadata::default(),
        tags: None,
        integrity: PutIntegrity::new(PutIntegrityInput::default())?,
        conditions: WriteConditions::default(),
    };

    database.create_bucket(&request.bucket)?;
    Ok((database, request))
}

fn facts(request: &PutRequest, bytes: &[u8]) -> Result<PayloadFacts, Box<dyn std::error::Error>> {
    let mut verifier = request.integrity.verifier(None)?;
    verifier.update(bytes)?;
    Ok(verifier.finish(ExpectedDigests::default())?)
}

fn read(request: &PutRequest) -> ReadRequest {
    ReadRequest {
        bucket: request.bucket.clone(),
        key: request.key.clone(),
        expected_owner: None,
        conditions: ReadConditions::default(),
        range: None,
        part_number: None,
        response: crate::object::Representation::default(),
    }
}

#[test]
fn publication_commit_witness_distinguishes_rollback_from_lost_acknowledgment()
-> Result<(), Box<dyn std::error::Error>> {
    let directory = TestDirectory::new()?;
    let (mut database, request) = setup(&directory)?;
    // SQLite transaction qualification: payload bytes are not synthesized here.
    // The connection tests separately run actual guarded publication and reads.
    let first = BlobId::from_bytes([1; 16]);

    let bucket = database
        .reserve_put(&crate::object::Authority::Native, &request, first)?
        .unwrap();

    let original = facts(&request, b"original")?;

    database.publish_put(
        &crate::object::Authority::Native,
        &request,
        bucket,
        first,
        &original,
    )?;

    let generation = database
        .select_object(&crate::object::Authority::Native, &read(&request))?
        .generation;

    let output = BlobId::from_bytes([2; 16]);
    database.reserve_put(&crate::object::Authority::Native, &request, output)?;
    let replacement = facts(&request, b"replacement")?;
    database.transaction_fault = Some(super::super::transaction::Fault::BeforeCommit);

    let error = database
        .publish_put(
            &crate::object::Authority::Native,
            &request,
            bucket,
            output,
            &replacement,
        )
        .unwrap_err();

    assert_eq!(error.outcome(), Some(MutationOutcome::NotCommitted));

    assert_eq!(
        database
            .select_object(&crate::object::Authority::Native, &read(&request))?
            .generation,
        generation
    );

    database.transaction_fault = Some(super::super::transaction::Fault::AfterCommit);

    database.publish_put(
        &crate::object::Authority::Native,
        &request,
        bucket,
        output,
        &replacement,
    )?;

    let selected = database.select_object(&crate::object::Authority::Native, &read(&request))?;
    assert_eq!(selected.blob_id, output);
    assert_ne!(selected.generation, generation);

    assert_eq!(
        database.connection.query_row::<i64, _, _>(
            "SELECT gc_pending FROM blobs WHERE blob_id = ?1",
            [first.as_bytes().as_slice()],
            |row| row.get(0)
        )?,
        1
    );

    close(database)?;
    directory.finish()?;
    Ok(())
}

#[test]
fn corruption_is_visible_and_never_repaired_by_delete_or_gc()
-> Result<(), Box<dyn std::error::Error>> {
    let directory = TestDirectory::new()?;
    let (mut database, request) = setup(&directory)?;
    let blob = BlobId::from_bytes([3; 16]);

    let bucket = database
        .reserve_put(&crate::object::Authority::Native, &request, blob)?
        .unwrap();

    database.publish_put(
        &crate::object::Authority::Native,
        &request,
        bucket,
        blob,
        &facts(&request, b"bytes")?,
    )?;

    let checksums: String =
        database
            .connection
            .query_row("SELECT checksums_json FROM blobs", [], |row| row.get(0))?;

    for invalid in [
        r#"{"CRC64NVME":null}"#,
        r#"{"CRC64NVME":"AAAA","CRC64NVME":"AAAA"}"#,
        r#"{"unknown":"AAAA"}"#,
    ] {
        database
            .connection
            .execute("UPDATE blobs SET checksums_json = ?1", [invalid])?;

        assert_eq!(
            database
                .select_object(&crate::object::Authority::Native, &read(&request))
                .unwrap_err()
                .failure(),
            OperationFailure::StoreCorrupt
        );

        assert_eq!(
            database
                .delete_object(&crate::object::Authority::Native, &read(&request), None)
                .unwrap_err()
                .failure(),
            OperationFailure::StoreCorrupt
        );
    }

    database.connection.execute(
        "UPDATE blobs SET checksums_json = ?1, gc_pending = 1",
        [checksums],
    )?;

    assert!(!database.gc_mark(blob)?);

    database.connection.execute_batch(
        "PRAGMA ignore_check_constraints = ON; UPDATE blobs SET state = 'deleting', gc_pending = 1; PRAGMA ignore_check_constraints = OFF;",
    )?;

    assert_eq!(
        database.gc_mark(blob).unwrap_err().failure(),
        OperationFailure::StoreCorrupt
    );

    assert_eq!(
        database.gc_finish(blob).unwrap_err().failure(),
        OperationFailure::StoreCorrupt
    );

    assert_eq!(
        database
            .connection
            .query_row::<i64, _, _>("SELECT count(*) FROM objects", [], |row| row.get(0))?,
        1
    );

    close(database)?;
    directory.finish()?;
    Ok(())
}

#[test]
fn retirement_rechecks_original_bucket_id_and_gc_keeps_referenced_ready_blobs()
-> Result<(), Box<dyn std::error::Error>> {
    let directory = TestDirectory::new()?;
    let (mut database, request) = setup(&directory)?;
    let abandoned = BlobId::from_bytes([7; 16]);

    let original_bucket = database
        .reserve_put(&crate::object::Authority::Native, &request, abandoned)?
        .unwrap();

    database.delete_bucket(&request.bucket, None)?;
    database.create_bucket(&request.bucket)?;

    assert_eq!(
        database
            .publish_put(
                &crate::object::Authority::Native,
                &request,
                original_bucket,
                abandoned,
                &facts(&request, b"late")?
            )
            .unwrap_err()
            .failure(),
        OperationFailure::NoSuchBucket
    );

    assert_eq!(database.retirement_candidate(None)?, Some(original_bucket));
    database.retire_namespace(original_bucket)?;
    assert_eq!(database.retirement_candidate(None)?, None);
    assert_eq!(database.head_bucket(&request.bucket, None)?, "us-east-1");
    let page = database.gc_page(7)?.unwrap();

    assert_eq!(
        page.candidates
            .iter()
            .map(|(_, blob)| *blob.as_ref().unwrap())
            .collect::<Vec<_>>(),
        vec![abandoned]
    );

    assert!(database.gc_mark(abandoned)?);
    database.gc_finish(abandoned)?;
    database.advance_sweep(page.progress)?;
    assert!(database.gc_page(7)?.is_none());
    close(database)?;
    directory.finish()?;
    Ok(())
}

#[test]
fn tag_only_mutations_change_generation_preserve_payload_and_settle_failures()
-> Result<(), Box<dyn std::error::Error>> {
    let directory = TestDirectory::new()?;
    let (mut database, request) = setup(&directory)?;
    let blob = BlobId::from_bytes([8; 16]);

    let bucket = database
        .reserve_put(&crate::object::Authority::Native, &request, blob)?
        .unwrap();

    database.publish_put(
        &crate::object::Authority::Native,
        &request,
        bucket,
        blob,
        &facts(&request, b"retained")?,
    )?;

    let original = database.select_object(&crate::object::Authority::Native, &read(&request))?;
    let tags = crate::object::ObjectTags::new([("Status".into(), "Ready".into())])?;
    database.transaction_fault = Some(super::super::transaction::Fault::BeforeCommit);

    assert_eq!(
        database
            .replace_object_tags(&read(&request), &tags)
            .unwrap_err()
            .outcome(),
        Some(MutationOutcome::NotCommitted)
    );

    assert_eq!(
        database
            .select_object(&crate::object::Authority::Native, &read(&request))?
            .generation,
        original.generation
    );

    database.transaction_fault = Some(super::super::transaction::Fault::AfterCommit);

    assert_eq!(
        database
            .replace_object_tags(&read(&request), &tags)
            .unwrap_err()
            .outcome(),
        Some(MutationOutcome::Unknown)
    );

    let changed = database.select_object(&crate::object::Authority::Native, &read(&request))?;
    assert_ne!(changed.generation, original.generation);
    assert_eq!(changed.blob_id, original.blob_id);
    assert_eq!(changed.etag, original.etag);
    assert_eq!(changed.checksums, original.checksums);
    assert_eq!(changed.last_modified, original.last_modified);
    assert_eq!(changed.tags, tags);

    assert_eq!(
        database
            .connection
            .query_row::<i64, _, _>("SELECT gc_pending FROM blobs", [], |row| row.get(0))?,
        0
    );

    database.replace_object_tags(&read(&request), &crate::object::ObjectTags::default())?;

    assert_eq!(
        database
            .select_object(&crate::object::Authority::Native, &read(&request))?
            .tags,
        crate::object::ObjectTags::default()
    );

    close(database)?;
    directory.finish()?;
    Ok(())
}
