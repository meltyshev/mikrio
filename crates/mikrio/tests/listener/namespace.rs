use mikrio::{GetBucketPolicyInput, PutBucketPolicyInput, PutObjectInput};

use super::*;

async fn read_response(
    socket: &mut tokio::net::TcpStream,
) -> Result<String, Box<dyn std::error::Error>> {
    let mut response = String::new();

    tokio::time::timeout(
        Duration::from_secs(10),
        socket.read_to_string(&mut response),
    )
    .await??;

    Ok(response)
}

#[test]
fn concurrent_bucket_creation_preserves_existing_configuration() -> TestResult {
    let directory = TestDirectory::new()?.retain_on_failure();

    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()?;

    test_stores::run(&runtime, async |stores| {
        let path = directory.path().join("store");
        let store = stores.open(OpenOptions::new(&path)).await?;
        let peer = stores.open(OpenOptions::new(&path)).await?;
        let server = store.listen(ListenOptions::default()).await?;
        let peer_server = peer.listen(ListenOptions::default()).await?;
        let policy = r#"{"Version":"2012-10-17","Statement":{"Effect":"Deny","Principal":"*","Action":"s3:GetObject","Resource":"arn:aws:s3:::raced/*"}}"#;

        for has_policy in [false, true] {
            if has_policy {
                store
                    .put_bucket_policy(PutBucketPolicyInput::builder("raced", policy).build()?)
                    .await?;
            }

            let mut requests = tokio::task::JoinSet::new();

            for listener in [&server, &peer_server] {
                let endpoint = listener.endpoint().to_owned();
                let credentials = listener.credentials();

                let raw = signed(
                    "PUT",
                    "/raced",
                    credentials.access_key_id(),
                    credentials.secret_access_key(),
                    "",
                );

                requests.spawn(async move {
                    exchange_endpoint(&endpoint, &raw)
                        .await
                        .map_err(|error| error.to_string())
                });
            }

            let mut completed = 0;

            while let Some(result) = requests.join_next().await {
                status(&result??, 200);
                completed += 1;
            }

            assert_eq!(completed, 2);

            peer.head_bucket(HeadBucketInput::builder("raced").build()?)
                .await?;

            if has_policy {
                assert_eq!(
                    peer.get_bucket_policy(GetBucketPolicyInput::builder("raced").build()?)
                        .await?
                        .document(),
                    policy
                );
            }
        }

        store.close().await?;
        peer.close().await?;
        let reopened = stores.open(OpenOptions::new(path)).await?;

        assert_eq!(
            reopened
                .get_bucket_policy(GetBucketPolicyInput::builder("raced").build()?)
                .await?
                .document(),
            policy
        );

        reopened.close().await?;
        Ok::<_, Box<dyn std::error::Error>>(())
    })?;

    directory.finish()?;
    Ok(())
}

#[test]
fn retired_bucket_rejects_already_streaming_http_write_after_name_reuse() -> TestResult {
    let directory = TestDirectory::new()?.retain_on_failure();

    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()?;

    test_stores::run(&runtime, async |stores| {
        for authority in ["application", "bootstrap"] {
            let path = directory.path().join(authority);
            let store = stores.open(OpenOptions::new(&path)).await?;
            let peer = stores.open(OpenOptions::new(&path)).await?;

            store
                .create_bucket(CreateBucketInput::builder("reused").build()?)
                .await?;

            let principal = store
                .create_principal(CreatePrincipalInput::builder("late-writer").build()?)
                .await?;

            store
                .put_principal_policy(
                    PutPrincipalPolicyInput::builder(
                        principal.principal_id(),
                        r#"{"Version":"2012-10-17","Statement":{"Effect":"Allow","Action":"s3:PutObject","Resource":"arn:aws:s3:::reused/*"}}"#,
                    )
                    .build()?,
                )
                .await?;

            let key = store
                .create_access_key(CreateAccessKeyInput::builder(principal.principal_id()).build()?)
                .await?;

            let server = store.listen(ListenOptions::default()).await?;
            let peer_server = peer.listen(ListenOptions::default()).await?;

            let (access_key_id, secret) = if authority == "application" {
                (key.access_key().access_key_id(), key.secret_access_key())
            } else {
                (
                    server.credentials().access_key_id(),
                    server.credentials().secret_access_key(),
                )
            };

            let raw = signed(
                "PUT",
                "/reused/late",
                access_key_id,
                secret,
                "complete candidate",
            );

            let boundary = raw
                .windows(4)
                .position(|bytes| bytes == b"\r\n\r\n")
                .ok_or("missing request header boundary")?
                + 4;

            let mut upload = tokio::net::TcpStream::connect(
                server
                    .endpoint()
                    .strip_prefix("http://")
                    .ok_or("missing HTTP origin")?,
            )
            .await?;

            upload.write_all(&raw[..=boundary]).await?;

            // A real staging byte proves this request reserved the original
            // bucket and entered native streaming before the namespace changes.
            tokio::time::timeout(Duration::from_secs(10), async {
                loop {
                    for shard in std::fs::read_dir(path.join("staging"))? {
                        for candidate in std::fs::read_dir(shard?.path())? {
                            let candidate = candidate?;

                            if candidate.metadata()?.len() == 1 {
                                assert_eq!(std::fs::read(candidate.path())?, b"c");
                                return Ok::<_, Box<dyn std::error::Error>>(());
                            }
                        }
                    }

                    tokio::time::sleep(Duration::from_millis(5)).await;
                }
            })
            .await??;

            let credentials = peer_server.credentials();

            for (method, expected) in [("DELETE", 204), ("PUT", 200)] {
                let request = signed(
                    method,
                    "/reused",
                    credentials.access_key_id(),
                    credentials.secret_access_key(),
                    "",
                );

                status(&exchange(&peer_server, &request).await?, expected);
            }

            peer.put_object(
                PutObjectInput::builder("reused", "winner", b"new namespace".to_vec()).build()?,
            )
            .await?;

            upload.write_all(&raw[boundary + 1..]).await?;
            let rejected = read_response(&mut upload).await?;
            status(&rejected, 404);

            assert!(
                rejected.contains("<Code>NoSuchBucket</Code>"),
                "{authority}"
            );

            store.close().await?;
            peer.close().await?;
            let reopened = stores.open(OpenOptions::new(path)).await?;

            let missing = reopened
                .get_object(GetObjectInput::builder("reused", "late").build()?)
                .await
                .err()
                .ok_or("old request wrote into the replacement bucket")?;

            assert_eq!(missing.code(), ErrorCode::NoSuchKey, "{authority}");

            let mut selected = reopened
                .get_object(GetObjectInput::builder("reused", "winner").build()?)
                .await?
                .into_body();

            assert_eq!(selected.read_all(64).await?, b"new namespace");
            reopened.close().await?;
        }

        Ok::<_, Box<dyn std::error::Error>>(())
    })?;

    directory.finish()?;
    Ok(())
}
