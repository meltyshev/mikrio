use std::sync::Arc;

use super::*;
use crate::access::{Bootstrap, BucketAccessRequest, HttpIdentity, PrincipalName};
use crate::integrity::{ChecksumAlgorithm, ChecksumType};
use crate::multipart::{CreateRequest, MultipartChecksum};
use crate::object::{
    Authority, BucketName, ObjectKey, ObjectTags, Representation, RepresentationInput, UserMetadata,
};
use crate::{MutationOutcome, OperationFailure, UploadId};

mod completion;
mod parts;
mod retention;
mod uploads;

type TestResult = Result<(), Box<dyn std::error::Error>>;

fn request() -> CreateRequest {
    CreateRequest {
        bucket: BucketName::parse("attachments").unwrap(),
        key: ObjectKey::parse("literal/../key+é").unwrap(),
        expected_owner: None,
        representation: Representation::new(RepresentationInput {
            content_type: Some("application/example".into()),
            content_encoding: Some(String::new()),
            content_disposition: Some("inline".into()),
            content_language: Some("en".into()),
            cache_control: Some("max-age=0".into()),
            expires: Some(EpochMillis::new(-1).unwrap()),
        })
        .unwrap(),
        metadata: UserMetadata::new([("Mixed".into(), "value é".into())]).unwrap(),
        tags: Some(ObjectTags::new([("topic".into(), "history".into())]).unwrap()),
        checksum: MultipartChecksum::new(ChecksumAlgorithm::Sha256, ChecksumType::Composite)
            .unwrap(),
    }
}

fn initialized(directory: &TestDirectory) -> Result<MetadataDatabase, Box<dyn std::error::Error>> {
    let mut database = database(directory)?;

    database.initialize(&BootstrapValues::generate(
        StoreId::generate()?,
        "us-east-1".into(),
        EpochMillis::new(0)?,
    )?)?;

    database.create_bucket(&request().bucket)?;
    Ok(database)
}

fn count(database: &MetadataDatabase) -> rusqlite::Result<i64> {
    database
        .connection
        .query_row("SELECT count(*) FROM multipart_uploads", [], |row| {
            row.get(0)
        })
}

fn initiator(database: &MetadataDatabase, upload_id: &UploadId) -> rusqlite::Result<String> {
    database.connection.query_row(
        "SELECT initiator_id FROM multipart_uploads WHERE upload_id = ?1",
        [upload_id.to_string()],
        |row| row.get(0),
    )
}

#[test]
fn explicit_checksum_pairs_reject_unsupported_modes_without_defaults() {
    for algorithm in [
        ChecksumAlgorithm::Crc32,
        ChecksumAlgorithm::Crc32c,
        ChecksumAlgorithm::Crc64Nvme,
        ChecksumAlgorithm::Sha1,
        ChecksumAlgorithm::Sha256,
    ] {
        for mode in [ChecksumType::FullObject, ChecksumType::Composite] {
            let supported = !matches!(
                (algorithm, mode),
                (ChecksumAlgorithm::Crc64Nvme, ChecksumType::Composite)
                    | (
                        ChecksumAlgorithm::Sha1 | ChecksumAlgorithm::Sha256,
                        ChecksumType::FullObject
                    )
            );

            let result = MultipartChecksum::new(algorithm, mode);
            assert_eq!(result.is_ok(), supported, "{algorithm:?}/{mode:?}");

            if let Ok(selection) = result {
                assert_eq!(selection.algorithm(), algorithm);
                assert_eq!(selection.checksum_type(), mode);
            }
        }
    }
}

