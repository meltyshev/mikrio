use mikrio::{
    AclPermission, CreateBucketInput, DeleteBucketInput, DeleteBucketPolicyInput,
    DeleteObjectInput, DeletePublicAccessBlockInput, ErrorCode, GetBucketAclInput,
    GetBucketOwnershipControlsInput, GetBucketPolicyInput, GetBucketPolicyStatusInput,
    GetObjectAclInput, GetPublicAccessBlockInput, MutationOutcome, ObjectOwnership, OpenOptions,
    PublicAccessBlockConfiguration, PutBucketOwnershipControlsInput, PutBucketPolicyInput,
    PutObjectInput, PutPublicAccessBlockInput,
};

#[path = "support/stores.rs"]
mod test_stores;

#[path = "../../mikrio-core/tests/support/directory.rs"]
mod test_directory;

use test_directory::TestDirectory;

type TestResult = Result<(), Box<dyn std::error::Error>>;

const PUBLIC: &str = r#"{ "Version":"2012-10-17", "Statement":{"Effect":"Allow","Principal":"*","Action":"s3:GetObject","Resource":"arn:aws:s3:::photos/*"}}"#;

#[test]
fn every_configuration_operation_checks_owner_and_closed_connection() -> TestResult {
    let directory = TestDirectory::new()?;

    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_time()
        .build()?;

    test_stores::run(&runtime, async |stores| {
        let store = stores
            .open(OpenOptions::new(directory.path().join("store")))
            .await?;

        store
            .create_bucket(CreateBucketInput::builder("photos").build()?)
            .await?;

        store
            .put_object(PutObjectInput::builder("photos", "object", vec![1]).build()?)
            .await?;

        let owner = store.get_owner_info().await?;

        let wrong = if owner.account_id() == "000000000000" {
            "111111111111"
        } else {
            "000000000000"
        };

        for is_closed in [false, true] {
            if is_closed {
                store.close().await?;
            }

            let expected = if is_closed { owner.account_id() } else { wrong };

            let code = if is_closed {
                ErrorCode::StoreClosed
            } else {
                ErrorCode::AccessDenied
            };

            let error = store
                .put_bucket_policy(
                    PutBucketPolicyInput::builder("photos", PUBLIC)
                        .expected_bucket_owner(expected)
                        .build()?,
                )
                .await
                .unwrap_err();

            assert_eq!(error.code(), code, "put_bucket_policy");

            assert_eq!(
                error.mutation_outcome(),
                Some(&MutationOutcome::NotCommitted),
                "put_bucket_policy"
            );

            let error = store
                .get_bucket_policy(
                    GetBucketPolicyInput::builder("photos")
                        .expected_bucket_owner(expected)
                        .build()?,
                )
                .await
                .unwrap_err();

            assert_eq!(error.code(), code, "get_bucket_policy");
            assert_eq!(error.mutation_outcome(), None, "get_bucket_policy");

            let error = store
                .delete_bucket_policy(
                    DeleteBucketPolicyInput::builder("photos")
                        .expected_bucket_owner(expected)
                        .build()?,
                )
                .await
                .unwrap_err();

            assert_eq!(error.code(), code, "delete_bucket_policy");

            assert_eq!(
                error.mutation_outcome(),
                Some(&MutationOutcome::NotCommitted),
                "delete_bucket_policy"
            );

            let error = store
                .get_bucket_policy_status(
                    GetBucketPolicyStatusInput::builder("photos")
                        .expected_bucket_owner(expected)
                        .build()?,
                )
                .await
                .unwrap_err();

            assert_eq!(error.code(), code, "get_bucket_policy_status");
            assert_eq!(error.mutation_outcome(), None, "get_bucket_policy_status");

            let error = store
                .put_public_access_block(
                    PutPublicAccessBlockInput::builder("photos", flags(0))
                        .expected_bucket_owner(expected)
                        .build()?,
                )
                .await
                .unwrap_err();

            assert_eq!(error.code(), code, "put_public_access_block");

            assert_eq!(
                error.mutation_outcome(),
                Some(&MutationOutcome::NotCommitted),
                "put_public_access_block"
            );

            let error = store
                .get_public_access_block(
                    GetPublicAccessBlockInput::builder("photos")
                        .expected_bucket_owner(expected)
                        .build()?,
                )
                .await
                .unwrap_err();

            assert_eq!(error.code(), code, "get_public_access_block");
            assert_eq!(error.mutation_outcome(), None, "get_public_access_block");

            let error = store
                .delete_public_access_block(
                    DeletePublicAccessBlockInput::builder("photos")
                        .expected_bucket_owner(expected)
                        .build()?,
                )
                .await
                .unwrap_err();

            assert_eq!(error.code(), code, "delete_public_access_block");

            assert_eq!(
                error.mutation_outcome(),
                Some(&MutationOutcome::NotCommitted),
                "delete_public_access_block"
            );

            let error = store
                .put_bucket_ownership_controls(
                    PutBucketOwnershipControlsInput::builder(
                        "photos",
                        ObjectOwnership::BucketOwnerEnforced,
                    )
                    .expected_bucket_owner(expected)
                    .build()?,
                )
                .await
                .unwrap_err();

            assert_eq!(error.code(), code, "put_bucket_ownership_controls");

            assert_eq!(
                error.mutation_outcome(),
                Some(&MutationOutcome::NotCommitted),
                "put_bucket_ownership_controls"
            );

            let error = store
                .get_bucket_ownership_controls(
                    GetBucketOwnershipControlsInput::builder("photos")
                        .expected_bucket_owner(expected)
                        .build()?,
                )
                .await
                .unwrap_err();

            assert_eq!(error.code(), code, "get_bucket_ownership_controls");

            assert_eq!(
                error.mutation_outcome(),
                None,
                "get_bucket_ownership_controls"
            );

            let error = store
                .get_bucket_acl(
                    GetBucketAclInput::builder("photos")
                        .expected_bucket_owner(expected)
                        .build()?,
                )
                .await
                .unwrap_err();

            assert_eq!(error.code(), code, "get_bucket_acl");
            assert_eq!(error.mutation_outcome(), None, "get_bucket_acl");

            let error = store
                .get_object_acl(
                    GetObjectAclInput::builder("photos", "object")
                        .expected_bucket_owner(expected)
                        .build()?,
                )
                .await
                .unwrap_err();

            assert_eq!(error.code(), code, "get_object_acl");
            assert_eq!(error.mutation_outcome(), None, "get_object_acl");
        }

        Ok::<_, Box<dyn std::error::Error>>(())
    })?;

    directory.finish()?;
    Ok(())
}

