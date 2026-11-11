use std::sync::Arc;

use super::*;
use crate::object::{Authority, ObjectTags, ReadConditionsInput};

const KEY: &str = "mikrio_a00112233445566778899AABBCCDDEEFF";
type TestResult = Result<(), Box<dyn std::error::Error>>;

fn identity(database: &mut MetadataDatabase) -> Result<Authority, Box<dyn std::error::Error>> {
    database.connection.execute(
        "INSERT INTO principals (principal_id, name, is_enabled, created_at_ms) VALUES (?1, 'app', 1, 0)",
        [[8_u8; 16].as_slice()],
    )?;

    database.connection.execute(
        "INSERT INTO access_keys (access_key_id, principal_id, secret_access_key, created_at_ms)
         VALUES (?1, ?2, 'synthetic-secret', 0)",
        rusqlite::params![KEY, [8_u8; 16].as_slice()],
    )?;

    Ok(Authority::Http(Arc::new(
        database.resolve_application_key(KEY)?.verified(),
    )))
}

fn policy(database: &MetadataDatabase, statement: &serde_json::Value) -> TestResult {
    let document = serde_json::json!({"Version":"2012-10-17", "Statement":statement}).to_string();

    database.connection.execute(
        "UPDATE principals SET policy_generation = ?1, policy_json = ?2",
        rusqlite::params![[9_u8; 16].as_slice(), document],
    )?;

    Ok(())
}

fn allow(database: &MetadataDatabase, actions: &[&str]) -> TestResult {
    policy(
        database,
        &serde_json::json!({"Effect":"Allow", "Action":actions, "Resource":"*"}),
    )
}

fn publish(database: &mut MetadataDatabase, request: &PutRequest) -> TestResult {
    let blob = BlobId::from_bytes([71; 16]);

    let bucket = database
        .reserve_put(&Authority::Native, request, blob)?
        .unwrap();

    database.publish_put(
        &Authority::Native,
        request,
        bucket,
        blob,
        &facts(request, b"original")?,
    )?;

    Ok(())
}

#[test]
fn http_reservation_denies_before_consumption_and_publication_rechecks_revocation() -> TestResult {
    let directory = TestDirectory::new()?;
    let (mut database, request) = setup(&directory)?;
    let authority = identity(&mut database)?;
    let blob = BlobId::from_bytes([72; 16]);

    assert_eq!(
        database
            .reserve_put(&authority, &request, blob)
            .unwrap_err()
            .failure(),
        OperationFailure::AccessDenied
    );

    assert_eq!(
        database
            .connection
            .query_row("SELECT count(*) FROM blobs", [], |row| row.get::<_, i64>(0))?,
        0
    );

    publish(&mut database, &request)?;
    allow(&database, &["s3:PutObject"])?;
    let bucket = database.reserve_put(&authority, &request, blob)?.unwrap();

    database
        .connection
        .execute("DELETE FROM access_keys WHERE access_key_id = ?1", [KEY])?;

    let error = database
        .publish_put(
            &authority,
            &request,
            bucket,
            blob,
            &facts(&request, b"replacement")?,
        )
        .unwrap_err();

    assert_eq!(error.failure(), OperationFailure::AccessDenied);
    assert_eq!(error.outcome(), Some(MutationOutcome::NotCommitted));

    assert_eq!(
        database
            .select_object(&Authority::Native, &read(&request))?
            .blob_id,
        BlobId::from_bytes([71; 16])
    );

    close(database)?;
    directory.finish()?;
    Ok(())
}

#[test]
fn http_publication_rejects_a_signing_secret_corrupted_after_reservation() -> TestResult {
    let directory = TestDirectory::new()?;
    let (mut database, request) = setup(&directory)?;
    let authority = identity(&mut database)?;
    publish(&mut database, &request)?;
    allow(&database, &["s3:PutObject"])?;
    let blob = BlobId::from_bytes([72; 16]);
    let bucket = database.reserve_put(&authority, &request, blob)?.unwrap();

    database.connection.execute_batch(
        "PRAGMA ignore_check_constraints = ON;
         UPDATE access_keys SET secret_access_key = '';",
    )?;

    let error = database
        .publish_put(
            &authority,
            &request,
            bucket,
            blob,
            &facts(&request, b"replacement")?,
        )
        .unwrap_err();

    assert_eq!(error.failure(), OperationFailure::StoreCorrupt);
    assert_eq!(error.outcome(), Some(MutationOutcome::NotCommitted));

    assert_eq!(
        database
            .select_object(&Authority::Native, &read(&request))?
            .blob_id,
        BlobId::from_bytes([71; 16])
    );

    close(database)?;
    directory.finish()?;
    Ok(())
}

#[test]
fn http_put_checks_read_and_tag_permissions_before_condition_disclosure() -> TestResult {
    let directory = TestDirectory::new()?;
    let (mut database, mut request) = setup(&directory)?;
    let authority = identity(&mut database)?;
    publish(&mut database, &request)?;
    allow(&database, &["s3:PutObject"])?;
    request.conditions = WriteConditions::new(Some("*".into()), None)?;

    assert_eq!(
        database
            .reserve_put(&authority, &request, BlobId::from_bytes([73; 16]))
            .unwrap_err()
            .failure(),
        OperationFailure::AccessDenied
    );

    request.conditions = WriteConditions::default();
    request.tags = Some(ObjectTags::default());

    assert_eq!(
        database
            .reserve_put(&authority, &request, BlobId::from_bytes([74; 16]))
            .unwrap_err()
            .failure(),
        OperationFailure::AccessDenied
    );

    request.tags = None;

    assert!(
        database
            .reserve_put(&authority, &request, BlobId::from_bytes([75; 16]))?
            .is_some()
    );

    close(database)?;
    directory.finish()?;
    Ok(())
}

