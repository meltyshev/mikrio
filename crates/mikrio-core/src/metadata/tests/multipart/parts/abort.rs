use super::*;
use crate::multipart::UploadRequest;

pub(super) fn control(request: &PartRequest) -> UploadRequest {
    UploadRequest {
        bucket: request.bucket.clone(),
        key: request.key.clone(),
        upload_id: request.upload_id.clone(),
        expected_owner: request.expected_owner.clone(),
    }
}

#[test]
fn abort_atomically_retires_parts_and_preserves_claim_without_creating_an_object() -> TestResult {
    let directory = TestDirectory::new()?.retain_on_failure();
    let mut database = initialized(&directory)?;

    for claimed in [false, true] {
        let created = database.create_multipart_upload(&Authority::Native, &request())?;
        let part = part(created.upload_id);
        let payload = facts(ChecksumAlgorithm::Sha256);
        let base = if claimed { 10 } else { 1 };
        let (reservation, first) = reserve(&mut database, &part, base)?;
        database.publish_part(&Authority::Native, &part, reservation, first, &payload)?;
        let (reservation, losing) = reserve(&mut database, &part, base + 1)?;

        if claimed {
            // Seed a completion claim to exercise abort's claimed-upload path.
            database.connection.execute(
                "INSERT INTO blobs (blob_id, shard, state, write_kind, target_bucket_id,
                 target_key, target_upload_id, created_at_ms, gc_pending, gc_attempt_count)
                 VALUES (?1, 20, 'reserved', 'complete_multipart_upload', ?2, ?3, ?4, 0, 1, 0)",
                params![
                    [20_u8; 16].as_slice(),
                    reservation.bucket_id.as_slice(),
                    part.key.as_bytes(),
                    part.upload_id.to_string()
                ],
            )?;

            database.connection.execute(
                "UPDATE multipart_uploads SET state = 'completing', completion_claim_id = ?1,
                 completion_blob_id = ?2, completion_parts_digest = ?3 WHERE upload_id = ?4",
                params![
                    [21_u8; 16].as_slice(),
                    [20_u8; 16].as_slice(),
                    [22_u8; 32].as_slice(),
                    part.upload_id.to_string()
                ],
            )?;
        }

        database.abort_multipart_upload(&Authority::Native, &control(&part))?;
        assert_eq!(count(&database)?, 0);

        let (outcome, [claim, output, digest]) = database.connection.query_row(
            "SELECT outcome, completion_claim_id, completion_blob_id, completion_parts_digest
                 FROM multipart_receipts WHERE upload_id = ?1",
            [part.upload_id.to_string()],
            |row| {
                Ok((
                    row.get::<_, String>(0)?,
                    [row.get::<_, Option<Vec<u8>>>(1)?, row.get(2)?, row.get(3)?],
                ))
            },
        )?;

        assert_eq!(outcome, "aborted");
        assert_eq!(claim.as_deref(), claimed.then_some([21_u8; 16].as_slice()));
        assert_eq!(output.as_deref(), claimed.then_some([20_u8; 16].as_slice()));
        assert_eq!(digest.as_deref(), claimed.then_some([22_u8; 32].as_slice()));

        for id in [first, losing] {
            assert_eq!(
                database.connection.query_row(
                    "SELECT gc_pending FROM blobs WHERE blob_id = ?1",
                    [id.as_bytes().as_slice()],
                    |row| row.get::<_, i64>(0),
                )?,
                1
            );
        }

        assert_eq!(
            database.connection.query_row(
                "SELECT (SELECT count(*) FROM objects) + (SELECT count(*) FROM multipart_parts)",
                [],
                |row| row.get::<_, i64>(0),
            )?,
            0
        );

        for error in [
            database
                .publish_part(&Authority::Native, &part, reservation, losing, &payload)
                .unwrap_err(),
            database
                .abort_multipart_upload(&Authority::Native, &control(&part))
                .unwrap_err(),
        ] {
            assert_eq!(error.failure(), OperationFailure::NoSuchUpload);
            assert_eq!(error.outcome(), Some(MutationOutcome::NotCommitted));
        }
    }

    close(database)?;
    directory.finish()?;
    Ok(())
}

