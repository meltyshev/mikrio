use mikrio::{GetObjectAclInput, PutObjectInput, Tag};

use super::*;

#[test]
fn object_acl_missing_disclosure_requires_current_acl_and_bucket_permissions() -> TestResult {
    let directory = TestDirectory::new()?;

    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()?;

    test_stores::run(&runtime, async |stores| {
        let path = directory.path().join("store");
        let store = stores.open(OpenOptions::new(&path)).await?;
        let administrator = stores.open(OpenOptions::new(path)).await?;

        store
            .create_bucket(CreateBucketInput::builder("photos").build()?)
            .await?;

        store
            .put_object(PutObjectInput::builder("photos", "present", b"private".to_vec()).build()?)
            .await?;

        let principal = store
            .create_principal(CreatePrincipalInput::builder("acl-reader").build()?)
            .await?;

        let key = store
            .create_access_key(CreateAccessKeyInput::builder(principal.principal_id()).build()?)
            .await?;

        let owner = store.get_owner_info().await?;
        let server = store.listen(ListenOptions::default()).await?;

        let cases = [
            (
                r#"{"Effect":"Allow","Action":"s3:GetObjectAcl","Resource":"arn:aws:s3:::photos/*"}"#,
                200,
                403,
            ),
            (
                r#"{"Effect":"Allow","Action":["s3:GetObjectAcl","s3:ListBucket"],"Resource":["arn:aws:s3:::photos","arn:aws:s3:::photos/*"]}"#,
                200,
                404,
            ),
            (
                r#"{"Effect":"Allow","Action":"s3:ListBucket","Resource":"arn:aws:s3:::photos"}"#,
                403,
                403,
            ),
            (
                r#"{"Effect":"Allow","Action":"s3:*","Resource":"*"},{"Effect":"Deny","Action":"s3:GetObjectAcl","Resource":"arn:aws:s3:::photos/*"}"#,
                403,
                403,
            ),
        ];

        for (statements, present, missing) in cases {
            let policy = format!(r#"{{"Version":"2012-10-17","Statement":[{statements}]}}"#);

            administrator
                .put_principal_policy(
                    PutPrincipalPolicyInput::builder(principal.principal_id(), policy).build()?,
                )
                .await?;

            for (target, expected) in [("present", present), ("missing", missing)] {
                let response = exchange(
                    &server,
                    &signed(
                        "GET",
                        &format!("/photos/{target}?acl"),
                        key.access_key().access_key_id(),
                        key.secret_access_key(),
                        "",
                    ),
                )
                .await?;

                status(&response, expected);

                if expected == 200 {
                    assert!(response.contains(owner.canonical_id()));

                    assert_eq!(
                        response
                            .matches("<Permission>FULL_CONTROL</Permission>")
                            .count(),
                        1
                    );
                } else {
                    let code = if expected == 404 {
                        "NoSuchKey"
                    } else {
                        "AccessDenied"
                    };

                    assert!(response.contains(&format!("<Code>{code}</Code>")));
                    assert!(!response.contains(owner.canonical_id()));
                    assert!(!response.contains("<AccessControlList>"));
                }
            }
        }

        let native = administrator
            .get_object_acl(GetObjectAclInput::builder("photos", "missing").build()?)
            .await
            .unwrap_err();

        assert_eq!(native.code(), ErrorCode::NoSuchKey);
        store.close().await?;
        administrator.close().await?;
        Ok::<_, Box<dyn std::error::Error>>(())
    })?;

    directory.finish()?;
    Ok(())
}

#[test]
fn read_and_acl_use_current_tags_before_conditional_disclosure() -> TestResult {
    let directory = TestDirectory::new()?;

    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()?;

    test_stores::run(&runtime, async |stores| {
        let path = directory.path().join("store");
        let store = stores.open(OpenOptions::new(&path)).await?;
        let administrator = stores.open(OpenOptions::new(path)).await?;

        store
            .create_bucket(CreateBucketInput::builder("photos").build()?)
            .await?;

        let principal = store
            .create_principal(CreatePrincipalInput::builder("tag-reader").build()?)
            .await?;

        let policy = r#"{"Version":"2012-10-17","Statement":{"Effect":"Allow","Action":["s3:GetObject","s3:GetObjectAcl"],"Resource":"arn:aws:s3:::photos/*","Condition":{"StringEquals":{"s3:ExistingObjectTag/state":"public"}}}}"#;

        store
            .put_principal_policy(
                PutPrincipalPolicyInput::builder(principal.principal_id(), policy).build()?,
            )
            .await?;

        let key = store
            .create_access_key(CreateAccessKeyInput::builder(principal.principal_id()).build()?)
            .await?;

        let server = store.listen(ListenOptions::default()).await?;
        let mut etag = None;

        for visibility in ["public", "private", "public"] {
            let put = administrator
                .put_object(
                    PutObjectInput::builder("photos", "selected", b"same bytes".to_vec())
                        .tags(vec![Tag {
                            key: "state".into(),
                            value: visibility.into(),
                        }])
                        .build()?,
                )
                .await?;

            if let Some(previous) = &etag {
                assert_eq!(previous, put.etag());
            }

            etag = Some(put.etag().to_owned());

            for method in ["GET", "HEAD"] {
                for (condition, value, allowed) in [
                    ("if-none-match", put.etag(), 304),
                    ("if-match", "\"different\"", 412),
                ] {
                    let response = exchange(
                        &server,
                        &signed_options(
                            method,
                            "/photos/selected",
                            key.access_key().access_key_id(),
                            key.secret_access_key(),
                            "",
                            "us-east-1",
                            &[(condition, value)],
                        ),
                    )
                    .await?;

                    status(
                        &response,
                        if visibility == "public" { allowed } else { 403 },
                    );

                    assert!(
                        !response
                            .to_ascii_lowercase()
                            .contains("x-amz-tagging-count:")
                    );

                    if visibility == "private" {
                        assert!(!response.to_ascii_lowercase().contains("\r\netag:"));
                    }

                    if method == "HEAD" || (visibility == "public" && allowed == 304) {
                        assert_eq!(response.split_once("\r\n\r\n").unwrap().1, "");
                    }
                }
            }

            let acl = exchange(
                &server,
                &signed(
                    "GET",
                    "/photos/selected?acl",
                    key.access_key().access_key_id(),
                    key.secret_access_key(),
                    "",
                ),
            )
            .await?;

            status(&acl, if visibility == "public" { 200 } else { 403 });
        }

        store.close().await?;
        administrator.close().await?;
        Ok::<_, Box<dyn std::error::Error>>(())
    })?;

    directory.finish()?;
    Ok(())
}

#[test]
fn selected_http_read_retains_bytes_across_peer_revoke_and_overwrite() -> TestResult {
    let directory = TestDirectory::new()?;

    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()?;

    test_stores::run(&runtime, async |stores| {
        let path = directory.path().join("store");
        let store = stores.open(OpenOptions::new(&path)).await?;
        let peer = stores.open(OpenOptions::new(&path)).await?;

        store
            .create_bucket(CreateBucketInput::builder("photos").build()?)
            .await?;

        let previous = store
            .put_object(
                PutObjectInput::builder("photos", "selected", vec![b'x'; 4 * 1024 * 1024])
                    .build()?,
            )
            .await?;

        let principal = store
            .create_principal(CreatePrincipalInput::builder("selected-reader").build()?)
            .await?;

        let policy = r#"{"Version":"2012-10-17","Statement":{"Effect":"Allow","Action":"s3:GetObject","Resource":"arn:aws:s3:::photos/selected"}}"#;

        store
            .put_principal_policy(
                PutPrincipalPolicyInput::builder(principal.principal_id(), policy).build()?,
            )
            .await?;

        let key = store
            .create_access_key(CreateAccessKeyInput::builder(principal.principal_id()).build()?)
            .await?;

        let first = store.listen(ListenOptions::default()).await?;
        let second = peer.listen(ListenOptions::default()).await?;

        let read = signed(
            "GET",
            "/photos/selected",
            key.access_key().access_key_id(),
            key.secret_access_key(),
            "",
        );

        let endpoint = first
            .endpoint()
            .strip_prefix("http://")
            .ok_or("expected HTTP endpoint")?;

        let mut socket = tokio::net::TcpStream::connect(endpoint).await?;
        socket.write_all(&read).await?;
        let mut headers = Vec::new();

        tokio::time::timeout(Duration::from_secs(10), async {
            while !headers.ends_with(b"\r\n\r\n") {
                assert!(headers.len() < 16 * 1024);
                headers.push(socket.read_u8().await?);
            }

            Ok::<_, std::io::Error>(())
        })
        .await??;

        let headers = std::str::from_utf8(&headers)?;
        status(headers, 200);

        assert!(
            headers
                .to_ascii_lowercase()
                .contains(&format!("etag: {}", previous.etag()))
        );

        let mut prefix = [0_u8; 17];
        tokio::time::timeout(Duration::from_secs(10), socket.read_exact(&mut prefix)).await??;
        assert_eq!(prefix, [b'x'; 17]);

        peer.delete_access_key(
            DeleteAccessKeyInput::builder(
                principal.principal_id(),
                key.access_key().access_key_id(),
            )
            .build()?,
        )
        .await?;

        peer.put_object(
            PutObjectInput::builder("photos", "selected", b"replacement".to_vec()).build()?,
        )
        .await?;

        for server in [&first, &second] {
            let denied = exchange(server, &read).await?;
            status(&denied, 403);
            assert!(!denied.to_ascii_lowercase().contains("etag:"));
            assert!(!denied.contains("replacement"));
        }

        // This asserts the already selected HTTP response, including bytes
        // retained by transport buffers. Native guard lifetime is qualified by
        // the separate publication/close checkpoint histories.
        let mut remainder = Vec::new();

        tokio::time::timeout(Duration::from_secs(10), socket.read_to_end(&mut remainder)).await??;

        assert_eq!(remainder.len() + prefix.len(), 4 * 1024 * 1024);
        assert!(remainder.iter().all(|byte| *byte == b'x'));

        let mut current = peer
            .get_object(GetObjectInput::builder("photos", "selected").build()?)
            .await?
            .into_body();

        assert_eq!(current.read_all(100).await?, b"replacement");
        current.close().await?;
        first.close().await?;
        second.close().await?;
        store.close().await?;
        peer.close().await?;
        Ok::<_, Box<dyn std::error::Error>>(())
    })?;

    directory.finish()?;
    Ok(())
}
