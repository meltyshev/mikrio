//! Actual localhost deadlines use the ordinary service's production budgets.

use super::*;

const DOWNLOAD_BYTES: u64 = 64 * 1024 * 1024;

#[test]
fn http_versions_reject_ambiguous_lengths_and_never_dispatch_a_pipelined_mutation() -> TestResult {
    with_fixture(|fixture| {
        status(
            &fixture.exchange(&request("PUT", "/photos/object", "predecessor", &[]))?,
            200,
        );

        for version in ["HTTP/1.0", "HTTP/1.1"] {
            report_case(format_args!("version={version}"));

            let valid = String::from_utf8(request("GET", "/photos/object", "", &[]))?
                .replacen("HTTP/1.1", version, 1);

            let response = fixture.exchange(valid.as_bytes())?;
            assert!(response.starts_with(&format!("{version} 200 ")));
            assert_eq!(response.split_once("\r\n\r\n").unwrap().1, "predecessor");

            let mut invalid =
                String::from_utf8(request("PUT", "/photos/object", "replacement", &[]))?
                    .replacen("HTTP/1.1", version, 1)
                    .replace(
                        "content-length: 11\r\n",
                        "content-length: 11\r\ncontent-length: 0\r\n",
                    )
                    .replace("connection: close", "connection: keep-alive")
                    .into_bytes();

            invalid.extend(request("PUT", "/photos/canary", "must-not-publish", &[]));
            let (response, outcome) = fixture.exchange_outcome(&invalid)?;
            assert!(outcome.is_err());
            assert!(response.contains(" 400 "));
            assert!(!response.contains(" 200 "));
            assert_eq!(bytes(fixture, "object"), b"predecessor");

            assert_eq!(
                fixture
                    .runtime
                    .block_on(fixture.connection.head_object(read("canary")))
                    .unwrap_err()
                    .failure(),
                mikrio_core::OperationFailure::NoSuchKey
            );
        }

        let (response, outcome) = fixture.exchange_outcome(b"PRI * HTTP/2.0\r\n\r\nSM\r\n\r\n")?;
        assert!(outcome.is_err());
        assert!(!response.contains(" 200 "));
        assert_eq!(bytes(fixture, "object"), b"predecessor");
        Ok(())
    })
}

#[test]
fn literal_ipv6_and_bucket_shaped_hosts_never_replace_path_style_bucket_routing() -> TestResult {
    with_fixture(|fixture| {
        status(
            &fixture.exchange(&request("PUT", "/photos/object", "predecessor", &[]))?,
            200,
        );

        for host in [
            "127.0.0.1:9000",
            "[::1]:9000",
            "photos.127.0.0.1:9000",
            "photos.localhost:9000",
        ] {
            report_case(format_args!("host={host}"));

            let response =
                fixture.exchange(&request("GET", "/photos/object", "", &[("host", host)]))?;

            status(&response, 200);
            assert_eq!(response.split_once("\r\n\r\n").unwrap().1, "predecessor");

            let response = fixture.exchange(&request("GET", "/object", "", &[("host", host)]))?;

            status(&response, 501);
            assert!(response.contains("<Code>NotImplemented</Code>"));
            assert!(!response.contains("predecessor"));
        }

        assert_eq!(bytes(fixture, "object"), b"predecessor");
        Ok(())
    })
}

