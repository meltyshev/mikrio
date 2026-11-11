//! External consumer of lifecycle, persistence and owned public values.

use std::error::Error;
use std::path::Path;

use mikrio::{
    ChecksumAlgorithm, ChecksumType, CompleteMultipartUploadInput, CompletedPart, CorsRule,
    CreateAccessKeyInput, CreateBucketInput, CreateMultipartUploadInput, CreatePrincipalInput,
    DeleteAccessKeyInput, DeleteBucketCorsInput, DeleteObjectInput, DeletePrincipalInput,
    ErrorCode, GetBucketCorsInput, GetBucketPolicyInput, GetObjectAclInput,
    GetObjectAttributesInput, GetObjectAttributesOutput, GetObjectInput, GetPrincipalInput,
    GetPrincipalPolicyInput, GetPublicAccessBlockInput, HeadObjectInput, ListBucketsInput,
    ListObjectsV2Input, ObjectAttribute, OpenOptions, PublicAccessBlockConfiguration,
    PutBucketCorsInput, PutBucketPolicyInput, PutObjectInput, PutPrincipalPolicyInput,
    PutPublicAccessBlockInput, Store, Tag, Timestamp, UploadPartInput,
};

#[path = "../crates/mikrio/tests/support/stores.rs"]
mod test_stores;

const PRINCIPAL_POLICY: &str = r#"{"Version":"2012-10-17","Statement":{"Effect":"Allow","Action":"s3:GetObject","Resource":"arn:aws:s3:::attachments/*"}}"#;
const BUCKET_POLICY: &str = r#"{"Version":"2012-10-17","Statement":{"Effect":"Deny","Principal":"*","Action":"s3:*","Resource":"*"}}"#;

const EXPECTED_PART_SHA256: &str = "ungWv48Bz+pBQUDeXa4iI7ADYaOWF3qctBD/YfIAFa0=";

#[allow(
    clippy::print_stdout,
    reason = "standalone consumer reports success to its caller"
)]
fn main() -> Result<(), Box<dyn Error>> {
    let directory = std::env::args_os()
        .nth(1)
        .ok_or("owned store path required")?;

    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()?;

    test_stores::run(&runtime, async |stores| {
        verify_lifecycle(stores, Path::new(&directory)).await
    })?;

    verify_input_values()?;

    println!("independent lifecycle and owned values passed");
    Ok(())
}

