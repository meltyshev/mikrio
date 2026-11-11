use std::fmt::Write as _;

use mikrio::{
    DeleteBucketPolicyInput, GetBucketPolicyInput, GetPublicAccessBlockInput,
    PublicAccessBlockConfiguration, PutBucketPolicyInput,
};

use super::*;

#[path = "configuration/wire.rs"]
mod wire;

const DENY: &str = r#"{"Version":"2012-10-17","Statement":{"Effect":"Deny","Principal":"*","Action":"s3:*","Resource":"*"}}"#;
const PUBLIC: &str = r#"{"Version":"2012-10-17","Statement":{"Effect":"Allow","Principal":"*","Action":"s3:GetObject","Resource":"arn:aws:s3:::photos/*"}}"#;
const ENFORCED: &str = "<OwnershipControls xmlns=\"http://s3.amazonaws.com/doc/2006-03-01/\"><Rule><ObjectOwnership>BucketOwnerEnforced</ObjectOwnership></Rule></OwnershipControls>";
const EMPTY_BLOCK: &str =
    "<PublicAccessBlockConfiguration xmlns=\"http://s3.amazonaws.com/doc/2006-03-01/\"/>";

const ADMIN: &str =
    r#"{"Version":"2012-10-17","Statement":{"Effect":"Allow","Action":"s3:*","Resource":"*"}}"#;
const INVALID_POLICY_DOCUMENTS: [&str; 3] = [
    "{",
    r#"{"Version":"2012-10-17","Statement":[],"Statement":[]}"#,
    r#"{"Version":"2012-10-17","Statement":{"Effect":"Allow","Principal":"*","Action":"s3:Unknown","Resource":"*"}}"#,
];
const INVALID_BLOCK_DOCUMENTS: [&str; 6] = [
    "<PublicAccessBlockConfiguration><Unknown/></PublicAccessBlockConfiguration>",
    "<PublicAccessBlockConfiguration><BlockPublicPolicy>false</BlockPublicPolicy><BlockPublicPolicy>true</BlockPublicPolicy></PublicAccessBlockConfiguration>",
    "<PublicAccessBlockConfiguration><BlockPublicPolicy>maybe</BlockPublicPolicy></PublicAccessBlockConfiguration>",
    "<PublicAccessBlockConfiguration/>trailing",
    "<!DOCTYPE x [<!ENTITY inject 'false'>]><PublicAccessBlockConfiguration><BlockPublicPolicy>&inject;</BlockPublicPolicy></PublicAccessBlockConfiguration>",
    "<PublicAccessBlockConfiguration xmlns=\"https://untrusted.invalid/\"/>",
];

fn flags(bits: u8) -> PublicAccessBlockConfiguration {
    PublicAccessBlockConfiguration {
        block_public_acls: bits & 1 != 0,
        ignore_public_acls: bits & 2 != 0,
        block_public_policy: bits & 4 != 0,
        restrict_public_buckets: bits & 8 != 0,
    }
}

#[test]
fn bootstrap_policy_recovery_never_grants_other_configuration_or_application_access() -> TestResult
{
    let directory = TestDirectory::new()?;

    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()?;

    test_stores::run(&runtime, async |stores| {
        let store = stores
            .open(OpenOptions::new(directory.path().join("store")))
            .await?;

        store
            .create_bucket(CreateBucketInput::builder("photos").build()?)
            .await?;

        let principal = store
            .create_principal(CreatePrincipalInput::builder("admin-app").build()?)
            .await?;

        store
            .put_principal_policy(
                PutPrincipalPolicyInput::builder(principal.principal_id(), ADMIN).build()?,
            )
            .await?;

        let key = store
            .create_access_key(CreateAccessKeyInput::builder(principal.principal_id()).build()?)
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

        status(
            &exchange(&server, &request("PUT", "/photos?policy", DENY)).await?,
            204,
        );

        let current = exchange(&server, &request("GET", "/photos?policy", "")).await?;
        status(&current, 200);
        assert_eq!(current.split_once("\r\n\r\n").unwrap().1, DENY);

        for (method, path, body) in [
            ("GET", "/photos?policyStatus", ""),
            ("GET", "/photos?publicAccessBlock", ""),
            ("PUT", "/photos?publicAccessBlock", EMPTY_BLOCK),
            ("DELETE", "/photos?publicAccessBlock", ""),
            ("GET", "/photos?ownershipControls", ""),
            ("PUT", "/photos?ownershipControls", ENFORCED),
            ("GET", "/photos?acl", ""),
            ("GET", "/photos/missing?acl", ""),
        ] {
            status(&exchange(&server, &request(method, path, body)).await?, 403);
        }

        for (method, body) in [("GET", ""), ("PUT", DENY), ("DELETE", "")] {
            status(
                &exchange(
                    &server,
                    &signed(
                        method,
                        "/photos?policy",
                        key.access_key().access_key_id(),
                        key.secret_access_key(),
                        body,
                    ),
                )
                .await?,
                403,
            );
        }

        // The recovery exception cannot skip the current BlockPublicPolicy flag.
        status(
            &exchange(&server, &request("PUT", "/photos?policy", PUBLIC)).await?,
            403,
        );

        let owner = store.get_owner_info().await?;

        let wrong_owner = if owner.account_id() == "000000000000" {
            "111111111111"
        } else {
            "000000000000"
        };

        status(
            &exchange(
                &server,
                &signed_options(
                    "DELETE",
                    "/photos?policy",
                    credentials.access_key_id(),
                    credentials.secret_access_key(),
                    "",
                    "us-east-1",
                    &[("x-amz-expected-bucket-owner", wrong_owner)],
                ),
            )
            .await?,
            403,
        );

        status(
            &exchange(&server, &request("DELETE", "/photos?policy", "")).await?,
            204,
        );

        let absent = exchange(&server, &request("GET", "/photos?policy", "")).await?;
        status(&absent, 404);
        assert!(absent.contains("<Code>NoSuchBucketPolicy</Code>"));

        status(
            &exchange(&server, &request("GET", "/photos?publicAccessBlock", "")).await?,
            200,
        );

        store.close().await?;
        Ok::<_, Box<dyn std::error::Error>>(())
    })?;

    directory.finish()?;
    Ok(())
}