#[test]
fn http_read_uses_current_tags_before_conditions_and_hides_ungranted_counts() -> TestResult {
    let directory = TestDirectory::new()?;
    let (mut database, mut request) = setup(&directory)?;
    let authority = identity(&mut database)?;
    request.tags = Some(ObjectTags::new([("visibility".into(), "public".into())])?);
    publish(&mut database, &request)?;

    policy(
        &database,
        &serde_json::json!({
            "Effect": "Allow",
            "Action": "s3:GetObject",
            "Resource": "*",
            "Condition": {
                "StringEquals": {"s3:ExistingObjectTag/visibility": "public"}
            }
        }),
    )?;

    let selected = database.select_object(&authority, &read(&request))?;
    assert_eq!(selected.tag_count, None);
    let mut conditional = read(&request);

    conditional.conditions = ReadConditions::new(ReadConditionsInput {
        if_none_match: Some(selected.etag),
        ..ReadConditionsInput::default()
    })?;

    assert_eq!(
        database
            .select_object(&authority, &conditional)
            .unwrap_err()
            .failure(),
        OperationFailure::NotModified
    );

    database.replace_object_tags(&read(&request), &ObjectTags::default())?;

    assert_eq!(
        database
            .select_object(&authority, &conditional)
            .unwrap_err()
            .failure(),
        OperationFailure::AccessDenied
    );

    allow(&database, &["s3:GetObject", "s3:GetObjectTagging"])?;

    assert_eq!(
        database
            .select_object(&authority, &read(&request))?
            .tag_count,
        Some(0)
    );

    database.delete_object(&Authority::Native, &read(&request), None)?;

    assert_eq!(
        database
            .select_object(&authority, &read(&request))
            .unwrap_err()
            .failure(),
        OperationFailure::AccessDenied
    );

    allow(&database, &["s3:GetObject", "s3:ListBucket"])?;

    assert_eq!(
        database
            .select_object(&authority, &read(&request))
            .unwrap_err()
            .failure(),
        OperationFailure::NoSuchKey
    );

    close(database)?;
    directory.finish()?;
    Ok(())
}

#[test]
fn http_delete_checks_specific_etag_read_permission_and_keeps_existing_tags_absent() -> TestResult {
    let directory = TestDirectory::new()?;
    let (mut database, mut request) = setup(&directory)?;
    let authority = identity(&mut database)?;
    request.tags = Some(ObjectTags::new([("visibility".into(), "public".into())])?);
    publish(&mut database, &request)?;

    policy(
        &database,
        &serde_json::json!([
            {"Effect":"Allow", "Action":"s3:DeleteObject", "Resource":"*"},
            {
                "Effect": "Deny",
                "Action": "s3:DeleteObject",
                "Resource": "*",
                "Condition": {
                    "StringEquals": {"s3:ExistingObjectTag/visibility": "public"}
                }
            }
        ]),
    )?;

    assert_eq!(
        database
            .delete_object(&authority, &read(&request), Some("\"other\""))
            .unwrap_err()
            .failure(),
        OperationFailure::AccessDenied
    );

    database.delete_object(&authority, &read(&request), Some("*"))?;
    close(database)?;
    directory.finish()?;
    Ok(())
}

#[test]
fn object_acl_uses_its_own_permission_and_current_tags_without_opening_payload() -> TestResult {
    let directory = TestDirectory::new()?;
    let (mut database, mut request) = setup(&directory)?;
    let authority = identity(&mut database)?;
    request.tags = Some(ObjectTags::new([("visibility".into(), "public".into())])?);
    publish(&mut database, &request)?;
    let read = read(&request);
    let owner = database.get_owner_info()?;
    assert_eq!(database.get_object_acl(&Authority::Native, &read)?, owner);

    allow(&database, &["s3:GetObject", "s3:GetBucketAcl"])?;

    assert_eq!(
        database
            .get_object_acl(&authority, &read)
            .unwrap_err()
            .failure(),
        OperationFailure::AccessDenied
    );

    policy(
        &database,
        &serde_json::json!({
            "Effect": "Allow",
            "Action": "s3:GetObjectAcl",
            "Resource": "*",
            "Condition": {
                "StringEquals": {"s3:ExistingObjectTag/visibility": "public"}
            }
        }),
    )?;

    assert_eq!(database.get_object_acl(&authority, &read)?, owner);
    database.replace_object_tags(&read, &ObjectTags::default())?;

    assert_eq!(
        database
            .get_object_acl(&authority, &read)
            .unwrap_err()
            .failure(),
        OperationFailure::AccessDenied
    );

    allow(&database, &["s3:GetObjectAcl"])?;
    assert_eq!(database.get_object_acl(&authority, &read)?, owner);
    database.delete_object(&Authority::Native, &read, None)?;

    assert_eq!(
        database
            .get_object_acl(&Authority::Native, &read)
            .unwrap_err()
            .failure(),
        OperationFailure::NoSuchKey
    );

    assert_eq!(
        database
            .get_object_acl(&authority, &read)
            .unwrap_err()
            .failure(),
        OperationFailure::AccessDenied
    );

    allow(&database, &["s3:GetObjectAcl", "s3:ListBucket"])?;

    assert_eq!(
        database
            .get_object_acl(&authority, &read)
            .unwrap_err()
            .failure(),
        OperationFailure::NoSuchKey
    );

    close(database)?;
    directory.finish()?;
    Ok(())
}