fn flags(bits: u8) -> PublicAccessBlockConfiguration {
    PublicAccessBlockConfiguration {
        block_public_acls: bits & 1 != 0,
        ignore_public_acls: bits & 2 != 0,
        block_public_policy: bits & 4 != 0,
        restrict_public_buckets: bits & 8 != 0,
    }
}

#[test]
fn configuration_is_shared_detached_and_preserved_across_reopen() -> TestResult {
    let directory = TestDirectory::new()?;

    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_time()
        .build()?;

    test_stores::run(&runtime, async |stores| {
        let path = directory.path().join("store");
        let first = stores.open(OpenOptions::new(&path)).await?;
        let second = stores.open(OpenOptions::new(&path)).await?;

        first
            .create_bucket(CreateBucketInput::builder("photos").build()?)
            .await?;

        let owner = first.get_owner_info().await?;

        assert_eq!(
            second
                .get_public_access_block(GetPublicAccessBlockInput::builder("photos").build()?)
                .await?
                .configuration(),
            flags(15)
        );

        assert_eq!(
            second
                .get_bucket_policy(GetBucketPolicyInput::builder("photos").build()?)
                .await
                .unwrap_err()
                .code(),
            ErrorCode::NoSuchBucketPolicy
        );

        let blocked = first
            .put_bucket_policy(PutBucketPolicyInput::builder("photos", PUBLIC).build()?)
            .await
            .unwrap_err();

        assert_eq!(blocked.code(), ErrorCode::AccessDenied);

        assert_eq!(
            blocked.mutation_outcome(),
            Some(&MutationOutcome::NotCommitted)
        );

        first
            .delete_public_access_block(DeletePublicAccessBlockInput::builder("photos").build()?)
            .await?;

        first
            .put_bucket_policy(
                PutBucketPolicyInput::builder("photos", PUBLIC)
                    .expected_bucket_owner(owner.account_id())
                    .build()?,
            )
            .await?;

        first
            .put_public_access_block(
                PutPublicAccessBlockInput::builder("photos", flags(15)).build()?,
            )
            .await?;

        let policy = second
            .get_bucket_policy(GetBucketPolicyInput::builder("photos").build()?)
            .await?;

        assert_eq!(policy.document(), PUBLIC);
        assert!(!format!("{policy:?}").contains("photos"));

        assert!(
            second
                .get_bucket_policy_status(GetBucketPolicyStatusInput::builder("photos").build()?)
                .await?
                .is_public()
        );

        for bits in 0..16 {
            first
                .put_public_access_block(
                    PutPublicAccessBlockInput::builder("photos", flags(bits)).build()?,
                )
                .await?;

            assert_eq!(
                second
                    .get_public_access_block(GetPublicAccessBlockInput::builder("photos").build()?)
                    .await?
                    .configuration(),
                flags(bits)
            );
        }

        first
            .put_bucket_ownership_controls(
                PutBucketOwnershipControlsInput::builder(
                    "photos",
                    ObjectOwnership::BucketOwnerEnforced,
                )
                .build()?,
            )
            .await?;

        assert_eq!(
            second
                .get_bucket_ownership_controls(
                    GetBucketOwnershipControlsInput::builder("photos").build()?
                )
                .await?
                .object_ownership(),
            ObjectOwnership::BucketOwnerEnforced
        );

        let bucket_acl = second
            .get_bucket_acl(GetBucketAclInput::builder("photos").build()?)
            .await?;

        assert_eq!(bucket_acl.owner().canonical_id(), owner.canonical_id());
        assert_eq!(bucket_acl.grants().len(), 1);

        assert_eq!(
            bucket_acl.grants()[0].grantee_canonical_id(),
            owner.canonical_id()
        );

        assert_eq!(
            bucket_acl.grants()[0].permission(),
            AclPermission::FullControl
        );

        first
            .put_object(
                PutObjectInput::builder("photos", "literal/%2f/file", b"payload".to_vec())
                    .build()?,
            )
            .await?;

        let object_acl = second
            .get_object_acl(GetObjectAclInput::builder("photos", "literal/%2f/file").build()?)
            .await?;

        assert_eq!(object_acl.owner(), bucket_acl.owner());
        assert_eq!(object_acl.grants(), bucket_acl.grants());
        first.close().await?;
        second.close().await?;

        let reopened = stores.open(OpenOptions::new(&path)).await?;

        assert_eq!(
            reopened
                .get_bucket_policy(GetBucketPolicyInput::builder("photos").build()?)
                .await?,
            policy
        );

        assert_eq!(
            reopened
                .get_public_access_block(GetPublicAccessBlockInput::builder("photos").build()?)
                .await?
                .configuration(),
            flags(15)
        );

        assert_eq!(
            reopened
                .get_object_acl(GetObjectAclInput::builder("photos", "literal/%2f/file").build()?)
                .await?,
            object_acl
        );

        reopened
            .delete_object(DeleteObjectInput::builder("photos", "literal/%2f/file").build()?)
            .await?;

        assert_eq!(
            reopened
                .get_object_acl(GetObjectAclInput::builder("photos", "literal/%2f/file").build()?)
                .await
                .unwrap_err()
                .code(),
            ErrorCode::NoSuchKey
        );

        for _ in 0..2 {
            reopened
                .delete_bucket_policy(DeleteBucketPolicyInput::builder("photos").build()?)
                .await?;

            reopened
                .delete_public_access_block(
                    DeletePublicAccessBlockInput::builder("photos").build()?,
                )
                .await?;
        }

        assert_eq!(
            reopened
                .get_bucket_policy_status(GetBucketPolicyStatusInput::builder("photos").build()?)
                .await
                .unwrap_err()
                .code(),
            ErrorCode::NoSuchBucketPolicy
        );

        assert_eq!(
            reopened
                .get_public_access_block(GetPublicAccessBlockInput::builder("photos").build()?)
                .await
                .unwrap_err()
                .code(),
            ErrorCode::NoSuchPublicAccessBlockConfiguration
        );

        reopened
            .put_public_access_block(
                PutPublicAccessBlockInput::builder("photos", flags(0)).build()?,
            )
            .await?;

        assert_eq!(
            reopened
                .get_public_access_block(GetPublicAccessBlockInput::builder("photos").build()?)
                .await?
                .configuration(),
            flags(0)
        );

        reopened
            .delete_bucket(DeleteBucketInput::builder("photos").build()?)
            .await?;

        reopened
            .create_bucket(CreateBucketInput::builder("photos").build()?)
            .await?;

        assert_eq!(
            reopened
                .get_public_access_block(GetPublicAccessBlockInput::builder("photos").build()?)
                .await?
                .configuration(),
            flags(15)
        );

        reopened.close().await?;

        // Detached result values retain no connection or payload lifetime.
        assert_eq!(policy.document(), PUBLIC);
        assert_eq!(object_acl.grants(), bucket_acl.grants());
        Ok::<_, Box<dyn std::error::Error>>(())
    })?;

    directory.finish()?;
    Ok(())
}

