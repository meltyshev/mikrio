use mikrio::{
    AbortIncompleteMultipartUpload, CreateBucketInput, DeleteBucketLifecycleInput, ErrorCode,
    GetBucketLifecycleConfigurationInput, LifecycleExpiration, LifecycleFilter, LifecycleRule,
    LifecycleStatus, MutationOutcome, OpenOptions, PutBucketLifecycleConfigurationInput,
};

#[path = "support/stores.rs"]
mod test_stores;

#[path = "../../mikrio-core/tests/support/directory.rs"]
mod test_directory;

use test_directory::TestDirectory;

type TestResult = Result<(), Box<dyn std::error::Error>>;

fn rules() -> Result<Vec<LifecycleRule>, mikrio::StorageError> {
    Ok(vec![
        LifecycleRule::builder(
            LifecycleStatus::Enabled,
            LifecycleFilter::new(Some("photos/雪".into()))?,
        )
        .id("retention-😀")
        .expiration(LifecycleExpiration::new(7)?)
        .abort_incomplete_multipart_upload(AbortIncompleteMultipartUpload::new(1)?)
        .build()?,
        LifecycleRule::builder(LifecycleStatus::Disabled, LifecycleFilter::default())
            .id("disabled")
            .expiration(LifecycleExpiration::new(2_147_483_647)?)
            .build()?,
        LifecycleRule::builder(
            LifecycleStatus::Disabled,
            LifecycleFilter::new(Some(String::new()))?,
        )
        .id("empty-prefix")
        .abort_incomplete_multipart_upload(AbortIncompleteMultipartUpload::new(2)?)
        .build()?,
    ])
}

#[test]
fn lifecycle_configuration_is_owned_ordered_atomic_and_current_across_connections_and_reopen()
-> TestResult {
    let directory = TestDirectory::new()?;

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

        assert_eq!(
            peer.get_bucket_lifecycle_configuration(
                GetBucketLifecycleConfigurationInput::builder("photos").build()?
            )
            .await
            .unwrap_err()
            .code(),
            ErrorCode::NoSuchLifecycleConfiguration
        );

        let expected = rules()?;

        let input =
            PutBucketLifecycleConfigurationInput::builder("photos", expected.clone()).build()?;

        assert_eq!(input.lifecycle_rules(), expected);
        store.put_bucket_lifecycle_configuration(input).await?;

        let detached = peer
            .get_bucket_lifecycle_configuration(
                GetBucketLifecycleConfigurationInput::builder("photos").build()?,
            )
            .await?;

        assert_eq!(detached.lifecycle_rules(), expected);

        let owner = store.get_owner_info().await?;

        store
            .put_bucket_lifecycle_configuration(
                PutBucketLifecycleConfigurationInput::builder("photos", expected.clone())
                    .expected_bucket_owner(owner.account_id())
                    .build()?,
            )
            .await?;

        assert_eq!(
            peer.get_bucket_lifecycle_configuration(
                GetBucketLifecycleConfigurationInput::builder("photos")
                    .expected_bucket_owner(owner.account_id())
                    .build()?,
            )
            .await?
            .lifecycle_rules(),
            expected,
        );

        for (operation, result, outcome) in [
            (
                "put",
                store
                    .put_bucket_lifecycle_configuration(
                        PutBucketLifecycleConfigurationInput::builder("missing", expected.clone())
                            .build()?,
                    )
                    .await
                    .map(drop),
                Some(MutationOutcome::NotCommitted),
            ),
            (
                "get",
                store
                    .get_bucket_lifecycle_configuration(
                        GetBucketLifecycleConfigurationInput::builder("missing").build()?,
                    )
                    .await
                    .map(drop),
                None,
            ),
            (
                "delete",
                store
                    .delete_bucket_lifecycle(
                        DeleteBucketLifecycleInput::builder("missing").build()?,
                    )
                    .await
                    .map(drop),
                Some(MutationOutcome::NotCommitted),
            ),
        ] {
            let error = result.unwrap_err();
            assert_eq!(error.code(), ErrorCode::NoSuchBucket, "{operation}");
            assert_eq!(error.mutation_outcome(), outcome.as_ref(), "{operation}");
        }

        let wrong = if owner.account_id() == "000000000000" {
            "111111111111"
        } else {
            "000000000000"
        };

        let denied = store
            .delete_bucket_lifecycle(
                DeleteBucketLifecycleInput::builder("photos")
                    .expected_bucket_owner(wrong)
                    .build()?,
            )
            .await
            .unwrap_err();

        assert_eq!(denied.code(), ErrorCode::AccessDenied);

        assert_eq!(
            denied.mutation_outcome(),
            Some(&MutationOutcome::NotCommitted)
        );

        assert_eq!(
            peer.get_bucket_lifecycle_configuration(
                GetBucketLifecycleConfigurationInput::builder("photos")
                    .expected_bucket_owner(wrong)
                    .build()?
            )
            .await
            .unwrap_err()
            .code(),
            ErrorCode::AccessDenied
        );

        let denied = store
            .put_bucket_lifecycle_configuration(
                PutBucketLifecycleConfigurationInput::builder("photos", expected.clone())
                    .expected_bucket_owner(wrong)
                    .build()?,
            )
            .await
            .unwrap_err();

        assert_eq!(denied.code(), ErrorCode::AccessDenied);

        assert_eq!(
            denied.mutation_outcome(),
            Some(&MutationOutcome::NotCommitted)
        );

        store.close().await?;
        peer.close().await?;
        let reopened = stores.open(OpenOptions::new(&path)).await?;

        assert_eq!(
            reopened
                .get_bucket_lifecycle_configuration(
                    GetBucketLifecycleConfigurationInput::builder("photos").build()?
                )
                .await?
                .lifecycle_rules(),
            detached.lifecycle_rules()
        );

        for _ in 0..2 {
            reopened
                .delete_bucket_lifecycle(
                    DeleteBucketLifecycleInput::builder("photos")
                        .expected_bucket_owner(owner.account_id())
                        .build()?,
                )
                .await?;
        }

        assert_eq!(
            reopened
                .get_bucket_lifecycle_configuration(
                    GetBucketLifecycleConfigurationInput::builder("photos").build()?
                )
                .await
                .unwrap_err()
                .code(),
            ErrorCode::NoSuchLifecycleConfiguration
        );

        reopened.close().await?;
        assert_eq!(detached.lifecycle_rules(), expected);

        let closed = reopened
            .put_bucket_lifecycle_configuration(
                PutBucketLifecycleConfigurationInput::builder("photos", expected).build()?,
            )
            .await
            .unwrap_err();

        assert_eq!(closed.code(), ErrorCode::StoreClosed);

        assert_eq!(
            closed.mutation_outcome(),
            Some(&MutationOutcome::NotCommitted)
        );

        assert_eq!(
            reopened
                .get_bucket_lifecycle_configuration(
                    GetBucketLifecycleConfigurationInput::builder("photos").build()?
                )
                .await
                .unwrap_err()
                .code(),
            ErrorCode::StoreClosed
        );

        let closed = reopened
            .delete_bucket_lifecycle(DeleteBucketLifecycleInput::builder("photos").build()?)
            .await
            .unwrap_err();

        assert_eq!(closed.code(), ErrorCode::StoreClosed);

        assert_eq!(
            closed.mutation_outcome(),
            Some(&MutationOutcome::NotCommitted)
        );

        Ok::<_, Box<dyn std::error::Error>>(())
    })?;

    directory.finish()?;

    Ok(())
}