#[test]
fn response_request_ids_are_unique_match_error_xml_and_ignore_supplied_ids() -> TestResult {
    with_fixture(|fixture| {
        let supplied = "00000000000000000000000000000000";
        let mut identifiers = std::collections::BTreeSet::new();

        for (method, path, body, expected_status) in [
            ("PUT", "/photos/request-identity", "original", 200),
            ("GET", "/photos/request-identity", "", 200),
            ("GET", "/photos/missing", "", 404),
            ("GET", "/photos/missing", "", 404),
            ("HEAD", "/photos/missing", "", 404),
        ] {
            report_case(format_args!("method={method}, status={expected_status}"));

            let response = fixture.exchange(&request(
                method,
                path,
                body,
                &[("x-amz-request-id", supplied)],
            ))?;

            status(&response, expected_status);
            let request_id = response_request_id(&response);
            assert_ne!(request_id, supplied);
            assert!(identifiers.insert(request_id.to_owned()));
            let (_, body) = response.split_once("\r\n\r\n").unwrap();

            if method == "HEAD" {
                assert_eq!(body, "");
            } else if expected_status == 404 {
                assert!(body.contains("<Code>NoSuchKey</Code>"));
                assert!(body.contains(&format!("<RequestId>{request_id}</RequestId>")));
            }
        }

        let response = fixture.exchange(&signed(
            "GET",
            "/photos/missing",
            "header",
            APP_KEY,
            "wrong-secret",
            "us-east-1",
            "s3",
            "",
        ))?;

        status(&response, 403);
        let request_id = response_request_id(&response);
        assert!(identifiers.insert(request_id.to_owned()));
        assert!(response.contains("<Code>SignatureDoesNotMatch</Code>"));
        assert!(response.contains(&format!("<RequestId>{request_id}</RequestId>")));
        assert!(!response.contains(APP_KEY));
        assert!(!response.contains(SECRET));
        assert!(!response.contains("wrong-secret"));
        assert_eq!(bytes(fixture, "request-identity"), b"original");

        for (method, headers, expected_status) in [
            ("GET", vec![("if-none-match", "*")], 304),
            ("DELETE", vec![], 204),
        ] {
            let response =
                fixture.exchange(&request(method, "/photos/request-identity", "", &headers))?;

            status(&response, expected_status);
            assert!(identifiers.insert(response_request_id(&response).to_owned()));
            assert_eq!(response.split_once("\r\n\r\n").unwrap().1, "");
        }

        let raw = b"GET /photos/missing HTTP/1.1\r\nHost: localhost\r\nAuthorization: one\r\nAuthorization: two\r\nConnection: close\r\n\r\n";
        let response = fixture.exchange(raw)?;
        status(&response, 400);
        assert!(identifiers.insert(response_request_id(&response).to_owned()));
        assert_eq!(response.split_once("\r\n\r\n").unwrap().1, "");
        Ok(())
    })
}

async fn seed_download(fixture: &Fixture<'_>) -> mikrio_core::object::StoredObject {
    use mikrio_core::integrity::{PutIntegrity, PutIntegrityInput};
    use mikrio_core::object::{
        ByteLength, PutRequest, Representation, UserMetadata, WriteConditions,
    };

    let request = PutRequest {
        bucket: BucketName::parse("photos").unwrap(),
        key: ObjectKey::parse("download").unwrap(),
        expected_owner: None,
        content_length: Some(ByteLength::new(DOWNLOAD_BYTES).unwrap()),
        representation: Representation::default(),
        metadata: UserMetadata::default(),
        tags: None,
        integrity: PutIntegrity::new(PutIntegrityInput::default()).unwrap(),
        conditions: WriteConditions::new(None, None).unwrap(),
    };

    fixture
        .connection
        .put_object(request, tokio::io::repeat(42).take(DOWNLOAD_BYTES))
        .await
        .unwrap();

    fixture
        .connection
        .head_object(read("download"))
        .await
        .unwrap()
}

async fn response_headers(socket: &mut tokio::net::TcpStream) -> String {
    let mut bytes = Vec::new();

    while !bytes.ends_with(b"\r\n\r\n") {
        assert!(bytes.len() < 8192);
        bytes.push(socket.read_u8().await.unwrap());
    }

    String::from_utf8(bytes).unwrap()
}

#[test]
fn stalled_download_peer_expires_without_a_successful_short_body() -> TestResult {
    with_fixture(|fixture| {
        let raw = request("GET", "/photos/download", "", &[]);

        fixture.runtime.block_on(async {
            seed_download(fixture).await;
            let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
            let address = listener.local_addr().unwrap();
            let (finished, stopped) = tokio::sync::oneshot::channel();

            let client = async {
                let mut socket = tokio::net::TcpStream::connect(address).await.unwrap();
                socket.write_all(&raw).await.unwrap();
                let headers = response_headers(&mut socket).await;
                status(&headers, 200);
                assert!(headers.contains(&format!("content-length: {DOWNLOAD_BYTES}\r\n")));
                // Observe the service's actual termination while the peer has
                // deliberately stopped reading. A watchdog alone is not that event.
                stopped.await.unwrap();
                let mut body = Vec::new();
                let result = socket.read_to_end(&mut body).await;

                assert!(
                    result.is_ok()
                        || result
                            .is_err_and(|error| error.kind() == std::io::ErrorKind::ConnectionReset)
                );

                assert!(u64::try_from(body.len()).unwrap() < DOWNLOAD_BYTES);
                assert!(body.iter().all(|byte| *byte == 42));
            };

            let server = async {
                let (socket, _) = listener.accept().await.unwrap();
                let started = tokio::time::Instant::now();
                assert!(fixture.service.serve(socket).await.is_err());
                assert!(started.elapsed() >= crate::progress::STALL_LIMIT);
                finished.send(()).unwrap();
            };

            tokio::time::timeout(
                Duration::from_secs(45),
                futures_util::future::join(client, server),
            )
            .await
            .unwrap();
        });

        Ok(())
    })
}

