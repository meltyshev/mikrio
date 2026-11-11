use mikrio::{HeadBucketInput, SetPrincipalEnabledInput};

use super::*;

#[test]
fn bodyless_delete_checks_integrity_before_removing_objects_or_buckets() -> TestResult {
    let directory = TestDirectory::new()?;

    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()?;

    test_stores::run(&runtime, async |stores| {
        let store = stores
            .open(OpenOptions::new(directory.path().join("store")))
            .await?;

        let server = store.listen(ListenOptions::default()).await?;
        let credentials = server.credentials();

        let request = |method: &str, path: &str, body: &str, headers: &[(&str, &str)]| {
            signed_options(
                method,
                path,
                credentials.access_key_id(),
                credentials.secret_access_key(),
                body,
                "us-east-1",
                headers,
            )
        };

        status(
            &exchange(&server, &request("PUT", "/integrity", "", &[])).await?,
            200,
        );

        status(
            &exchange(
                &server,
                &request("PUT", "/integrity/object", "preserved", &[]),
            )
            .await?,
            200,
        );

        let wrong_digest = [("content-md5", "kAFQmDzST7DWlj99KOF/cg==")];
        let empty_digest = [("content-md5", "1B2M2Y8AsgTpgAmY7PhCfg==")];

        for path in ["/integrity/object", "/integrity"] {
            let response = exchange(&server, &request("DELETE", path, "", &wrong_digest)).await?;
            status(&response, 400);
            assert!(response.contains("<Code>BadDigest</Code>"), "{response}");
        }

        let response = exchange(&server, &request("GET", "/integrity/object", "", &[])).await?;
        status(&response, 200);
        assert_eq!(response.split_once("\r\n\r\n").unwrap().1, "preserved");

        for path in ["/integrity/object", "/integrity"] {
            status(
                &exchange(&server, &request("DELETE", path, "", &empty_digest)).await?,
                204,
            );
        }

        assert_eq!(
            store
                .head_bucket(HeadBucketInput::builder("integrity").build()?)
                .await
                .unwrap_err()
                .code(),
            ErrorCode::NoSuchBucket
        );

        store.close().await?;
        server.close().await?;
        Ok::<_, Box<dyn std::error::Error>>(())
    })?;

    directory.finish()?;
    Ok(())
}

#[test]
fn bucket_routes_authorize_prospective_resources_and_current_identity() -> TestResult {
    let directory = TestDirectory::new()?;

    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()?;

    test_stores::run(&runtime, async |stores| {
        let store = stores
            .open(OpenOptions::new(directory.path().join("store")))
            .await?;

        let principal = store
            .create_principal(CreatePrincipalInput::builder("provisioner").build()?)
            .await?;

        let policy = r#"{
            "Version": "2012-10-17",
            "Statement": {
                "Effect": "Allow",
                "Action": ["s3:CreateBucket", "s3:DeleteBucket", "s3:GetBucketLocation"],
                "Resource": "arn:aws:s3:::permitted"
            }
        }"#;

        store
            .put_principal_policy(
                PutPrincipalPolicyInput::builder(principal.principal_id(), policy).build()?,
            )
            .await?;

        let key = store
            .create_access_key(CreateAccessKeyInput::builder(principal.principal_id()).build()?)
            .await?;

        let server = store.listen(ListenOptions::default()).await?;

        let request = |method: &str, path: &str, body: &str| {
            signed(
                method,
                path,
                key.access_key().access_key_id(),
                key.secret_access_key(),
                body,
            )
        };

        status(
            &exchange(&server, &request("PUT", "/denied", "")).await?,
            403,
        );

        assert_eq!(
            store
                .head_bucket(HeadBucketInput::builder("denied").build()?)
                .await
                .unwrap_err()
                .code(),
            ErrorCode::NoSuchBucket
        );

        status(
            &exchange(&server, &request("PUT", "/permitted", "")).await?,
            200,
        );

        status(
            &exchange(&server, &request("PUT", "/permitted", "")).await?,
            200,
        );

        status(
            &exchange(&server, &request("HEAD", "/permitted", "")).await?,
            403,
        );

        let location = exchange(&server, &request("GET", "/permitted?location", "")).await?;
        status(&location, 200);
        assert!(location.contains("LocationConstraint"));
        assert!(!location.contains("us-east-1"));

        store
            .set_principal_enabled(
                SetPrincipalEnabledInput::builder(principal.principal_id(), false).build()?,
            )
            .await?;

        status(
            &exchange(&server, &request("DELETE", "/permitted", "")).await?,
            403,
        );

        store
            .head_bucket(HeadBucketInput::builder("permitted").build()?)
            .await?;

        store
            .set_principal_enabled(
                SetPrincipalEnabledInput::builder(principal.principal_id(), true).build()?,
            )
            .await?;

        status(
            &exchange(&server, &request("DELETE", "/permitted", "unexpected")).await?,
            400,
        );

        store
            .head_bucket(HeadBucketInput::builder("permitted").build()?)
            .await?;

        status(
            &exchange(&server, &request("DELETE", "/permitted", "")).await?,
            204,
        );

        assert_eq!(
            store
                .head_bucket(HeadBucketInput::builder("permitted").build()?)
                .await
                .unwrap_err()
                .code(),
            ErrorCode::NoSuchBucket
        );

        store.close().await?;
        server.close().await?;
        Ok::<_, Box<dyn std::error::Error>>(())
    })?;

    directory.finish()?;
    Ok(())
}

