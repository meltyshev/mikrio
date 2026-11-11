use mikrio::{
    AbortMultipartUploadInput, CompleteMultipartUploadInput, CompletedPart, CreateAccessKeyInput,
    CreateBucketInput, CreateMultipartUploadInput, CreatePrincipalInput, DeleteAccessKeyInput,
    DeleteObjectInput, DeletePrincipalInput, DeletePrincipalPolicyInput, ErrorCode,
    GetBucketLocationInput, GetObjectAttributesInput, GetObjectInput, GetPrincipalInput,
    GetPrincipalPolicyInput, ListAccessKeysInput, ListMultipartUploadsInput, ListPartsInput,
    ListPrincipalsInput, MutationOutcome, ObjectAttribute, ObjectRef, OpenOptions, PutObjectInput,
    PutPrincipalPolicyInput, SetPrincipalEnabledInput, StorageError, UploadPartCopyInput,
    UploadPartInput,
};

#[path = "support/stores.rs"]
mod test_stores;

#[path = "../../mikrio-core/tests/support/directory.rs"]
mod test_directory;

use test_directory::TestDirectory;

type TestResult = Result<(), Box<dyn std::error::Error>>;

const POLICY: &str = r#"{"Version":"2012-10-17","Statement":{"Effect":"Allow","Action":"s3:GetObject","Resource":"arn:aws:s3:::photos/*"}}"#;

fn rejected(error: &StorageError, code: ErrorCode, mutation: bool) {
    assert_eq!(error.code(), code);

    let outcome = mutation.then_some(MutationOutcome::NotCommitted);
    assert_eq!(error.mutation_outcome(), outcome.as_ref());
    assert!(error.partial_result().is_none());
}

#[test]
fn administration_rejections_preserve_principals_keys_and_policy() -> TestResult {
    let directory = TestDirectory::new()?.retain_on_failure();

    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_time()
        .build()?;

    test_stores::run(&runtime, async |stores| {
        let path = directory.path().join("store");
        let store = stores.open(OpenOptions::new(&path)).await?;

        let principal = store
            .create_principal(CreatePrincipalInput::builder("application").build()?)
            .await?;

        let principal_id = principal.principal_id();

        store
            .put_principal_policy(PutPrincipalPolicyInput::builder(principal_id, POLICY).build()?)
            .await?;

        let key = store
            .create_access_key(CreateAccessKeyInput::builder(principal_id).build()?)
            .await?;

        rejected(
            &store
                .create_principal(CreatePrincipalInput::builder("application").build()?)
                .await
                .unwrap_err(),
            ErrorCode::PrincipalAlreadyExists,
            true,
        );

        let removed = store
            .create_principal(CreatePrincipalInput::builder("removed").build()?)
            .await?;

        let missing = removed.principal_id();

        store
            .delete_principal(DeletePrincipalInput::builder(missing).build()?)
            .await?;

        rejected(
            &store
                .get_principal(GetPrincipalInput::builder(missing).build()?)
                .await
                .unwrap_err(),
            ErrorCode::NoSuchPrincipal,
            false,
        );

        rejected(
            &store
                .set_principal_enabled(SetPrincipalEnabledInput::builder(missing, false).build()?)
                .await
                .unwrap_err(),
            ErrorCode::NoSuchPrincipal,
            true,
        );

        rejected(
            &store
                .put_principal_policy(PutPrincipalPolicyInput::builder(missing, POLICY).build()?)
                .await
                .unwrap_err(),
            ErrorCode::NoSuchPrincipal,
            true,
        );

        rejected(
            &store
                .get_principal_policy(GetPrincipalPolicyInput::builder(missing).build()?)
                .await
                .unwrap_err(),
            ErrorCode::NoSuchPrincipal,
            false,
        );

        rejected(
            &store
                .delete_principal_policy(DeletePrincipalPolicyInput::builder(missing).build()?)
                .await
                .unwrap_err(),
            ErrorCode::NoSuchPrincipal,
            true,
        );

        rejected(
            &store
                .create_access_key(CreateAccessKeyInput::builder(missing).build()?)
                .await
                .unwrap_err(),
            ErrorCode::NoSuchPrincipal,
            true,
        );

        rejected(
            &store
                .list_access_keys(ListAccessKeysInput::builder(missing).build()?)
                .await
                .unwrap_err(),
            ErrorCode::NoSuchPrincipal,
            false,
        );

        rejected(
            &store
                .delete_access_key(
                    DeleteAccessKeyInput::builder(missing, key.access_key().access_key_id())
                        .build()?,
                )
                .await
                .unwrap_err(),
            ErrorCode::NoSuchPrincipal,
            true,
        );

        rejected(
            &store
                .delete_principal(DeletePrincipalInput::builder(principal_id).build()?)
                .await
                .unwrap_err(),
            ErrorCode::PrincipalHasAccessKeys,
            true,
        );

        rejected(
            &store
                .list_principals(
                    ListPrincipalsInput::builder()
                        .continuation_token("invalid")
                        .build()?,
                )
                .await
                .unwrap_err(),
            ErrorCode::InvalidArgument,
            false,
        );

        store.close().await?;

        rejected(
            &store.get_owner_info().await.unwrap_err(),
            ErrorCode::StoreClosed,
            false,
        );

        let reopened = stores.open(OpenOptions::new(path)).await?;

        assert_eq!(
            reopened
                .get_principal(GetPrincipalInput::builder(principal_id).build()?)
                .await?,
            principal
        );

        assert_eq!(
            reopened
                .get_principal_policy(GetPrincipalPolicyInput::builder(principal_id).build()?)
                .await?
                .document(),
            POLICY
        );

        assert_eq!(
            reopened
                .list_access_keys(ListAccessKeysInput::builder(principal_id).build()?)
                .await?
                .access_keys(),
            &[key.access_key().clone()]
        );

        reopened.close().await?;
        Ok::<_, Box<dyn std::error::Error>>(())
    })?;

    directory.finish()?;
    Ok(())
}