#[test]
fn truncated_native_file_after_response_start_terminates_the_http_transfer() -> TestResult {
    with_fixture(|fixture| {
        let raw = request("GET", "/photos/download", "", &[]);

        fixture.runtime.block_on(async {
            let selected = seed_download(fixture).await;

            let payload_path = fixture
                .connection
                .info()
                .directory
                .join("objects")
                .join(format!("{:02x}", selected.blob_id.shard()))
                .join(selected.blob_id.to_string());

            let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
            let address = listener.local_addr().unwrap();

            let client = async {
                let mut socket = tokio::net::TcpStream::connect(address).await.unwrap();
                socket.write_all(&raw).await.unwrap();
                status(&response_headers(&mut socket).await, 200);

                // Only this owned fixture file is corrupted, after selection and
                // response headers. Ordinary external mutation remains unsupported.
                std::fs::OpenOptions::new()
                    .write(true)
                    .open(payload_path)
                    .unwrap()
                    .set_len(0)
                    .unwrap();

                let mut body = Vec::new();
                let result = socket.read_to_end(&mut body).await;

                assert!(
                    result.is_ok()
                        || result
                            .is_err_and(|error| error.kind() == std::io::ErrorKind::ConnectionReset)
                );

                assert!(u64::try_from(body.len()).unwrap() < DOWNLOAD_BYTES);
                assert!(body.iter().all(|byte| *byte == 42));
            };

            let server = async {
                let (socket, _) = listener.accept().await.unwrap();
                assert!(fixture.service.serve(socket).await.is_err());
            };

            tokio::time::timeout(
                Duration::from_secs(10),
                futures_util::future::join(client, server),
            )
            .await
            .unwrap();
        });

        Ok(())
    })
}

#[test]
fn upload_transport_loss_preserves_predecessor_and_releases_owned_work() -> TestResult {
    with_fixture(|fixture| {
        status(
            &fixture.exchange(&request("PUT", "/photos/lost", "predecessor", &[]))?,
            200,
        );

        let raw = request("PUT", "/photos/lost", "replacement", &[]);

        fixture.runtime.block_on(async {
            let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
            let address = listener.local_addr().unwrap();

            let client = async {
                let mut socket = tokio::net::TcpStream::connect(address).await.unwrap();
                socket.write_all(&raw[..raw.len() - 1]).await.unwrap();
                reserved(fixture, "lost", 1).await;
                drop(socket);
            };

            let server = async {
                let (socket, _) = listener.accept().await.unwrap();
                // Depending on whether the peer FIN reaches the response write,
                // Hyper may report transport failure or finish the rejected reply.
                let _transport = fixture.service.serve(socket).await;
            };

            tokio::time::timeout(
                Duration::from_secs(10),
                futures_util::future::join(client, server),
            )
            .await
            .unwrap();
        });

        assert_eq!(bytes(fixture, "lost"), b"predecessor");
        Ok(())
    })
}