#[test]
fn abort_rollback_and_lost_commit_acknowledgment_keep_honest_outcomes() -> TestResult {
    use super::super::super::super::transaction::Fault;
    let directory = TestDirectory::new()?.retain_on_failure();
    let mut database = initialized(&directory)?;
    let created = database.create_multipart_upload(&Authority::Native, &request())?;
    let request = part(created.upload_id);
    let payload = facts(ChecksumAlgorithm::Sha256);
    let (reservation, first) = reserve(&mut database, &request, 1)?;
    database.publish_part(&Authority::Native, &request, reservation, first, &payload)?;
    let previous = mapping(&database, &request)?;

    database.transaction_fault = Some(Fault::BeforeCommit);

    let error = database
        .abort_multipart_upload(&Authority::Native, &control(&request))
        .unwrap_err();

    assert_eq!(error.outcome(), Some(MutationOutcome::NotCommitted));
    assert_eq!(mapping(&database, &request)?, previous);
    assert_eq!(count(&database)?, 1);

    database.transaction_fault = Some(Fault::AfterCommit);

    let error = database
        .abort_multipart_upload(&Authority::Native, &control(&request))
        .unwrap_err();

    assert_eq!(error.outcome(), Some(MutationOutcome::Unknown));
    assert_eq!(count(&database)?, 0);
    close(database)?;

    let reopened = MetadataDatabase::open(
        &directory.path().join("metadata.sqlite3"),
        MetadataOpen::Existing,
    )?;

    assert_eq!(count(&reopened)?, 0);

    assert_eq!(
        reopened
            .connection
            .query_row("SELECT outcome FROM multipart_receipts", [], |row| {
                row.get::<_, String>(0)
            })?,
        "aborted"
    );

    close(reopened)?;
    directory.finish()?;
    Ok(())
}

#[test]
fn abort_initiator_defaults_require_current_credentials_and_yield_to_explicit_denies() -> TestResult
{
    let directory = TestDirectory::new()?.retain_on_failure();
    let mut database = initialized(&directory)?;
    let principal = database.create_principal(PrincipalName::new("uploader".into())?)?;
    let key = database.create_access_key(principal.principal_id)?;

    let identity = Authority::Http(Arc::new(
        database
            .resolve_application_key(key.access_key.access_key_id.as_str())?
            .verified(),
    ));

    database.put_principal_policy(
        principal.principal_id,
        r#"{"Version":"2012-10-17","Statement":{"Effect":"Allow","Action":"s3:PutObject*","Resource":"*"}}"#,
    )?;

    let created = database.create_multipart_upload(&identity, &request())?;
    let request = control(&part(created.upload_id));

    for state in ["policy", "disabled", "revoked"] {
        match state {
            "policy" => database.put_principal_policy(
                principal.principal_id,
                r#"{"Version":"2012-10-17","Statement":{"Effect":"Deny","Action":"s3:AbortMultipartUpload","Resource":"*"}}"#,
            )?,
            "disabled" => {
                database.set_principal_enabled(principal.principal_id, false)?;
            }
            "revoked" => {
                database.delete_access_key(principal.principal_id, &key.access_key.access_key_id)?;
            }
            _ => return Err("unknown authority fixture".into()),
        }

        assert_eq!(
            database
                .abort_multipart_upload(&identity, &request)
                .unwrap_err()
                .failure(),
            OperationFailure::AccessDenied,
            "{state}"
        );

        assert_eq!(count(&database)?, 1);
        database.delete_principal_policy(principal.principal_id)?;
        database.set_principal_enabled(principal.principal_id, true)?;
    }

    // Rotation preserves the principal ARN; the old key never becomes valid again.
    let replacement = database.create_access_key(principal.principal_id)?;

    let rotated = Authority::Http(Arc::new(
        database
            .resolve_application_key(replacement.access_key.access_key_id.as_str())?
            .verified(),
    ));

    database.abort_multipart_upload(&rotated, &request)?;
    assert_eq!(count(&database)?, 0);
    close(database)?;
    directory.finish()?;
    Ok(())
}