#[test]
fn bucket_xml_integrity_and_regional_repeats_preserve_namespace() -> TestResult {
    let directory = TestDirectory::new()?;

    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()?;

    test_stores::run(&runtime, async |stores| {
        let store = stores
            .open(OpenOptions::new(directory.path().join("store")).region("eu-west-1"))
            .await?;

        let server = store.listen(ListenOptions::default()).await?;
        let credentials = server.credentials();

        let request = |method: &str, path: &str, body: &str, headers: &[(&str, &str)]| {
            signed_options(
                method,
                path,
                credentials.access_key_id(),
                credentials.secret_access_key(),
                body,
                "eu-west-1",
                headers,
            )
        };

        let configuration = concat!(
            "<CreateBucketConfiguration xmlns=\"http://s3.amazonaws.com/doc/2006-03-01/\">",
            "<LocationConstraint>eu-west-1</LocationConstraint>",
            "</CreateBucketConfiguration>"
        );

        let duplicate = concat!(
            "<CreateBucketConfiguration>",
            "<LocationConstraint>eu-west-1</LocationConstraint>",
            "<LocationConstraint>eu-west-1</LocationConstraint>",
            "</CreateBucketConfiguration>"
        );

        for body in [
            configuration.to_owned() + "trailing",
            format!("{configuration}<Extra/>"),
            "<CreateBucketConfiguration><Unknown/></CreateBucketConfiguration>".into(),
            "<!DOCTYPE bucket><CreateBucketConfiguration/>".into(),
            "<CreateBucketConfiguration xmlns=\"https://attacker.invalid\"/>".into(),
            duplicate.into(),
        ] {
            let response = exchange(&server, &request("PUT", "/invalid", &body, &[])).await?;
            status(&response, 400);
            assert!(response.contains("<Code>MalformedXML</Code>"), "{response}");

            assert_eq!(
                store
                    .head_bucket(HeadBucketInput::builder("invalid").build()?)
                    .await
                    .unwrap_err()
                    .code(),
                ErrorCode::NoSuchBucket
            );
        }

        status(
            &exchange(
                &server,
                &request(
                    "PUT",
                    "/invalid",
                    configuration,
                    &[("content-md5", "1B2M2Y8AsgTpgAmY7PhCfg==")],
                ),
            )
            .await?,
            400,
        );

        assert_eq!(
            store
                .head_bucket(HeadBucketInput::builder("invalid").build()?)
                .await
                .unwrap_err()
                .code(),
            ErrorCode::NoSuchBucket
        );

        let absent = exchange(&server, &request("PUT", "/regional", "", &[])).await?;
        status(&absent, 400);
        assert!(absent.contains("IllegalLocationConstraintException"));

        status(
            &exchange(&server, &request("PUT", "/regional", configuration, &[])).await?,
            200,
        );

        let repeated = exchange(&server, &request("PUT", "/regional", configuration, &[])).await?;
        status(&repeated, 409);
        assert!(repeated.contains("BucketAlreadyOwnedByYou"));
        let location = exchange(&server, &request("GET", "/regional?location", "", &[])).await?;
        status(&location, 200);
        assert!(location.contains(">eu-west-1</LocationConstraint>"));

        status(
            &exchange(
                &server,
                &request(
                    "HEAD",
                    "/regional",
                    "",
                    &[("x-amz-expected-bucket-owner", "000000000000")],
                ),
            )
            .await?,
            403,
        );

        store.close().await?;
        server.close().await?;
        Ok::<_, Box<dyn std::error::Error>>(())
    })?;

    directory.finish()?;
    Ok(())
}