#[test]
fn configuration_round_trips_all_flags_and_fixed_owner_acl_over_the_listener() -> TestResult {
    let directory = TestDirectory::new()?;

    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()?;

    test_stores::run(&runtime, async |stores| {
        let store = stores
            .open(OpenOptions::new(directory.path().join("store")))
            .await?;

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

        status(
            &exchange(
                &server,
                &request("PUT", "/photos?publicAccessBlock", EMPTY_BLOCK),
            )
            .await?,
            200,
        );

        assert_eq!(
            store
                .get_public_access_block(GetPublicAccessBlockInput::builder("photos").build()?)
                .await?
                .configuration(),
            flags(0)
        );

        status(
            &exchange(&server, &request("PUT", "/photos?policy", PUBLIC)).await?,
            204,
        );

        for bits in 0..16 {
            let fields = [
                ("BlockPublicAcls", bits & 1 != 0),
                ("IgnorePublicAcls", bits & 2 != 0),
                ("BlockPublicPolicy", bits & 4 != 0),
                ("RestrictPublicBuckets", bits & 8 != 0),
            ];

            let mut elements = String::new();

            for (name, value) in fields {
                write!(elements, "<{name}>{value}</{name}>")?;
            }

            let xml = format!(
                "<PublicAccessBlockConfiguration>{elements}</PublicAccessBlockConfiguration>"
            );

            status(
                &exchange(&server, &request("PUT", "/photos?publicAccessBlock", &xml)).await?,
                200,
            );

            let response =
                exchange(&server, &request("GET", "/photos?publicAccessBlock", "")).await?;

            status(&response, 200);

            for (name, value) in fields {
                assert!(
                    response.contains(&format!("<{name}>{value}</{name}>")),
                    "missing flag in combination {bits}"
                );
            }

            assert_eq!(
                store
                    .get_public_access_block(GetPublicAccessBlockInput::builder("photos").build()?)
                    .await?
                    .configuration(),
                flags(bits)
            );

            let response = exchange(&server, &request("GET", "/photos?policyStatus", "")).await?;
            status(&response, 200);
            assert!(response.contains("<IsPublic>true</IsPublic>"));
        }

        status(
            &exchange(
                &server,
                &request("PUT", "/photos?ownershipControls", ENFORCED),
            )
            .await?,
            200,
        );

        let ownership = exchange(&server, &request("GET", "/photos?ownershipControls", "")).await?;
        status(&ownership, 200);
        assert!(ownership.contains("<ObjectOwnership>BucketOwnerEnforced</ObjectOwnership>"));

        status(
            &exchange(&server, &request("PUT", "/photos/object", "payload")).await?,
            200,
        );

        let owner = store.get_owner_info().await?;

        for path in ["/photos?acl", "/photos/object?acl"] {
            let response = exchange(&server, &request("GET", path, "")).await?;
            status(&response, 200);

            assert_eq!(
                response
                    .matches(&format!("<ID>{}</ID>", owner.canonical_id()))
                    .count(),
                2
            );

            assert_eq!(response.matches("<Grant>").count(), 1);
            assert!(response.contains("CanonicalUser"));
            assert!(response.contains("<Permission>FULL_CONTROL</Permission>"));
            assert!(!response.contains("DisplayName"));
            status(&exchange(&server, &request("PUT", path, "")).await?, 501);
        }

        status(
            &exchange(&server, &request("DELETE", "/photos?ownershipControls", "")).await?,
            501,
        );

        status(
            &exchange(
                &server,
                &request(
                    "PUT",
                    "/photos?ownershipControls",
                    &ENFORCED.replace("BucketOwnerEnforced", "ObjectWriter"),
                ),
            )
            .await?,
            501,
        );

        for _ in 0..2 {
            status(
                &exchange(&server, &request("DELETE", "/photos?publicAccessBlock", "")).await?,
                204,
            );
        }

        let absent = exchange(&server, &request("GET", "/photos?publicAccessBlock", "")).await?;
        status(&absent, 404);
        assert!(absent.contains("<Code>NoSuchPublicAccessBlockConfiguration</Code>"));
        store.close().await?;
        Ok::<_, Box<dyn std::error::Error>>(())
    })?;

    directory.finish()?;
    Ok(())
}

