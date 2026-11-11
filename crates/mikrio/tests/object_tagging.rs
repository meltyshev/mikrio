use mikrio::{
    CreateBucketInput, DeleteObjectTaggingInput, ErrorCode, GetObjectInput, GetObjectTaggingInput,
    HeadObjectInput, MutationOutcome, OpenOptions, PutObjectInput, PutObjectTaggingInput, Tag,
};

#[path = "support/stores.rs"]
mod test_stores;

#[path = "../../mikrio-core/tests/support/directory.rs"]
mod test_directory;

use test_directory::TestDirectory;

type TestResult = Result<(), Box<dyn std::error::Error>>;

fn tags() -> Vec<Tag> {
    vec![
        Tag {
            key: "日本語".into(),
            value: "𐐀".into(),
        },
        Tag {
            key: "Name".into(),
            value: String::new(),
        },
        Tag {
            key: "name".into(),
            value: "exact".into(),
        },
    ]
}

#[test]
fn tagging_is_detached_atomic_and_preserves_payload_across_connections_and_reopen() -> TestResult {
    let directory = TestDirectory::new()?.retain_on_failure();

    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_time()
        .build()?;

    test_stores::run(&runtime, async |stores| {
        let path = directory.path().join("store");
        let store = stores.open(OpenOptions::new(&path)).await?;
        let peer = stores.open(OpenOptions::new(&path)).await?;

        store
            .create_bucket(CreateBucketInput::builder("photos").build()?)
            .await?;

        store
            .put_object(
                PutObjectInput::builder("photos", "literal/../%2f", b"payload".to_vec())
                    .metadata(vec![("name".into(), "metadata".into())])
                    .build()?,
            )
            .await?;

        let before = peer
            .head_object(HeadObjectInput::builder("photos", "literal/../%2f").build()?)
            .await?;

        assert_eq!(
            store
                .get_object_tagging(
                    GetObjectTaggingInput::builder("photos", "literal/../%2f").build()?
                )
                .await?
                .tags(),
            []
        );

        let input = PutObjectTaggingInput::builder("photos", "literal/../%2f", tags()).build()?;

        let expected = vec![
            Tag {
                key: "Name".into(),
                value: String::new(),
            },
            Tag {
                key: "name".into(),
                value: "exact".into(),
            },
            Tag {
                key: "日本語".into(),
                value: "𐐀".into(),
            },
        ];

        assert_eq!(input.tags(), expected);
        store.put_object_tagging(input).await?;

        let detached = peer
            .get_object_tagging(GetObjectTaggingInput::builder("photos", "literal/../%2f").build()?)
            .await?;

        assert_eq!(detached.tags(), expected);

        let after = peer
            .head_object(HeadObjectInput::builder("photos", "literal/../%2f").build()?)
            .await?;

        assert_eq!(after.etag(), before.etag());
        assert_eq!(after.last_modified(), before.last_modified());
        assert_eq!(after.metadata(), before.metadata());
        assert_eq!(after.tag_count(), 3);

        let owner = store.get_owner_info().await?;

        let wrong = if owner.account_id() == "000000000000" {
            "111111111111"
        } else {
            "000000000000"
        };

        for (operation, result) in ["put", "delete"].into_iter().zip([
            store
                .put_object_tagging(
                    PutObjectTaggingInput::builder("photos", "literal/../%2f", vec![])
                        .expected_bucket_owner(wrong)
                        .build()?,
                )
                .await
                .map(drop),
            store
                .delete_object_tagging(
                    DeleteObjectTaggingInput::builder("photos", "literal/../%2f")
                        .expected_bucket_owner(wrong)
                        .build()?,
                )
                .await
                .map(drop),
        ]) {
            let error = result.unwrap_err();
            assert_eq!(error.code(), ErrorCode::AccessDenied, "{operation}");

            assert_eq!(
                error.mutation_outcome(),
                Some(&MutationOutcome::NotCommitted),
                "{operation}"
            );
        }

        assert_eq!(
            store
                .get_object_tagging(
                    GetObjectTaggingInput::builder("photos", "literal/../%2f")
                        .expected_bucket_owner(wrong)
                        .build()?
                )
                .await
                .unwrap_err()
                .code(),
            ErrorCode::AccessDenied
        );

        for (bucket, key, code) in [
            ("missing", "key", ErrorCode::NoSuchBucket),
            ("photos", "missing", ErrorCode::NoSuchKey),
        ] {
            assert_eq!(
                store
                    .get_object_tagging(GetObjectTaggingInput::builder(bucket, key).build()?)
                    .await
                    .unwrap_err()
                    .code(),
                code
            );

            for (operation, result) in ["put", "delete"].into_iter().zip([
                store
                    .put_object_tagging(
                        PutObjectTaggingInput::builder(bucket, key, tags()).build()?,
                    )
                    .await
                    .map(drop),
                store
                    .delete_object_tagging(DeleteObjectTaggingInput::builder(bucket, key).build()?)
                    .await
                    .map(drop),
            ]) {
                let error = result.unwrap_err();
                assert_eq!(error.code(), code, "{operation}: {bucket}/{key}");

                assert_eq!(
                    error.mutation_outcome(),
                    Some(&MutationOutcome::NotCommitted),
                    "{operation}"
                );
            }
        }

        store.close().await?;
        peer.close().await?;
        let reopened = stores.open(OpenOptions::new(&path)).await?;

        assert_eq!(
            reopened
                .get_object_tagging(
                    GetObjectTaggingInput::builder("photos", "literal/../%2f").build()?
                )
                .await?
                .tags(),
            detached.tags()
        );

        let mut payload = reopened
            .get_object(GetObjectInput::builder("photos", "literal/../%2f").build()?)
            .await?;

        // A retained payload read must not block a metadata-only tag replacement.
        reopened
            .put_object_tagging(
                PutObjectTaggingInput::builder("photos", "literal/../%2f", vec![]).build()?,
            )
            .await?;

        assert_eq!(payload.body_mut().read_all(7).await?, b"payload");

        for _ in 0..2 {
            reopened
                .delete_object_tagging(
                    DeleteObjectTaggingInput::builder("photos", "literal/../%2f").build()?,
                )
                .await?;
        }

        assert_eq!(
            reopened
                .get_object_tagging(
                    GetObjectTaggingInput::builder("photos", "literal/../%2f").build()?
                )
                .await?
                .tags(),
            []
        );

        reopened.close().await?;
        assert_eq!(detached.tags(), expected);

        let error = reopened
            .delete_object_tagging(
                DeleteObjectTaggingInput::builder("photos", "literal/../%2f").build()?,
            )
            .await
            .unwrap_err();

        assert_eq!(error.code(), ErrorCode::StoreClosed);

        assert_eq!(
            error.mutation_outcome(),
            Some(&MutationOutcome::NotCommitted)
        );

        Ok::<_, Box<dyn std::error::Error>>(())
    })?;

    directory.finish()?;
    Ok(())
}

