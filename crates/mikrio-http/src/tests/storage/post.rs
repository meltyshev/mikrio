//! Real form publication, authorization and terminal rejection over shared storage.

use super::*;

const ADMIN: &str =
    r#"{"Version":"2012-10-17","Statement":{"Effect":"Allow","Action":"s3:*","Resource":"*"}}"#;

fn policy(fixture: &Fixture<'_>, document: &str) -> TestResult {
    fixture.database.execute(
        "UPDATE principals SET policy_generation=randomblob(16), policy_json=?1",
        [document],
    )?;

    Ok(())
}

#[test]
fn durable_post_publishes_form_fields_tags_file_checksums_and_reusable_replacement() -> TestResult {
    with_fixture(|fixture| {
        policy(fixture, ADMIN)?;
        status(&fixture.exchange(&form("form-fields"))?, 204);
        assert_eq!(bytes(fixture, "album/a.txt"), b"payload");

        let stored = fixture
            .runtime
            .block_on(fixture.connection.head_object(read("album/a.txt")))?;

        assert_eq!(
            stored.representation.fields().content_type.as_deref(),
            Some("image/png")
        );

        assert_eq!(
            stored.metadata.entries().get("note").map(String::as_str),
            Some("form metadata")
        );

        assert_eq!(
            stored.tags.entries().get("Label").map(String::as_str),
            Some("café")
        );

        let original = stored.etag.clone();

        // The same policy is reusable; no one-use record or attachment promotion.
        let issued = form("valid");
        status(&fixture.exchange(&issued)?, 204);

        let replay =
            String::from_utf8(issued)?.replace("\r\n\r\npayload\r\n", "\r\n\r\nchanged\r\n");

        status(&fixture.exchange(replay.as_bytes())?, 204);
        assert_eq!(bytes(fixture, "album/a.txt"), b"changed");

        let stored = fixture
            .runtime
            .block_on(fixture.connection.head_object(read("album/a.txt")))?;

        assert_ne!(stored.etag, original);
        assert!(stored.tags.entries().is_empty());
        assert!(stored.metadata.entries().is_empty());
        assert_eq!(stored.representation.fields().content_type, None);

        assert_eq!(
            stored.representation.effective_content_type(),
            "binary/octet-stream"
        );

        let checksum = fixture.exchange(&form("checksums"))?;
        status(&checksum, 204);
        assert!(checksum.contains("x-amz-checksum-crc32:"));
        assert_eq!(bytes(fixture, "album/a.txt"), b"payload");

        for (mode, expected) in [
            ("201", 201),
            ("bad-status", 204),
            ("bad-redirect-201", 201),
            ("redirect", 303),
            ("ignored-tail", 204),
            ("range-at-limit", 204),
            ("durable-empty-tags", 204),
        ] {
            report_case(format_args!("mode={mode:?}, expected={expected:?}"));

            status(&fixture.exchange(&form(mode))?, expected);
            assert_eq!(bytes(fixture, "album/a.txt"), b"payload");
        }

        Ok(())
    })
}