#[test]
fn object_rejections_preserve_acknowledged_bytes_after_reopen() -> TestResult {
    let directory = TestDirectory::new()?.retain_on_failure();

    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_time()
        .build()?;

    test_stores::run(&runtime, async |stores| {
        let path = directory.path().join("store");
        let store = stores.open(OpenOptions::new(&path)).await?;

        store
            .create_bucket(CreateBucketInput::builder("photos").build()?)
            .await?;

        store
            .put_object(PutObjectInput::builder("photos", "kept", b"original".to_vec()).build()?)
            .await?;

        rejected(
            &store
                .put_object(
                    PutObjectInput::builder("photos", "kept", b"replacement".to_vec())
                        .if_none_match("*")
                        .build()?,
                )
                .await
                .unwrap_err(),
            ErrorCode::PreconditionFailed,
            true,
        );

        rejected(
            &store
                .delete_object(
                    DeleteObjectInput::builder("photos", "kept")
                        .if_match("\"wrong\"")
                        .build()?,
                )
                .await
                .unwrap_err(),
            ErrorCode::PreconditionFailed,
            true,
        );

        rejected(
            &store
                .get_bucket_location(GetBucketLocationInput::builder("missing").build()?)
                .await
                .unwrap_err(),
            ErrorCode::NoSuchBucket,
            false,
        );

        rejected(
            &store
                .get_object(GetObjectInput::builder("photos", "missing").build()?)
                .await
                .unwrap_err(),
            ErrorCode::NoSuchKey,
            false,
        );

        rejected(
            &store
                .get_object_attributes(
                    GetObjectAttributesInput::builder(
                        "photos",
                        "missing",
                        vec![ObjectAttribute::Etag],
                    )
                    .build()?,
                )
                .await
                .unwrap_err(),
            ErrorCode::NoSuchKey,
            false,
        );

        store.close().await?;

        let reopened = stores.open(OpenOptions::new(path)).await?;
        let mut object = reopened
            .get_object(GetObjectInput::builder("photos", "kept").build()?)
            .await?;

        assert_eq!(object.body_mut().read_all(8).await?, b"original");
        object.body_mut().close().await?;
        reopened.close().await?;
        Ok::<_, Box<dyn std::error::Error>>(())
    })?;

    directory.finish()?;
    Ok(())
}