#[test]
fn invalid_complete_envelopes_and_delete_digests_preserve_configuration() -> TestResult {
    let directory = TestDirectory::new()?;

    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()?;

    test_stores::run(&runtime, async |stores| {
        let store = stores
            .open(OpenOptions::new(directory.path().join("store")))
            .await?;

        store
            .create_bucket(CreateBucketInput::builder("photos").build()?)
            .await?;

        store
            .put_bucket_policy(PutBucketPolicyInput::builder("photos", DENY).build()?)
            .await?;

        // Use native recovery between attempts; no invalid request can remove it.
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

        let wrong_digest = [("content-md5", "kAFQmDzST7DWlj99KOF/cg==")];
        let empty_digest = [("content-md5", "1B2M2Y8AsgTpgAmY7PhCfg==")];

        for (path, body) in [
            ("/photos?policy", DENY),
            ("/photos?publicAccessBlock", EMPTY_BLOCK),
            ("/photos?ownershipControls", ENFORCED),
        ] {
            let response = exchange(&server, &request("PUT", path, body, &wrong_digest)).await?;
            status(&response, 400);
            assert!(response.contains("<Code>BadDigest</Code>"));
        }

        for body in INVALID_POLICY_DOCUMENTS {
            let response = exchange(&server, &request("PUT", "/photos?policy", body, &[])).await?;
            status(&response, 400);
            assert!(response.contains("<Code>MalformedPolicy</Code>"));

            assert_eq!(
                store
                    .get_bucket_policy(GetBucketPolicyInput::builder("photos").build()?)
                    .await?
                    .document(),
                DENY
            );
        }

        for path in ["/photos?policy", "/photos?publicAccessBlock"] {
            let response = exchange(&server, &request("DELETE", path, "", &wrong_digest)).await?;
            status(&response, 400);
            assert!(response.contains("<Code>BadDigest</Code>"));

            status(
                &exchange(&server, &request("DELETE", path, "unexpected", &[])).await?,
                400,
            );
        }

        assert_eq!(
            store
                .get_bucket_policy(GetBucketPolicyInput::builder("photos").build()?)
                .await?
                .document(),
            DENY
        );

        assert_eq!(
            store
                .get_public_access_block(GetPublicAccessBlockInput::builder("photos").build()?)
                .await?
                .configuration(),
            flags(15)
        );

        store
            .delete_bucket_policy(DeleteBucketPolicyInput::builder("photos").build()?)
            .await?;

        for body in INVALID_BLOCK_DOCUMENTS {
            let response = exchange(
                &server,
                &request("PUT", "/photos?publicAccessBlock", body, &[]),
            )
            .await?;

            status(&response, 400);
            assert!(response.contains("<Code>MalformedXML</Code>"));

            assert_eq!(
                store
                    .get_public_access_block(GetPublicAccessBlockInput::builder("photos").build()?)
                    .await?
                    .configuration(),
                flags(15)
            );
        }

        status(
            &exchange(
                &server,
                &request("DELETE", "/photos?publicAccessBlock", "", &empty_digest),
            )
            .await?,
            204,
        );

        assert_eq!(
            store
                .get_public_access_block(GetPublicAccessBlockInput::builder("photos").build()?)
                .await
                .unwrap_err()
                .code(),
            ErrorCode::NoSuchPublicAccessBlockConfiguration
        );

        store.close().await?;
        Ok::<_, Box<dyn std::error::Error>>(())
    })?;

    directory.finish()?;
    Ok(())
}