#[test]
fn outer_checksum_assertions_cannot_be_ignored_by_durable_post() -> TestResult {
    with_fixture(|fixture| {
        policy(fixture, ADMIN)?;

        status(
            &fixture.exchange(&request("PUT", "/photos/album/a.txt", "predecessor", &[]))?,
            200,
        );

        let sequence: i64 = fixture.database.query_row(
            "SELECT seq FROM sqlite_sequence WHERE name = 'blobs'",
            [],
            |row| row.get(0),
        )?;

        let raw = String::from_utf8(form("valid"))?;

        for (name, value) in [
            ("x-amz-checksum-type", "COMPOSITE"),
            ("x-amz-checksum-crc16", "AAAA"),
            ("x-amz-checksum-crc32", "NSRBwg=="),
            ("x-amz-checksum-algorithm", "CRC32"),
            ("x-amz-sdk-checksum-algorithm", "CRC32"),
            ("x-amz-checksum-mode", "ENABLED"),
            ("content-md5", "kAFQmDzST7DWlj99KOF/cg=="),
        ] {
            for value in [value, ""] {
                report_case(format_args!("name={name:?}, value={value:?}"));

                // Form signatures authenticate fields, not these outer headers.
                // Unsupported assertions must fail rather than claim verification.
                let request = raw.replacen("\r\n", &format!("\r\n{name}: {value}\r\n"), 1);
                let response = fixture.exchange(request.as_bytes())?;

                assert!(
                    response.starts_with("HTTP/1.1 501 "),
                    "{name}={value:?}: {response}"
                );

                assert!(response.contains("<Code>NotImplemented</Code>"));
                assert_eq!(bytes(fixture, "album/a.txt"), b"predecessor");

                let current: i64 = fixture.database.query_row(
                    "SELECT seq FROM sqlite_sequence WHERE name = 'blobs'",
                    [],
                    |row| row.get(0),
                )?;

                assert_eq!(current, sequence);
            }
        }

        status(&fixture.exchange(&form("checksums"))?, 204);
        assert_eq!(bytes(fixture, "album/a.txt"), b"payload");
        Ok(())
    })
}

#[test]
fn durable_post_invalid_tail_size_digest_and_options_preserve_the_predecessor() -> TestResult {
    with_fixture(|fixture| {
        policy(fixture, ADMIN)?;

        status(
            &fixture.exchange(&request("PUT", "/photos/album/a.txt", "predecessor", &[]))?,
            200,
        );

        let original = fixture
            .runtime
            .block_on(fixture.connection.head_object(read("album/a.txt")))?;

        for (mode, code) in [
            ("extra-file", "MalformedPOSTRequest"),
            ("epilogue", "MalformedPOSTRequest"),
            ("tail-over-limit", "MalformedPOSTRequest"),
            ("range-too-short", "EntityTooSmall"),
            ("range-too-long", "EntityTooLarge"),
            ("bad-form-md5", "BadDigest"),
            ("bad-form-crc32", "BadDigest"),
            ("durable-empty-md5", "InvalidDigest"),
            ("durable-algorithm-only", "InvalidRequest"),
            ("durable-algorithm-mismatch", "BadDigest"),
            ("durable-private-acl", "AccessControlListNotSupported"),
            ("durable-tag-tail", "MalformedXML"),
            ("form-fields-empty", "MalformedXML"),
            ("field-if-none-match", "InvalidArgument"),
            ("field-outer-if-none-match", "InvalidArgument"),
            ("redirect-control", "InvalidArgument"),
        ] {
            let response = fixture.exchange(&form(mode))?;
            assert!(response.starts_with("HTTP/1.1 4"), "{mode}: {response}");

            assert!(
                response.contains(&format!("<Code>{code}</Code>")),
                "{mode}: {response}"
            );

            assert_eq!(bytes(fixture, "album/a.txt"), b"predecessor", "{mode}");

            let selected = fixture
                .runtime
                .block_on(fixture.connection.head_object(read("album/a.txt")))?;

            assert_eq!(selected.etag, original.etag, "{mode}");
            assert_eq!(selected.last_modified, original.last_modified, "{mode}");
        }

        for mode in ["truncated", "tail-truncated", "redirect-truncated"] {
            report_case(format_args!("mode={mode:?}"));

            assert!(
                fixture.exchange(&form(mode))?.starts_with("HTTP/1.1 4"),
                "{mode}"
            );

            assert_eq!(bytes(fixture, "album/a.txt"), b"predecessor");
        }

        Ok(())
    })
}

