use mikrio::Store;

use super::*;

const PRIVATE: &str = r#"{"Version":"2012-10-17","Statement":{"Effect":"Deny","Principal":"*","Action":"s3:PutObject","Resource":"arn:aws:s3:::photos/*"}}"#;

#[test]
fn policy_status_preserves_absence_and_rejected_confirmation_headers_preserve_policy() -> TestResult
{
    let directory = TestDirectory::new()?;

    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()?;

    test_stores::run(&runtime, async |stores| {
        let path = directory.path().join("store");
        let store = stores.open(OpenOptions::new(&path)).await?;

        store
            .create_bucket(CreateBucketInput::builder("photos").build()?)
            .await?;

        let server = store.listen(ListenOptions::default()).await?;
        let credentials = server.credentials();

        let request = |method: &str, path: &str, body: &str| {
            signed(
                method,
                path,
                credentials.access_key_id(),
                credentials.secret_access_key(),
                body,
            )
        };

        assert_policy_absent(&server).await?;

        status(
            &exchange(&server, &request("PUT", "/photos?policy", PRIVATE)).await?,
            204,
        );

        let current = exchange(&server, &request("GET", "/photos?policyStatus", "")).await?;
        status(&current, 200);
        assert_eq!(current.matches("<IsPublic>false</IsPublic>").count(), 1);

        for confirmation in ["", "true", "false"] {
            let rejected = signed_options(
                "PUT",
                "/photos?policy",
                credentials.access_key_id(),
                credentials.secret_access_key(),
                DENY,
                "us-east-1",
                &[("x-amz-confirm-remove-self-bucket-access", confirmation)],
            );

            let response = exchange(&server, &rejected).await?;
            status(&response, 501);
            assert!(response.contains("<Code>NotImplemented</Code>"));

            assert_eq!(
                store
                    .get_bucket_policy(GetBucketPolicyInput::builder("photos").build()?)
                    .await?
                    .document(),
                PRIVATE,
                "confirmation {confirmation:?} changed the policy"
            );
        }

        for _ in 0..2 {
            status(
                &exchange(&server, &request("DELETE", "/photos?policy", "")).await?,
                204,
            );
        }

        assert_policy_absent(&server).await?;
        store.close().await?;
        let reopened = stores.open(OpenOptions::new(path)).await?;

        let error = reopened
            .get_bucket_policy(GetBucketPolicyInput::builder("photos").build()?)
            .await
            .unwrap_err();

        assert_eq!(error.code(), ErrorCode::NoSuchBucketPolicy);
        reopened.close().await?;
        Ok::<_, Box<dyn std::error::Error>>(())
    })?;

    directory.finish()?;
    Ok(())
}

async fn assert_policy_absent(server: &Server) -> TestResult {
    let credentials = server.credentials();

    for path in ["/photos?policy", "/photos?policyStatus"] {
        let request = signed(
            "GET",
            path,
            credentials.access_key_id(),
            credentials.secret_access_key(),
            "",
        );

        let absent = exchange(server, &request).await?;
        status(&absent, 404);
        assert!(absent.contains("<Code>NoSuchBucketPolicy</Code>"));
        assert!(!absent.contains("<IsPublic>"));
    }

    Ok(())
}

#[test]
fn simultaneous_bucket_policy_replacements_return_whole_documents_and_survive_reopen() -> TestResult
{
    let directory = TestDirectory::new()?;

    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()?;

    test_stores::run(&runtime, async |stores| {
        let path = directory.path().join("store");
        let writer = stores.open(OpenOptions::new(&path)).await?;
        let peer = stores.open(OpenOptions::new(&path)).await?;
        let reader = stores.open(OpenOptions::new(&path)).await?;

        writer
            .create_bucket(CreateBucketInput::builder("photos").build()?)
            .await?;

        writer
            .put_bucket_policy(PutBucketPolicyInput::builder("photos", PRIVATE).build()?)
            .await?;

        for round in 0..32 {
            let replace = |connection: Store, document: &'static str| {
                tokio::spawn(async move {
                    connection
                        .put_bucket_policy(
                            PutBucketPolicyInput::builder("photos", document).build()?,
                        )
                        .await
                })
            };

            let written = replace(writer.clone(), PRIVATE);
            let replaced = replace(peer.clone(), DENY);

            let observed = reader
                .get_bucket_policy(GetBucketPolicyInput::builder("photos").build()?)
                .await;

            let written = written.await;
            let replaced = replaced.await;
            written??;
            replaced??;
            let observed = observed?;

            assert!(
                [PRIVATE, DENY].contains(&observed.document()),
                "round {round}: reader observed a partial policy"
            );
        }

        let settled = reader
            .get_bucket_policy(GetBucketPolicyInput::builder("photos").build()?)
            .await?;

        assert!([PRIVATE, DENY].contains(&settled.document()));
        writer.close().await?;
        peer.close().await?;
        reader.close().await?;
        let reopened = stores.open(OpenOptions::new(path)).await?;

        assert_eq!(
            reopened
                .get_bucket_policy(GetBucketPolicyInput::builder("photos").build()?)
                .await?
                .document(),
            settled.document()
        );

        reopened.close().await?;
        Ok::<_, Box<dyn std::error::Error>>(())
    })?;

    directory.finish()?;
    Ok(())
}
