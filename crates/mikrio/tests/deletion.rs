use mikrio::{
    CreateBucketInput, DeleteObjectsInput, ErrorCode, HeadObjectInput, MutationOutcome,
    ObjectIdentifier, OpenOptions, PutObjectInput,
};

#[path = "support/stores.rs"]
mod test_stores;

#[path = "../../mikrio-core/tests/support/directory.rs"]
mod test_directory;

use test_directory::TestDirectory;

#[test]
fn batch_outputs_preserve_duplicate_conditions_quiet_errors_and_detachment()
-> Result<(), Box<dyn std::error::Error>> {
    let directory = TestDirectory::new()?;

    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_time()
        .build()?;

    test_stores::run(&runtime, async |stores| {
        let store = stores
            .open(OpenOptions::new(directory.path().join("store")))
            .await?;

        store
            .create_bucket(CreateBucketInput::builder("bucket").build()?)
            .await?;

        let put = store
            .put_object(PutObjectInput::builder("bucket", "same", b"payload".to_vec()).build()?)
            .await?;

        let entries = vec![
            ObjectIdentifier::builder("same")
                .etag("\"wrong\"")
                .build()?,
            ObjectIdentifier::builder("same").etag(put.etag()).build()?,
            ObjectIdentifier::builder("same").etag(put.etag()).build()?,
            ObjectIdentifier::builder("same").build()?,
        ];

        let output = store
            .delete_objects(DeleteObjectsInput::builder("bucket", entries).build()?)
            .await?;

        assert_eq!(
            output
                .deleted()
                .iter()
                .map(mikrio::DeletedObject::key)
                .collect::<Vec<_>>(),
            ["same", "same"]
        );

        assert_eq!(
            output
                .errors()
                .iter()
                .map(mikrio::DeleteObjectError::code)
                .collect::<Vec<_>>(),
            [ErrorCode::PreconditionFailed, ErrorCode::NoSuchKey]
        );

        assert_eq!(
            store
                .head_object(HeadObjectInput::builder("bucket", "same").build()?)
                .await
                .unwrap_err()
                .code(),
            ErrorCode::NoSuchKey
        );

        let quiet = store
            .delete_objects(
                DeleteObjectsInput::builder(
                    "bucket",
                    vec![
                        ObjectIdentifier::builder("missing").build()?,
                        ObjectIdentifier::builder("missing").etag("*").build()?,
                    ],
                )
                .quiet(true)
                .build()?,
            )
            .await?;

        assert_eq!(quiet.deleted(), []);
        assert_eq!(quiet.errors()[0].code(), ErrorCode::NoSuchKey);

        let denied = store
            .delete_objects(
                DeleteObjectsInput::builder(
                    "bucket",
                    vec![ObjectIdentifier::builder("missing").build()?],
                )
                .expected_bucket_owner("999999999999")
                .build()?,
            )
            .await
            .unwrap_err();

        assert_eq!(denied.code(), ErrorCode::AccessDenied);

        assert_eq!(
            denied.mutation_outcome(),
            Some(&MutationOutcome::NotCommitted)
        );

        assert!(denied.partial_result().is_none());
        store.close().await?;
        assert_eq!(output.errors()[0].key(), "same");
        assert_ne!(output.errors()[0].message(), "");
        assert_eq!(quiet.errors()[0].key(), "missing");
        Ok::<_, Box<dyn std::error::Error>>(())
    })?;

    directory.finish()?;
    Ok(())
}