#[test]
fn tagging_builders_reject_invalid_whole_sets_without_io() {
    for (case, invalid) in [
        vec![Tag {
            key: "AWS:reserved".into(),
            value: String::new(),
        }],
        vec![
            Tag {
                key: "same".into(),
                value: "one".into(),
            },
            Tag {
                key: "same".into(),
                value: "two".into(),
            },
        ],
        vec![Tag {
            key: String::new(),
            value: String::new(),
        }],
    ]
    .into_iter()
    .enumerate()
    {
        assert_eq!(
            PutObjectTaggingInput::builder("photos", "key", invalid)
                .build()
                .unwrap_err()
                .code(),
            ErrorCode::InvalidArgument,
            "case {case}"
        );
    }

    for (operation, result) in ["get", "put", "delete"].into_iter().zip([
        GetObjectTaggingInput::builder("photos", "key")
            .expected_bucket_owner("")
            .build()
            .map(drop),
        PutObjectTaggingInput::builder("photos", "key", vec![])
            .expected_bucket_owner("")
            .build()
            .map(drop),
        DeleteObjectTaggingInput::builder("photos", "key")
            .expected_bucket_owner("")
            .build()
            .map(drop),
    ]) {
        assert_eq!(
            result.unwrap_err().code(),
            ErrorCode::InvalidArgument,
            "{operation}"
        );
    }
}