#[test]
fn durable_post_uses_creation_and_request_tag_authority_without_inventing_preconditions()
-> TestResult {
    with_fixture(|fixture| {
        // Existing PUT-only policy can create without tagging but not submit an empty tag set.
        status(&fixture.exchange(&form("valid"))?, 204);
        status(&fixture.exchange(&form("durable-empty-tags"))?, 403);

        policy(
            fixture,
            r#"{"Version":"2012-10-17","Statement":{"Effect":"Allow","Action":["s3:PutObject","s3:PutObjectTagging"],"Resource":"*","Condition":{"StringEquals":{"s3:RequestObjectTag/Label":"café"},"Bool":{"s3:ObjectCreationOperation":"true"}}}}"#,
        )?;

        status(&fixture.exchange(&form("form-fields"))?, 204);
        status(&fixture.exchange(&form("valid"))?, 403);

        policy(
            fixture,
            r#"{"Version":"2012-10-17","Statement":{"Effect":"Allow","Action":"s3:PutObject","Resource":"*","Condition":{"StringEquals":{"s3:if-none-match":"*"}}}}"#,
        )?;

        status(&fixture.exchange(&form("valid"))?, 403);

        let stored = fixture
            .runtime
            .block_on(fixture.connection.head_object(read("album/a.txt")))?;

        assert_eq!(
            stored.tags.entries().get("Label").map(String::as_str),
            Some("café")
        );

        Ok(())
    })
}

#[test]
fn durable_post_terminal_barrier_rechecks_revocation_and_cancels_without_publication() -> TestResult
{
    for mode in [
        "policy",
        "credential",
        "close",
        "disconnect",
        "commit",
        "bucket",
        "publication",
    ] {
        report_case(format_args!("mode={mode:?}"));

        with_fixture(|fixture| {
            policy(fixture, ADMIN)?;

            status(
                &fixture.exchange(&request("PUT", "/photos/album/a.txt", "predecessor", &[]))?,
                200,
            );

            fixture.runtime.block_on(terminal_case(fixture, mode))?;
            Ok(())
        })?;
    }

    Ok(())
}

async fn terminal_case(fixture: &Fixture<'_>, mode: &str) -> TestResult {
    let raw = form("valid");

    let peer = fixture
        .cleanup
        .track(Connection::open(fixture.connection.info().directory.clone(), None).await?);

    let (_, mut predecessor) = peer.get_object(read("album/a.txt")).await?;
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await?;
    let address = listener.local_addr()?;

    let client = async {
        let mut socket = tokio::net::TcpStream::connect(address).await?;
        // File bytes are present; a missing final form byte keeps publication
        // blocked. Observe a real reservation, not merely decoder admission.
        socket.write_all(&raw[..raw.len() - 1]).await?;
        reserved(fixture, "album/a.txt", 1).await;
        let (_, mut current) = peer.get_object(read("album/a.txt")).await?;
        assert_eq!(current.read_all(100).await?, b"predecessor");

        match mode {
            "policy" => policy(
                fixture,
                r#"{"Version":"2012-10-17","Statement":{"Effect":"Deny","Action":"s3:*","Resource":"*"}}"#,
            )?,
            "credential" => {
                fixture.database.execute("DELETE FROM access_keys", [])?;
            }
            "close" => fixture.connection.close().await?,
            "disconnect" => return Ok::<_, Box<dyn std::error::Error>>(()),
            "commit" => {}
            "bucket" => {
                peer.delete_object(read("album/a.txt"), None).await?;

                peer.delete_bucket(BucketName::parse("photos")?, None)
                    .await?;

                peer.create_bucket(BucketName::parse("photos")?).await?;
            }
            "publication" => {
                // A real SQLite error after reservation/payload processing forces
                // the ordinary publication transaction to roll back atomically.
                fixture.database.execute_batch(
                    "CREATE TRIGGER reject_post BEFORE INSERT ON objects
                     BEGIN SELECT RAISE(ABORT, 'synthetic publication failure'); END;",
                )?;
            }
            _ => return Err("unknown POST terminal scenario".into()),
        }

        if mode != "close" {
            socket.write_all(&raw[raw.len() - 1..]).await?;
        }

        let mut response = String::new();
        socket.read_to_string(&mut response).await?;

        let expected = match mode {
            "commit" => 204,
            "close" | "publication" => 500,
            "bucket" => 404,
            _ => 403,
        };

        status(&response, expected);

        if mode == "commit" {
            fixture.database.execute("DELETE FROM access_keys", [])?;
        }

        if mode == "publication" {
            fixture.database.execute_batch("DROP TRIGGER reject_post")?;
        }

        Ok(())
    };

    let server = async {
        let (socket, _) = listener.accept().await?;
        let _transport = fixture.service.serve(socket).await;
        Ok::<_, std::io::Error>(())
    };

    let (client, server) = tokio::time::timeout(
        Duration::from_secs(15),
        futures_util::future::join(client, server),
    )
    .await?;

    client?;
    server?;
    assert_eq!(predecessor.read_all(100).await?, b"predecessor");

    if mode == "bucket" {
        assert_eq!(
            peer.head_object(read("album/a.txt"))
                .await
                .unwrap_err()
                .failure(),
            mikrio_core::OperationFailure::NoSuchKey,
        );
    } else {
        let (_, mut current) = peer.get_object(read("album/a.txt")).await?;

        let expected = if mode == "commit" {
            b"payload".as_slice()
        } else {
            b"predecessor".as_slice()
        };

        assert_eq!(current.read_all(100).await?, expected);
    }

    peer.close().await?;

    Ok(())
}