#[test]
fn core_close_fences_http_upload_while_an_independent_connection_remains_usable() -> TestResult {
    with_fixture(|fixture| {
        status(
            &fixture.exchange(&request("PUT", "/photos/closing", "predecessor", &[]))?,
            200,
        );

        let raw = request("PUT", "/photos/closing", "replacement", &[]);

        fixture.runtime.block_on(async {
            let peer = fixture.cleanup.track(
                Connection::open(fixture.connection.info().directory.clone(), None)
                    .await
                    .unwrap(),
            );

            let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
            let address = listener.local_addr().unwrap();

            let client = async {
                let mut socket = tokio::net::TcpStream::connect(address).await.unwrap();
                socket.write_all(&raw[..raw.len() - 1]).await.unwrap();
                reserved(fixture, "closing", 1).await;
                fixture.connection.close().await.unwrap();
                let mut response = String::new();
                socket.read_to_string(&mut response).await.unwrap();
                status(&response, 500);
                assert!(response.contains("<Code>InternalError</Code>"));
            };

            let server = async {
                let (socket, _) = listener.accept().await.unwrap();
                fixture.service.serve(socket).await.unwrap();
            };

            tokio::time::timeout(
                Duration::from_secs(10),
                futures_util::future::join(client, server),
            )
            .await
            .unwrap();

            let (_, mut body) = peer.get_object(read("closing")).await.unwrap();
            assert_eq!(body.read_all(100).await.unwrap(), b"predecessor");
            peer.close().await.unwrap();
        });

        Ok(())
    })
}

#[test]
fn core_close_cancels_selected_http_download_without_successful_short_eof() -> TestResult {
    with_fixture(|fixture| {
        let raw = request("GET", "/photos/download", "", &[]);

        fixture.runtime.block_on(async {
            seed_download(fixture).await;

            let peer = fixture.cleanup.track(
                Connection::open(fixture.connection.info().directory.clone(), None)
                    .await
                    .unwrap(),
            );

            let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
            let address = listener.local_addr().unwrap();

            let client = async {
                let mut socket = tokio::net::TcpStream::connect(address).await.unwrap();
                socket.write_all(&raw).await.unwrap();
                status(&response_headers(&mut socket).await, 200);
                fixture.connection.close().await.unwrap();
                let mut body = Vec::new();
                let result = socket.read_to_end(&mut body).await;

                assert!(
                    result.is_ok()
                        || result
                            .is_err_and(|error| error.kind() == std::io::ErrorKind::ConnectionReset)
                );

                assert!(u64::try_from(body.len()).unwrap() < DOWNLOAD_BYTES);
                assert!(body.iter().all(|byte| *byte == 42));
            };

            let server = async {
                let (socket, _) = listener.accept().await.unwrap();
                assert!(fixture.service.serve(socket).await.is_err());
            };

            tokio::time::timeout(
                Duration::from_secs(10),
                futures_util::future::join(client, server),
            )
            .await
            .unwrap();

            let (_, mut body) = peer.get_object(read("download")).await.unwrap();
            let mut total = 0u64;

            while let Some(chunk) = body.read_chunk(64 * 1024).await.unwrap() {
                assert!(chunk.iter().all(|byte| *byte == 42));
                total += u64::try_from(chunk.len()).unwrap();
            }

            assert_eq!(total, DOWNLOAD_BYTES);
            peer.close().await.unwrap();
        });

        Ok(())
    })
}

#[test]
fn healthy_upload_outlasts_the_production_stall_interval_and_persists() -> TestResult {
    with_fixture(|fixture| {
        let payload = "x".repeat(66);
        let raw = request("PUT", "/photos/slow", &payload, &[]);

        fixture.runtime.block_on(async {
            let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
            let address = listener.local_addr().unwrap();

            let client = async {
                let mut socket = tokio::net::TcpStream::connect(address).await.unwrap();

                socket
                    .write_all(&raw[..raw.len() - payload.len()])
                    .await
                    .unwrap();

                let started = tokio::time::Instant::now();

                for byte in payload.bytes() {
                    tokio::time::sleep(Duration::from_millis(500)).await;
                    socket.write_all(&[byte]).await.unwrap();
                }

                let mut response = String::new();
                socket.read_to_string(&mut response).await.unwrap();
                status(&response, 200);
                assert!(started.elapsed() > crate::progress::STALL_LIMIT);
            };

            let server = async {
                let (socket, _) = listener.accept().await.unwrap();
                fixture.service.serve(socket).await.unwrap();
            };

            tokio::time::timeout(
                Duration::from_secs(45),
                futures_util::future::join(client, server),
            )
            .await
            .unwrap();
        });

        assert_eq!(bytes(fixture, "slow"), payload.as_bytes());
        Ok(())
    })
}