#[test]
fn multipart_rejections_preserve_parts_and_destination_before_successful_completion() -> TestResult
{
    let directory = TestDirectory::new()?.retain_on_failure();

    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_time()
        .build()?;

    test_stores::run(&runtime, async |stores| {
        let path = directory.path().join("store");
        let store = stores.open(OpenOptions::new(&path)).await?;

        store
            .create_bucket(CreateBucketInput::builder("photos").build()?)
            .await?;

        store
            .put_object(
                PutObjectInput::builder("photos", "destination", b"previous".to_vec()).build()?,
            )
            .await?;

        rejected(
            &store
                .create_multipart_upload(
                    CreateMultipartUploadInput::builder("missing", "destination").build()?,
                )
                .await
                .unwrap_err(),
            ErrorCode::NoSuchBucket,
            true,
        );

        rejected(
            &store
                .list_multipart_uploads(ListMultipartUploadsInput::builder("missing").build()?)
                .await
                .unwrap_err(),
            ErrorCode::NoSuchBucket,
            false,
        );

        let upload = store
            .create_multipart_upload(
                CreateMultipartUploadInput::builder("photos", "destination").build()?,
            )
            .await?;

        let part = store
            .upload_part(
                UploadPartInput::builder(
                    "photos",
                    "destination",
                    upload.upload_id(),
                    1,
                    b"abc".to_vec(),
                )
                .build()?,
            )
            .await?;

        rejected(
            &store
                .upload_part_copy(
                    UploadPartCopyInput::builder(
                        "photos",
                        "destination",
                        upload.upload_id(),
                        1,
                        ObjectRef::new("photos", "missing")?,
                    )
                    .build()?,
                )
                .await
                .unwrap_err(),
            ErrorCode::NoSuchKey,
            true,
        );

        rejected(
            &store
                .complete_multipart_upload(
                    CompleteMultipartUploadInput::builder(
                        "photos",
                        "destination",
                        upload.upload_id(),
                        vec![
                            CompletedPart::builder(1, "\"00000000000000000000000000000000\"")
                                .build()?,
                        ],
                    )
                    .build()?,
                )
                .await
                .unwrap_err(),
            ErrorCode::InvalidPart,
            true,
        );

        let parts = store
            .list_parts(
                ListPartsInput::builder("photos", "destination", upload.upload_id()).build()?,
            )
            .await?;

        assert_eq!(parts.parts().len(), 1);
        assert_eq!(parts.parts()[0].etag(), part.etag());

        let mut previous = store
            .get_object(GetObjectInput::builder("photos", "destination").build()?)
            .await?;

        assert_eq!(previous.body_mut().read_all(8).await?, b"previous");
        previous.body_mut().close().await?;

        store
            .complete_multipart_upload(
                CompleteMultipartUploadInput::builder(
                    "photos",
                    "destination",
                    upload.upload_id(),
                    vec![CompletedPart::builder(1, part.etag()).build()?],
                )
                .build()?,
            )
            .await?;

        rejected(
            &store
                .abort_multipart_upload(
                    AbortMultipartUploadInput::builder("photos", "destination", upload.upload_id())
                        .build()?,
                )
                .await
                .unwrap_err(),
            ErrorCode::NoSuchUpload,
            true,
        );

        store.close().await?;

        let reopened = stores.open(OpenOptions::new(path)).await?;
        let mut completed = reopened
            .get_object(GetObjectInput::builder("photos", "destination").build()?)
            .await?;

        assert_eq!(completed.body_mut().read_all(3).await?, b"abc");
        completed.body_mut().close().await?;
        reopened.close().await?;
        Ok::<_, Box<dyn std::error::Error>>(())
    })?;

    directory.finish()?;
    Ok(())
}