#[allow(
    clippy::unwrap_used,
    reason = "standalone consumer asserts required output values"
)]
async fn verify_lifecycle(
    stores: &test_stores::Stores,
    directory: &Path,
) -> Result<(), Box<dyn Error>> {
    let writer = stores.open(OpenOptions::new(directory)).await?;
    let reader = stores.open(OpenOptions::new(directory)).await?;
    let store_id = reader.info().store_id().to_owned();
    assert_eq!(writer.info().store_id(), store_id);

    writer
        .create_bucket(CreateBucketInput::builder("attachments").build()?)
        .await?;

    let owner = writer.get_owner_info().await?;

    let principal = writer
        .create_principal(CreatePrincipalInput::builder("external-consumer").build()?)
        .await?;

    writer
        .put_principal_policy(
            PutPrincipalPolicyInput::builder(principal.principal_id(), PRINCIPAL_POLICY).build()?,
        )
        .await?;

    let stored_policy = reader
        .get_principal_policy(GetPrincipalPolicyInput::builder(principal.principal_id()).build()?)
        .await?;

    assert_eq!(stored_policy.document(), PRINCIPAL_POLICY);

    let access_key = writer
        .create_access_key(CreateAccessKeyInput::builder(principal.principal_id()).build()?)
        .await?;

    let access_block = PublicAccessBlockConfiguration {
        block_public_acls: true,
        ignore_public_acls: false,
        block_public_policy: true,
        restrict_public_buckets: false,
    };

    writer
        .put_public_access_block(
            PutPublicAccessBlockInput::builder("attachments", access_block)
                .expected_bucket_owner(owner.account_id())
                .build()?,
        )
        .await?;

    writer
        .put_bucket_policy(PutBucketPolicyInput::builder("attachments", BUCKET_POLICY).build()?)
        .await?;

    assert_eq!(
        reader
            .get_public_access_block(GetPublicAccessBlockInput::builder("attachments").build()?)
            .await?
            .configuration(),
        access_block
    );

    let cors_rule = CorsRule::builder(vec!["https://app.example".into()], vec!["GET".into()])
        .allowed_headers(vec![])
        .max_age_seconds(i32::MIN)
        .build()?;

    writer
        .put_bucket_cors(
            PutBucketCorsInput::builder("attachments", vec![cors_rule.clone()])
                .expected_bucket_owner(owner.account_id())
                .build()?,
        )
        .await?;

    let cors = reader
        .get_bucket_cors(GetBucketCorsInput::builder("attachments").build()?)
        .await?;

    assert_eq!(cors.cors_rules(), &[cors_rule]);

    #[cfg(feature = "http")]
    {
        let server = writer.listen(mikrio::ListenOptions::default()).await?;
        assert!(server.endpoint().starts_with("http://127.0.0.1:"));
        server.close().await?;
    }

    writer
        .put_object(PutObjectInput::builder("attachments", "literal/key", vec![0, 255, 3]).build()?)
        .await?;

    let mut selected = reader
        .get_object(GetObjectInput::builder("attachments", "literal/key").build()?)
        .await?;

    let acl = reader
        .get_object_acl(GetObjectAclInput::builder("attachments", "literal/key").build()?)
        .await?;

    assert_eq!(acl.owner().canonical_id(), owner.canonical_id());

    writer
        .delete_object(DeleteObjectInput::builder("attachments", "literal/key").build()?)
        .await?;

    writer.close().await?;

    assert_eq!(selected.body_mut().read_all(3).await?, vec![0, 255, 3]);
    selected.body_mut().close().await?;

    assert_eq!(reader.info().store_id(), store_id);
    assert_eq!(acl.grants().len(), 1);

    assert_eq!(
        reader
            .get_principal(GetPrincipalInput::builder(principal.principal_id()).build()?)
            .await?
            .name(),
        "external-consumer"
    );

    reader
        .delete_access_key(
            DeleteAccessKeyInput::builder(
                principal.principal_id(),
                access_key.access_key().access_key_id(),
            )
            .build()?,
        )
        .await?;

    reader
        .delete_principal(DeletePrincipalInput::builder(principal.principal_id()).build()?)
        .await?;

    let attributes = verify_completed_object_layout(&reader).await?;
    reader.close().await?;
    assert_eq!(attributes.object_parts().unwrap().parts()[0].size(), 3);

    let reopened = stores.open(OpenOptions::new(directory)).await?;
    assert_eq!(reopened.info().store_id(), store_id);

    assert_eq!(
        reopened
            .get_bucket_policy(GetBucketPolicyInput::builder("attachments").build()?)
            .await?
            .document(),
        BUCKET_POLICY
    );

    assert_eq!(
        reopened
            .get_public_access_block(GetPublicAccessBlockInput::builder("attachments").build()?)
            .await?
            .configuration(),
        access_block
    );

    assert_eq!(
        reopened
            .get_bucket_cors(GetBucketCorsInput::builder("attachments").build()?)
            .await?,
        cors
    );

    reopened
        .delete_bucket_cors(DeleteBucketCorsInput::builder("attachments").build()?)
        .await?;

    assert_eq!(
        reopened
            .get_bucket_cors(GetBucketCorsInput::builder("attachments").build()?)
            .await
            .unwrap_err()
            .code(),
        ErrorCode::NoSuchCorsConfiguration
    );

    reopened.close().await?;
    Ok(())
}