#[test]
fn stalled_upload_times_out_and_cannot_publish_over_the_predecessor() -> TestResult {
    with_fixture(|fixture| {
        status(
            &fixture.exchange(&request("PUT", "/photos/stalled", "predecessor", &[]))?,
            200,
        );

        let raw = request("PUT", "/photos/stalled", "replacement", &[]);

        fixture.runtime.block_on(async {
            let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
            let address = listener.local_addr().unwrap();

            let client = async {
                let mut socket = tokio::net::TcpStream::connect(address).await.unwrap();
                socket.write_all(&raw[..raw.len() - 1]).await.unwrap();
                reserved(fixture, "stalled", 1).await;
                let mut response = String::new();
                socket.read_to_string(&mut response).await.unwrap();
                status(&response, 400);
                assert!(response.contains("<Code>RequestTimeout</Code>"));
            };

            let server = async {
                let (socket, _) = listener.accept().await.unwrap();
                fixture.service.serve(socket).await.unwrap();
            };

            tokio::time::timeout(
                Duration::from_secs(40),
                futures_util::future::join(client, server),
            )
            .await
            .unwrap();
        });

        assert_eq!(bytes(fixture, "stalled"), b"predecessor");
        Ok(())
    })
}

#[test]
fn incomplete_headers_have_a_finite_deadline_before_storage_admission() -> TestResult {
    with_fixture(|fixture| {
        fixture.runtime.block_on(async {
            let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
            let address = listener.local_addr().unwrap();

            let client = async {
                let mut socket = tokio::net::TcpStream::connect(address).await.unwrap();

                socket
                    .write_all(b"PUT /photos/header HTTP/1.1\r\nhost: ")
                    .await
                    .unwrap();

                let mut response = Vec::new();
                socket.read_to_end(&mut response).await.unwrap();
                assert!(response.is_empty() || response.starts_with(b"HTTP/1.1 408"));
            };

            let server = async {
                let (socket, _) = listener.accept().await.unwrap();
                assert!(fixture.service.serve(socket).await.is_err());
            };

            tokio::time::timeout(
                Duration::from_secs(8),
                futures_util::future::join(client, server),
            )
            .await
            .unwrap();
        });

        let rows: i64 = fixture
            .database
            .query_row("SELECT count(*) FROM blobs", [], |row| row.get(0))?;

        assert_eq!(rows, 0);
        Ok(())
    })
}

#[test]
fn header_count_and_query_envelope_reject_before_storage_and_leave_service_usable() -> TestResult {
    with_fixture(|fixture| {
        status(
            &fixture.exchange(&request("PUT", "/photos/envelope", "predecessor", &[]))?,
            200,
        );

        let baseline = request("GET", "/photos/envelope", "", &[]);
        let baseline = std::str::from_utf8(&baseline)?
            .split_once("\r\n\r\n")
            .unwrap()
            .0;

        let header_count = baseline.lines().count() - 1;

        for count in [100, 101] {
            let names: Vec<_> = (header_count..count)
                .map(|index| format!("x-probe-{index}"))
                .collect();

            let headers: Vec<_> = names.iter().map(|name| (name.as_str(), "")).collect();
            let raw = request("GET", "/photos/envelope", "", &headers);
            let (response, served) = fixture.exchange_outcome(&raw)?;

            if count == 100 {
                served?;
                status(&response, 200);
                assert!(response.ends_with("predecessor"));
            } else {
                assert!(served.is_err());
                status(&response, 431);
            }

            assert_eq!(bytes(fixture, "envelope"), b"predecessor");
        }

        // Query work is bounded by the request envelope, not a separate pair quota.
        let query = (0..128)
            .map(|index| format!("probe{index}=x"))
            .collect::<Vec<_>>()
            .join("&");

        let path = format!("/photos/envelope?{query}");
        status(&fixture.exchange(&request("GET", &path, "", &[]))?, 200);
        let path = format!("/photos/envelope?probe={}", "x".repeat(8192));
        let raw = request("GET", &path, "", &[]);
        assert!(raw.len() > 8192);
        // The server rejects the full buffer before the request line ends.
        // Withhold the rest so unread peer bytes do not reset its 431 response.
        let (response, served) = fixture.exchange_outcome(&raw[..8192])?;
        assert!(served.is_err());
        status(&response, 431);

        status(
            &fixture.exchange(&request("PUT", "/photos/envelope", "after rejection", &[]))?,
            200,
        );

        assert_eq!(bytes(fixture, "envelope"), b"after rejection");
        Ok(())
    })
}
