use mikrio::{
    CorsRule, CreateBucketInput, DeleteBucketCorsInput, ErrorCode, GetBucketCorsInput,
    MutationOutcome, OpenOptions, PutBucketCorsInput,
};

#[path = "support/stores.rs"]
mod test_stores;

#[path = "../../mikrio-core/tests/support/directory.rs"]
mod test_directory;

use test_directory::TestDirectory;

type TestResult = Result<(), Box<dyn std::error::Error>>;

fn rules() -> Result<Vec<CorsRule>, mikrio::StorageError> {
    Ok(vec![
        CorsRule::builder(
            vec!["https://app.example".into()],
            vec!["PUT".into(), "GET".into()],
        )
        .id("browser-😀")
        .allowed_headers(vec![])
        .expose_headers(vec!["ETag".into()])
        .max_age_seconds(0)
        .build()?,
        CorsRule::builder(vec!["*".into()], vec!["HEAD".into()]).build()?,
    ])
}

#[test]
fn cors_is_owned_ordered_atomic_and_current_across_connections_and_reopen() -> TestResult {
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
            peer.get_bucket_cors(GetBucketCorsInput::builder("photos").build()?)
                .await
                .unwrap_err()
                .code(),
            ErrorCode::NoSuchCorsConfiguration
        );

        let expected = rules()?;
        let input = PutBucketCorsInput::builder("photos", expected.clone()).build()?;
        assert_eq!(input.cors_rules(), expected);
        store.put_bucket_cors(input).await?;

        let detached = peer
            .get_bucket_cors(GetBucketCorsInput::builder("photos").build()?)
            .await?;

        assert_eq!(detached.cors_rules(), expected);

        let owner = store.get_owner_info().await?;

        store
            .put_bucket_cors(
                PutBucketCorsInput::builder("photos", expected.clone())
                    .expected_bucket_owner(owner.account_id())
                    .build()?,
            )
            .await?;

        assert_eq!(
            peer.get_bucket_cors(
                GetBucketCorsInput::builder("photos")
                    .expected_bucket_owner(owner.account_id())
                    .build()?,
            )
            .await?
            .cors_rules(),
            expected,
        );

        for (operation, result, outcome) in [
            (
                "put",
                store
                    .put_bucket_cors(
                        PutBucketCorsInput::builder("missing", expected.clone()).build()?,
                    )
                    .await
                    .map(drop),
                Some(MutationOutcome::NotCommitted),
            ),
            (
                "get",
                store
                    .get_bucket_cors(GetBucketCorsInput::builder("missing").build()?)
                    .await
                    .map(drop),
                None,
            ),
            (
                "delete",
                store
                    .delete_bucket_cors(DeleteBucketCorsInput::builder("missing").build()?)
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
            .delete_bucket_cors(
                DeleteBucketCorsInput::builder("photos")
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
            peer.get_bucket_cors(
                GetBucketCorsInput::builder("photos")
                    .expected_bucket_owner(wrong)
                    .build()?
            )
            .await
            .unwrap_err()
            .code(),
            ErrorCode::AccessDenied
        );

        let denied = store
            .put_bucket_cors(
                PutBucketCorsInput::builder("photos", expected.clone())
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
                .get_bucket_cors(GetBucketCorsInput::builder("photos").build()?)
                .await?
                .cors_rules(),
            detached.cors_rules()
        );

        for _ in 0..2 {
            reopened
                .delete_bucket_cors(
                    DeleteBucketCorsInput::builder("photos")
                        .expected_bucket_owner(owner.account_id())
                        .build()?,
                )
                .await?;
        }

        assert_eq!(
            reopened
                .get_bucket_cors(GetBucketCorsInput::builder("photos").build()?)
                .await
                .unwrap_err()
                .code(),
            ErrorCode::NoSuchCorsConfiguration
        );

        reopened.close().await?;
        assert_eq!(detached.cors_rules(), expected);

        let closed = reopened
            .put_bucket_cors(PutBucketCorsInput::builder("photos", expected).build()?)
            .await
            .unwrap_err();

        assert_eq!(closed.code(), ErrorCode::StoreClosed);

        assert_eq!(
            closed.mutation_outcome(),
            Some(&MutationOutcome::NotCommitted)
        );

        assert_eq!(
            reopened
                .get_bucket_cors(GetBucketCorsInput::builder("photos").build()?)
                .await
                .unwrap_err()
                .code(),
            ErrorCode::StoreClosed
        );

        let closed = reopened
            .delete_bucket_cors(DeleteBucketCorsInput::builder("photos").build()?)
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
fn cors_builders_reject_whole_invalid_configurations_without_io() -> TestResult {
    for (operation, result) in ["put", "get", "delete"].into_iter().zip([
        PutBucketCorsInput::builder("photos", rules()?)
            .expected_bucket_owner("")
            .build()
            .map(drop),
        GetBucketCorsInput::builder("photos")
            .expected_bucket_owner("")
            .build()
            .map(drop),
        DeleteBucketCorsInput::builder("photos")
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

    assert_eq!(
        PutBucketCorsInput::builder("photos", vec![])
            .build()
            .unwrap_err()
            .code(),
        ErrorCode::InvalidArgument
    );

    let rule = rules()?.remove(0);

    assert_eq!(
        PutBucketCorsInput::builder("photos", vec![rule.clone(), rule])
            .build()
            .unwrap_err()
            .code(),
        ErrorCode::InvalidArgument
    );

    for method in ["get", "OPTIONS", "PATCH", ""] {
        assert_eq!(
            CorsRule::builder(vec!["*".into()], vec![method.into()])
                .build()
                .unwrap_err()
                .code(),
            ErrorCode::InvalidArgument,
            "method={method:?}"
        );
    }

    assert_eq!(
        CorsRule::builder(vec!["*".into()], vec!["GET".into()])
            .max_age_seconds(i32::MIN)
            .build()?
            .max_age_seconds(),
        Some(i32::MIN)
    );

    Ok(())
}