#[test]
fn initiation_keeps_complete_values_and_sortable_original_times_across_reopen() -> TestResult {
    let directory = TestDirectory::new()?.retain_on_failure();
    let mut database = initialized(&directory)?;
    let request = request();
    let owner = database.get_owner_info()?;

    let ids = [
        UploadId::new(EpochMillis::new(1000)?, [8; 16]),
        UploadId::new(EpochMillis::new(1000)?, [9; 16]),
        UploadId::new(EpochMillis::new(-1)?, [10; 16]),
    ];

    for upload_id in &ids {
        let result =
            database.initiate_multipart(&Authority::Native, &request, upload_id.clone())?;

        assert_eq!(&result.upload_id, upload_id);
        assert_eq!(result.checksum, request.checksum);
    }

    close(database)?;

    let reopened = MetadataDatabase::open(
        &directory.path().join("metadata.sqlite3"),
        MetadataOpen::Existing,
    )?;

    assert_eq!(count(&reopened)?, 3);

    let mut statement = reopened.connection.prepare(
        "SELECT upload_id, initiated_at_ms, key, initiator_id, state,
         checksum_algorithm, checksum_type, content_type, content_encoding,
         content_disposition, content_language, cache_control, expires_at_ms,
         user_metadata_json, tags_json, completion_claim_id, completion_blob_id,
         completion_parts_digest FROM multipart_uploads ORDER BY upload_id",
    )?;

    let mut rows = statement.query([])?;

    for expected in [&ids[2], &ids[0], &ids[1]] {
        let row = rows.next()?.unwrap();
        let encoded: String = row.get(0)?;
        let time = EpochMillis::new(row.get(1)?)?;
        assert_eq!(&UploadId::parse(&encoded, time)?, expected);
        assert_eq!(row.get::<_, Vec<u8>>(2)?, request.key.as_bytes());
        assert_eq!(row.get::<_, String>(3)?, owner.canonical_id());

        for (index, value) in [
            (4, "open"),
            (5, "SHA256"),
            (6, "COMPOSITE"),
            (7, "application/example"),
            (8, ""),
            (9, "inline"),
            (10, "en"),
            (11, "max-age=0"),
            (13, r#"{"mixed":"value é"}"#),
            (14, r#"{"topic":"history"}"#),
        ] {
            assert_eq!(row.get::<_, String>(index)?, value);
        }

        assert_eq!(row.get::<_, i64>(12)?, -1);

        for index in 15..18 {
            assert!(row.get::<_, Option<Vec<u8>>>(index)?.is_none());
        }
    }

    assert!(rows.next()?.is_none());
    drop(rows);
    drop(statement);

    let side_effects: i64 = reopened.connection.query_row(
        "SELECT (SELECT count(*) FROM objects) + (SELECT count(*) FROM blobs) + (SELECT count(*) FROM multipart_parts) + (SELECT count(*) FROM multipart_receipts)",
        [],
        |row| row.get(0),
    )?;

    assert_eq!(side_effects, 0);
    close(reopened)?;
    directory.finish()?;
    Ok(())
}

#[test]
fn identity_collision_preserves_both_active_uploads_and_terminal_receipts() -> TestResult {
    let directory = TestDirectory::new()?.retain_on_failure();
    let mut database = initialized(&directory)?;
    let request = request();
    let upload_id = UploadId::new(EpochMillis::new(10)?, [1; 16]);
    database.initiate_multipart(&Authority::Native, &request, upload_id.clone())?;

    for terminal in [false, true] {
        if terminal {
            // Seed an abort receipt to exercise collision with a terminal upload.
            database.connection.execute_batch(
                "INSERT INTO multipart_receipts (upload_id,bucket_id,key,outcome,finished_at_ms) SELECT upload_id,bucket_id,key,'aborted',11 FROM multipart_uploads; DELETE FROM multipart_uploads;",
            )?;
        }

        let error = database
            .initiate_multipart(&Authority::Native, &request, upload_id.clone())
            .unwrap_err();

        assert_eq!(error.failure(), OperationFailure::IoError);
        assert_eq!(error.outcome(), Some(MutationOutcome::NotCommitted));
        assert_eq!(count(&database)?, i64::from(!terminal));

        assert_eq!(
            database.connection.query_row(
                "SELECT count(*) FROM multipart_receipts",
                [],
                |row| row.get::<_, i64>(0)
            )?,
            i64::from(terminal)
        );
    }

    close(database)?;
    directory.finish()?;
    Ok(())
}

#[test]
fn initiation_commit_faults_preserve_rollback_and_unknown_without_replay() -> TestResult {
    use super::super::transaction::Fault;
    let directory = TestDirectory::new()?.retain_on_failure();
    let mut database = initialized(&directory)?;

    let peer = MetadataDatabase::open(
        &directory.path().join("metadata.sqlite3"),
        MetadataOpen::Existing,
    )?;

    for (fault, outcome, expected) in [
        (Fault::BeforeCommit, MutationOutcome::NotCommitted, 0),
        (Fault::AfterCommit, MutationOutcome::Unknown, 1),
    ] {
        database.transaction_fault = Some(fault);

        let error = database
            .create_multipart_upload(&Authority::Native, &request())
            .unwrap_err();

        assert_eq!(error.outcome(), Some(outcome));
        assert_eq!(count(&peer)?, expected);
    }

    close(peer)?;
    close(database)?;

    let reopened = MetadataDatabase::open(
        &directory.path().join("metadata.sqlite3"),
        MetadataOpen::Existing,
    )?;

    assert_eq!(count(&reopened)?, 1);
    close(reopened)?;
    directory.finish()?;
    Ok(())
}

#[test]
fn historical_initiator_survives_key_rotation_deletion_and_label_reuse() -> TestResult {
    let directory = TestDirectory::new()?.retain_on_failure();
    let mut database = initialized(&directory)?;
    let principal = database.create_principal(PrincipalName::new("uploader".into())?)?;

    database.put_principal_policy(
        principal.principal_id,
        r#"{"Version":"2012-10-17","Statement":{"Effect":"Allow","Action":"s3:*","Resource":"*"}}"#,
    )?;

    let expected = principal
        .principal_id
        .arn(database.get_owner_info()?.account_id())?;

    let mut ids = Vec::new();

    for _ in 0..2 {
        let key = database.create_access_key(principal.principal_id)?;

        let identity = Authority::Http(Arc::new(
            database
                .resolve_application_key(key.access_key.access_key_id.as_str())?
                .verified(),
        ));

        let upload = database.create_multipart_upload(&identity, &request())?;
        ids.push(upload.upload_id);
        database.delete_access_key(principal.principal_id, &key.access_key.access_key_id)?;

        assert_eq!(
            database
                .create_multipart_upload(&identity, &request())
                .unwrap_err()
                .failure(),
            OperationFailure::AccessDenied
        );
    }

    database.delete_principal(principal.principal_id)?;
    let replacement = database.create_principal(PrincipalName::new("uploader".into())?)?;
    assert_ne!(replacement.principal_id, principal.principal_id);

    for upload_id in ids {
        assert_eq!(initiator(&database, &upload_id)?, expected);
    }

    assert_eq!(count(&database)?, 2);
    close(database)?;
    directory.finish()?;
    Ok(())
}

#[test]
fn initiation_uses_current_authority_and_unfinished_stage_context() -> TestResult {
    let directory = TestDirectory::new()?.retain_on_failure();
    let mut database = initialized(&directory)?;
    let principal = database.create_principal(PrincipalName::new("uploader".into())?)?;
    let key = database.create_access_key(principal.principal_id)?;

    let identity = Authority::Http(Arc::new(
        database
            .resolve_application_key(key.access_key.access_key_id.as_str())?
            .verified(),
    ));

    let mut request = request();
    request.tags = None;

    assert_eq!(
        database
            .create_multipart_upload(&identity, &request)
            .unwrap_err()
            .failure(),
        OperationFailure::AccessDenied
    );

    database.put_principal_policy(
        principal.principal_id,
        r#"{"Version":"2012-10-17","Statement":{"Effect":"Allow","Action":"s3:PutObject","Resource":"*","Condition":{"Bool":{"s3:ObjectCreationOperation":"false"},"Null":{"s3:if-match":"true","s3:if-none-match":"true","s3:ExistingObjectTag/topic":"true"}}}}"#,
    )?;

    database.create_multipart_upload(&identity, &request)?;
    request.tags = Some(ObjectTags::default());

    assert_eq!(
        database
            .create_multipart_upload(&identity, &request)
            .unwrap_err()
            .failure(),
        OperationFailure::AccessDenied
    );

    database.put_principal_policy(
        principal.principal_id,
        r#"{"Version":"2012-10-17","Statement":[{"Effect":"Allow","Action":["s3:PutObject","s3:PutObjectTagging"],"Resource":"*"},{"Effect":"Deny","Action":"s3:PutObject","Resource":"*"}]}"#,
    )?;

    assert_eq!(
        database
            .create_multipart_upload(&identity, &request)
            .unwrap_err()
            .failure(),
        OperationFailure::AccessDenied
    );

    database.put_principal_policy(
        principal.principal_id,
        r#"{"Version":"2012-10-17","Statement":{"Effect":"Allow","Action":"s3:*","Resource":"*"}}"#,
    )?;

    database.set_principal_enabled(principal.principal_id, false)?;

    assert_eq!(
        database
            .create_multipart_upload(&identity, &request)
            .unwrap_err()
            .failure(),
        OperationFailure::AccessDenied
    );

    let bootstrap = Bootstrap::generate()?;

    let owner_identity = Authority::Http(Arc::new(
        bootstrap
            .resolve(bootstrap.access_key_id())
            .unwrap()
            .verified(),
    ));

    let owner_upload = database.create_multipart_upload(&owner_identity, &request)?;

    assert_eq!(
        initiator(&database, &owner_upload.upload_id)?,
        database.get_owner_info()?.canonical_id()
    );

    let mut other_owner = database.get_owner_info()?.account_id().to_owned();

    other_owner.replace_range(
        ..1,
        if other_owner.starts_with('0') {
            "1"
        } else {
            "0"
        },
    );

    request.expected_owner = Some(other_owner);

    assert_eq!(
        database
            .create_multipart_upload(&Authority::Native, &request)
            .unwrap_err()
            .failure(),
        OperationFailure::AccessDenied
    );

    request.expected_owner = None;

    let bucket = BucketAccessRequest {
        bucket: request.bucket.clone(),
        expected_owner: None,
    };

    database.delete_public_access_block(&Authority::Native, &bucket)?;

    database.put_bucket_policy(
        &Authority::Native,
        &bucket,
        r#"{"Version":"2012-10-17","Statement":{"Effect":"Allow","Principal":"*","Action":"s3:GetObject","Resource":"arn:aws:s3:::attachments/*"}}"#,
    )?;

    assert_eq!(
        database
            .create_multipart_upload(
                &Authority::Http(Arc::new(HttpIdentity::anonymous())),
                &request
            )
            .unwrap_err()
            .failure(),
        OperationFailure::AccessDenied
    );

    assert_eq!(count(&database)?, 2);
    close(database)?;
    directory.finish()?;
    Ok(())
}

#[test]
fn namespace_retirement_targets_the_old_upload_and_preserves_reused_name() -> TestResult {
    let directory = TestDirectory::new()?.retain_on_failure();
    let mut database = initialized(&directory)?;
    let request = request();
    let first = database.create_multipart_upload(&Authority::Native, &request)?;

    let old_bucket: Vec<u8> = database.connection.query_row(
        "SELECT bucket_id FROM multipart_uploads WHERE upload_id = ?1",
        [first.upload_id.to_string()],
        |row| row.get(0),
    )?;

    database.delete_bucket(&request.bucket, None)?;

    assert_eq!(
        database
            .create_multipart_upload(&Authority::Native, &request)
            .unwrap_err()
            .failure(),
        OperationFailure::NoSuchBucket
    );

    database.create_bucket(&request.bucket)?;
    let second = database.create_multipart_upload(&Authority::Native, &request)?;

    database.retire_namespace(
        old_bucket
            .try_into()
            .map_err(|_| "fixture bucket identity")?,
    )?;

    assert_eq!(count(&database)?, 1);

    assert_eq!(
        initiator(&database, &second.upload_id)?,
        database.get_owner_info()?.canonical_id()
    );

    assert_eq!(
        database.connection.query_row(
            "SELECT outcome FROM multipart_receipts WHERE upload_id = ?1",
            [first.upload_id.to_string()],
            |row| row.get::<_, String>(0)
        )?,
        "aborted"
    );

    close(database)?;
    directory.finish()?;
    Ok(())
}

#[test]
fn omitted_negotiation_matches_the_declared_development_contract() -> TestResult {
    #[derive(serde::Deserialize)]
    struct Case {
        algorithm: Option<String>,
        r#type: Option<String>,
        selected_algorithm: String,
        selected_type: String,
    }

    #[derive(serde::Deserialize)]
    struct Fixture {
        negotiation: Vec<Case>,
    }

    let fixture: Fixture = serde_json::from_str(include_str!(
        "../../../../../tests/contract/copy-multipart-public.json"
    ))?;

    for case in fixture.negotiation {
        let algorithm = case
            .algorithm
            .as_deref()
            .map(ChecksumAlgorithm::parse)
            .transpose()?;

        assert!(case.r#type.is_none());

        let selection = MultipartChecksum::resolve(algorithm, None)?;
        assert_eq!(selection.algorithm().as_str(), case.selected_algorithm);

        assert_eq!(
            selection.checksum_type(),
            if case.selected_type == "COMPOSITE" {
                ChecksumType::Composite
            } else {
                ChecksumType::FullObject
            }
        );

        assert_eq!(selection.is_explicit(), algorithm.is_some());
    }

    assert!(MultipartChecksum::resolve(None, Some(ChecksumType::Composite)).is_err());
    Ok(())
}