#[test]
fn durable_post_owned_listener_close_settles_native_work_and_preserves_independent_access()
-> TestResult {
    with_fixture(|fixture| {
        policy(fixture, ADMIN)?;

        status(
            &fixture.exchange(&request("PUT", "/photos/album/a.txt", "predecessor", &[]))?,
            200,
        );

        fixture.runtime.block_on(async {
            let listener = Listener::bind(
                &fixture.connection,
                "127.0.0.1",
                0,
                None,
                Arc::new(mikrio_core::access::Bootstrap::generate()?),
            )
            .await?;

            let address = listener
                .endpoint()
                .strip_prefix("http://")
                .ok_or("invalid test endpoint")?;

            let mut socket = tokio::net::TcpStream::connect(address).await?;
            let raw = form("valid");
            socket.write_all(&raw[..raw.len() - 1]).await?;
            reserved(fixture, "album/a.txt", 1).await;
            tokio::time::timeout(Duration::from_secs(15), listener.close()).await??;
            reserved(fixture, "album/a.txt", 0).await;
            let (_, mut current) = fixture.connection.get_object(read("album/a.txt")).await?;
            assert_eq!(current.read_all(100).await?, b"predecessor");
            Ok::<_, Box<dyn std::error::Error>>(())
        })?;

        Ok(())
    })
}

#[test]
fn durable_post_malformed_envelope_cannot_dispatch_an_embedded_next_request() -> TestResult {
    with_fixture(|fixture| {
        policy(fixture, ADMIN)?;

        status(
            &fixture.exchange(&request("PUT", "/photos/album/a.txt", "predecessor", &[]))?,
            200,
        );

        let embedded = request("DELETE", "/photos/album/a.txt", "", &[]);
        let raw = String::from_utf8(form("tail-truncated"))?;
        let (headers, body) = raw.split_once("\r\n\r\n").ok_or("missing test headers")?;

        let headers = headers
            .replace("connection: close", "connection: keep-alive")
            .replace(
                &format!("content-length: {}", body.len()),
                &format!("content-length: {}", body.len() + embedded.len()),
            );

        let mut raw = format!("{headers}\r\n\r\n{body}").into_bytes();
        // These bytes belong to the incomplete declared POST envelope, not a
        // subsequent request; either parser error or transport closure is allowed.
        raw.extend_from_slice(&embedded);
        raw.extend_from_slice(&request("GET", "/photos/album/a.txt", "", &[]));
        let (response, _transport) = fixture.exchange_outcome(&raw)?;
        assert!(response.starts_with("HTTP/1.1 4"));
        assert!(!response.contains("HTTP/1.1 204"));
        assert!(response.matches("HTTP/1.1 ").count() <= 2);
        assert_eq!(bytes(fixture, "album/a.txt"), b"predecessor");
        Ok(())
    })
}