#[allow(
    clippy::unwrap_used,
    reason = "standalone consumer asserts required builder values"
)]
fn verify_input_values() -> Result<(), Box<dyn Error>> {
    let input = PutObjectInput::builder("photos", "a/../é//%2F", vec![0, 255])
        .content_length(2)
        .metadata(vec![("LABEL".into(), "日本語".into())])
        .tags(vec![Tag {
            key: "𐐀".into(),
            value: "日本語".into(),
        }])
        .checksum_algorithm(ChecksumAlgorithm::Sha256)
        .if_none_match("*")
        .build()?;

    assert_eq!(input.key(), "a/../é//%2F");
    assert_eq!(input.metadata()["label"], "日本語");
    assert_eq!(input.tags().unwrap()[0].key, "𐐀");

    let read_input = GetObjectInput::builder("photos", "key")
        .if_modified_since(Timestamp::from_unix_millis(-1)?)
        .build()?;

    assert_eq!(
        read_input.if_modified_since().unwrap().as_unix_millis(),
        -1000
    );

    let error = PutObjectInput::builder("PHOTOS", "key", Vec::new())
        .build()
        .unwrap_err();

    assert_eq!(error.code(), ErrorCode::InvalidBucketName);
    assert_eq!(error.mutation_outcome(), None);

    let listing = ListObjectsV2Input::builder("photos")
        .prefix("../literal%2F")
        .max_keys(1)
        .build()?;

    assert_eq!(listing.prefix(), Some("../literal%2F"));
    assert_eq!(listing.max_keys(), 1);
    assert_eq!(ListBucketsInput::builder().build()?.max_buckets(), 1000);
    Ok(())
}

#[allow(
    clippy::unwrap_used,
    reason = "standalone consumer asserts required multipart attributes"
)]
async fn verify_completed_object_layout(
    store: &Store,
) -> Result<GetObjectAttributesOutput, Box<dyn Error>> {
    let upload = store
        .create_multipart_upload(
            CreateMultipartUploadInput::builder("attachments", "completed")
                .checksum_algorithm(ChecksumAlgorithm::Sha256)
                .checksum_type(ChecksumType::Composite)
                .build()?,
        )
        .await?;

    let part = store
        .upload_part(
            UploadPartInput::builder(
                "attachments",
                "completed",
                upload.upload_id(),
                1,
                b"abc".to_vec(),
            )
            .build()?,
        )
        .await?;

    assert_eq!(part.checksum_sha256(), Some(EXPECTED_PART_SHA256));

    store
        .complete_multipart_upload(
            CompleteMultipartUploadInput::builder(
                "attachments",
                "completed",
                upload.upload_id(),
                vec![
                    CompletedPart::builder(1, part.etag())
                        .checksum_sha256(EXPECTED_PART_SHA256)
                        .build()?,
                ],
            )
            .build()?,
        )
        .await?;

    let attributes = store
        .get_object_attributes(
            GetObjectAttributesInput::builder(
                "attachments",
                "completed",
                vec![ObjectAttribute::ObjectParts],
            )
            .max_parts(1)
            .build()?,
        )
        .await?;

    assert!(attributes.etag().is_none());

    let parts = attributes.object_parts().unwrap();
    assert_eq!(parts.total_parts_count(), 1);

    assert_eq!(
        parts.parts()[0].checksum_sha256(),
        Some(EXPECTED_PART_SHA256)
    );

    assert!(!parts.is_truncated());
    assert!(parts.next_part_number_marker().is_none());

    let mut selected = store
        .get_object(
            GetObjectInput::builder("attachments", "completed")
                .part_number(1)
                .build()?,
        )
        .await?;

    assert_eq!(selected.content_range(), Some("bytes 0-2/3"));
    assert_eq!(selected.parts_count(), Some(1));
    assert_eq!(selected.body_mut().read_all(3).await?, b"abc");
    selected.body_mut().close().await?;

    let head = store
        .head_object(
            HeadObjectInput::builder("attachments", "completed")
                .part_number(1)
                .build()?,
        )
        .await?;

    assert_eq!(head.content_length(), 3);
    assert_eq!(head.parts_count(), Some(1));
    Ok(attributes)
}