#[test]
fn input_validation_is_io_free_and_policy_diagnostics_are_redacted() -> TestResult {
    let builder = PutBucketPolicyInput::builder("photos", PUBLIC);
    assert!(!format!("{builder:?}").contains("s3:GetObject"));
    let input = builder.build()?;
    assert_eq!(input.document(), PUBLIC);
    assert!(!format!("{input:?}").contains("s3:GetObject"));

    for document in [
        "{",
        "{\"Version\":1,\"Version\":2}",
        &" ".repeat(20 * 1024 + 1),
    ] {
        assert_eq!(
            PutBucketPolicyInput::builder("photos", document)
                .build()
                .unwrap_err()
                .code(),
            ErrorCode::InvalidArgument
        );
    }

    assert_eq!(
        GetBucketAclInput::builder("BAD")
            .build()
            .unwrap_err()
            .code(),
        ErrorCode::InvalidBucketName
    );

    assert_eq!(
        GetObjectAclInput::builder("photos", "")
            .build()
            .unwrap_err()
            .code(),
        ErrorCode::InvalidArgument
    );

    assert_eq!(
        PutPublicAccessBlockInput::builder("photos", flags(0))
            .expected_bucket_owner("1")
            .build()
            .unwrap_err()
            .code(),
        ErrorCode::InvalidArgument
    );

    assert_eq!(
        GetBucketOwnershipControlsInput::builder("photos")
            .expected_bucket_owner("000000000000")
            .build()?
            .expected_bucket_owner(),
        Some("000000000000")
    );

    Ok(())
}
