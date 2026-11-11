use mikrio::{
    CreateAccessKeyInput, CreatePrincipalInput, DeleteAccessKeyInput, DeletePrincipalInput,
    DeletePrincipalPolicyInput, ErrorCode, GetPrincipalInput, GetPrincipalPolicyInput,
    ListAccessKeysInput, ListPrincipalsInput, MutationOutcome, OpenOptions,
    PutPrincipalPolicyInput, SetPrincipalEnabledInput,
};

#[path = "support/stores.rs"]
mod test_stores;

#[path = "../../mikrio-core/tests/support/directory.rs"]
mod test_directory;

use test_directory::TestDirectory;

type TestResult = Result<(), Box<dyn std::error::Error>>;

const POLICY: &str = r#"{ "Version":"2012-10-17", "Statement":{"Effect":"Allow","Action":"s3:GetObject","Resource":"arn:aws:s3:::photos/*"}}"#;

#[test]
fn administration_persists_through_independent_connections_and_reopen() -> TestResult {
    let directory = TestDirectory::new()?;

    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_time()
        .build()?;

    test_stores::run(&runtime, async |stores| {
        let path = directory.path().join("store");
        let first = stores.open(OpenOptions::new(&path)).await?;
        let second = stores.open(OpenOptions::new(&path)).await?;
        let owner = first.get_owner_info().await?;
        assert_eq!(owner, second.get_owner_info().await?);

        let principal = first
            .create_principal(CreatePrincipalInput::builder("app").build()?)
            .await?;

        assert!(principal.is_enabled());
        let principal_id = principal.principal_id();

        assert_eq!(
            second
                .get_principal(GetPrincipalInput::builder(principal_id).build()?)
                .await?,
            principal
        );

        first
            .put_principal_policy(PutPrincipalPolicyInput::builder(principal_id, POLICY).build()?)
            .await?;

        let policy = second
            .get_principal_policy(GetPrincipalPolicyInput::builder(principal_id).build()?)
            .await?;

        assert_eq!(policy.document(), POLICY);
        assert!(!format!("{policy:?}").contains("photos"));

        let key = first
            .create_access_key(CreateAccessKeyInput::builder(principal_id).build()?)
            .await?;

        assert!(!format!("{key:?}").contains(key.secret_access_key()));

        let inventory = second
            .list_access_keys(ListAccessKeysInput::builder(principal_id).build()?)
            .await?;

        assert_eq!(inventory.access_keys(), &[key.access_key().clone()]);
        assert_eq!(inventory.next_continuation_token(), None);
        assert!(!format!("{inventory:?}").contains(key.secret_access_key()));

        let error = first
            .delete_principal(DeletePrincipalInput::builder(principal_id).build()?)
            .await
            .unwrap_err();

        assert_eq!(error.code(), ErrorCode::PrincipalHasAccessKeys);

        assert_eq!(
            error.mutation_outcome(),
            Some(&MutationOutcome::NotCommitted)
        );

        let disabled = second
            .set_principal_enabled(SetPrincipalEnabledInput::builder(principal_id, false).build()?)
            .await?;

        assert!(!disabled.is_enabled());
        assert_eq!(disabled.created_at(), principal.created_at());
        first.close().await?;

        let closed = first
            .create_access_key(CreateAccessKeyInput::builder(principal_id).build()?)
            .await
            .unwrap_err();

        assert_eq!(closed.code(), ErrorCode::StoreClosed);

        assert_eq!(
            closed.mutation_outcome(),
            Some(&MutationOutcome::NotCommitted)
        );

        second.close().await?;

        let reopened = stores.open(OpenOptions::new(&path)).await?;
        assert_eq!(reopened.get_owner_info().await?, owner);

        let page = reopened
            .list_principals(ListPrincipalsInput::builder().max_results(1).build()?)
            .await?;

        assert_eq!(page.principals(), &[disabled]);
        assert_eq!(page.next_continuation_token(), None);

        reopened
            .delete_access_key(
                DeleteAccessKeyInput::builder(principal_id, key.access_key().access_key_id())
                    .build()?,
            )
            .await?;

        reopened
            .delete_principal_policy(DeletePrincipalPolicyInput::builder(principal_id).build()?)
            .await?;

        assert_eq!(
            reopened
                .get_principal_policy(GetPrincipalPolicyInput::builder(principal_id).build()?)
                .await
                .unwrap_err()
                .code(),
            ErrorCode::NoSuchPrincipalPolicy
        );

        reopened
            .delete_principal(DeletePrincipalInput::builder(principal_id).build()?)
            .await?;

        assert_eq!(
            reopened
                .get_principal(GetPrincipalInput::builder(principal_id).build()?)
                .await
                .unwrap_err()
                .code(),
            ErrorCode::NoSuchPrincipal
        );

        reopened.close().await?;
        Ok::<_, Box<dyn std::error::Error>>(())
    })?;

    directory.finish()?;
    Ok(())
}

#[test]
fn administration_builders_validate_without_attaching_mutation_outcomes() {
    for name in ["", "line\nfeed", &"é".repeat(65)] {
        let error = CreatePrincipalInput::builder(name).build().unwrap_err();
        assert_eq!(error.code(), ErrorCode::InvalidArgument, "name={name:?}");
        assert_eq!(error.mutation_outcome(), None, "name={name:?}");
    }

    for size in [0, 1001, u32::MAX] {
        assert_eq!(
            ListPrincipalsInput::builder()
                .max_results(size)
                .build()
                .unwrap_err()
                .code(),
            ErrorCode::InvalidArgument,
            "size={size}"
        );
    }

    assert_eq!(
        ListPrincipalsInput::builder()
            .build()
            .unwrap()
            .max_results(),
        100
    );

    assert_eq!(
        GetPrincipalInput::builder("app")
            .build()
            .unwrap_err()
            .code(),
        ErrorCode::InvalidArgument
    );

    assert_eq!(
        PutPrincipalPolicyInput::builder("01".repeat(16), "{}")
            .build()
            .unwrap_err()
            .code(),
        ErrorCode::InvalidArgument
    );

    let input = PutPrincipalPolicyInput::builder("01".repeat(16), POLICY)
        .build()
        .unwrap();

    assert!(!format!("{input:?}").contains("photos"));
}