#[test]
fn lifecycle_values_validate_complete_rules_and_sets_before_io() -> TestResult {
    for days in [0, 2_147_483_648, u32::MAX] {
        assert_eq!(
            LifecycleExpiration::new(days).unwrap_err().code(),
            ErrorCode::InvalidArgument,
            "days={days}"
        );

        assert_eq!(
            AbortIncompleteMultipartUpload::new(days)
                .unwrap_err()
                .code(),
            ErrorCode::InvalidArgument,
            "days={days}"
        );
    }

    assert_eq!(
        LifecycleRule::builder(LifecycleStatus::Disabled, LifecycleFilter::default())
            .build()
            .unwrap_err()
            .code(),
        ErrorCode::InvalidArgument
    );

    for (case, prefix) in [("oversized", "雪".repeat(342)), ("NUL", "a\0".into())] {
        assert_eq!(
            LifecycleFilter::new(Some(prefix)).unwrap_err().code(),
            ErrorCode::InvalidArgument,
            "{case}"
        );
    }

    assert_eq!(
        PutBucketLifecycleConfigurationInput::builder("photos", vec![])
            .build()
            .unwrap_err()
            .code(),
        ErrorCode::InvalidArgument
    );

    let rule = rules()?.remove(0);

    assert_eq!(
        PutBucketLifecycleConfigurationInput::builder("photos", vec![rule.clone(), rule])
            .build()
            .unwrap_err()
            .code(),
        ErrorCode::InvalidArgument
    );

    let anonymous = LifecycleRule::builder(LifecycleStatus::Disabled, LifecycleFilter::default())
        .expiration(LifecycleExpiration::new(1)?)
        .build()?;

    let maximum =
        PutBucketLifecycleConfigurationInput::builder("photos", vec![anonymous.clone(); 1000])
            .build()?;

    assert_eq!(maximum.lifecycle_rules().len(), 1000);

    assert_eq!(
        PutBucketLifecycleConfigurationInput::builder("photos", vec![anonymous; 1001])
            .build()
            .unwrap_err()
            .code(),
        ErrorCode::InvalidArgument
    );

    Ok(())
}
