//! Real authenticated storage; observation sinks cannot satisfy these cases.

use std::io::Write as _;
use std::process::Stdio;

use mikrio_core::object::{BucketName, ObjectKey, ReadConditions, ReadRequest};

use super::*;

mod authorization;
mod bucket_listing;
mod conditions;
mod configuration;
mod copy;
mod cors;
mod deletion;
mod listing;
mod multipart;
mod object_layout;
mod options;
mod owners;
mod post;
mod ranges;
mod tagging;
mod transport;

pub(super) fn with_fixture(scenario: impl FnOnce(&mut Fixture<'_>) -> TestResult) -> TestResult {
    Fixture::run(false, |fixture| {
        fixture.database.execute("UPDATE principals SET policy_generation=randomblob(16), policy_json=?1", [
            r#"{"Version":"2012-10-17","Statement":{"Effect":"Allow","Action":["s3:GetObject","s3:PutObject","s3:DeleteObject","s3:ListBucket","s3:GetObjectTagging"],"Resource":"*"}}"#,
        ])?;

        fixture.service = Arc::new(Service::new(fixture.connection.clone(), None)?);
        scenario(fixture)
    })
}

fn request(method: &str, path: &str, body: &str, headers: &[(&str, &str)]) -> Vec<u8> {
    request_with_mode(method, path, body, headers, "header")
}

fn request_with_mode(
    method: &str,
    path: &str,
    body: &str,
    headers: &[(&str, &str)],
    mode: &str,
) -> Vec<u8> {
    let producer = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../tests/contract");
    let mut command = crate::tests::python::command();

    command
        .arg("-c")
        .arg(
            r#"
import sys
from urllib.parse import parse_qsl

sys.path.insert(0, sys.argv[1])
from http_sigv4 import request

method, target, mode, *fields = sys.argv[2:]
path, _, query = target.partition("?")

signed = request(
    method,
    path,
    mode,
    "mikrio_a00112233445566778899AABBCCDDEEFF",
    "synthetic-http-probe-secret",
    "us-east-1",
    "s3",
    sys.stdin.buffer.read(),
    dict(zip(fields[::2], fields[1::2])),
    dict(parse_qsl(query, keep_blank_values=True)),
)

sys.stdout.buffer.write(signed)
"#,
        )
        .arg(producer)
        .args([method, path, mode])
        .stdin(Stdio::piped())
        .stdout(Stdio::piped());

    for (name, value) in headers {
        command.args([name, value]);
    }

    let mut child = command.spawn().unwrap();

    let written = child.stdin.take().unwrap().write_all(body.as_bytes());
    // Reap the producer even when sending its input fails.
    let completed = child.wait_with_output();

    assert!(
        written.is_ok(),
        "request producer input failed: {written:?}; completion: {completed:?}"
    );

    let output = completed.unwrap();

    assert!(
        output.status.success(),
        "independent storage request producer failed"
    );

    output.stdout
}

pub(super) fn read(key: &str) -> ReadRequest {
    ReadRequest {
        bucket: BucketName::parse("photos").unwrap(),
        key: ObjectKey::parse(key).unwrap(),
        expected_owner: None,
        conditions: ReadConditions::default(),
        range: None,
        part_number: None,
        response: mikrio_core::object::Representation::default(),
    }
}

pub(super) fn bytes(fixture: &Fixture<'_>, key: &str) -> Vec<u8> {
    fixture.runtime.block_on(async {
        let (_, mut body) = fixture.connection.get_object(read(key)).await.unwrap();
        body.read_all(1024 * 1024).await.unwrap()
    })
}

#[test]
fn authenticated_put_get_head_delete_persist_shared_metadata_and_checksums() -> TestResult {
    with_fixture(|fixture| {
        let response = fixture.exchange(&request(
            "PUT",
            "/photos/object",
            "123456789",
            &[
                ("content-type", "text/plain"),
                ("x-amz-meta-title", "café"),
                ("cache-control", "private, max-age=0"),
                ("x-amz-checksum-crc32", "y/Q5Jg=="),
            ],
        ))?;

        status(&response, 200);
        assert!(response.contains("etag: \"25f9e794323b453885f5181f1b624d0b\""));
        assert!(response.contains("x-amz-checksum-crc32: y/Q5Jg=="));
        assert_eq!(bytes(fixture, "object"), b"123456789");

        let stored = fixture
            .runtime
            .block_on(fixture.connection.head_object(read("object")))?;

        assert_eq!(
            stored.metadata.entries().get("title").map(String::as_str),
            Some("café")
        );

        for method in ["GET", "HEAD"] {
            report_case(format_args!("method={method:?}"));

            let response = fixture.exchange(&request(
                method,
                "/photos/object",
                "",
                &[("x-amz-checksum-mode", "ENABLED")],
            ))?;

            status(&response, 200);

            for field in [
                "content-length: 9",
                "content-type: text/plain",
                "x-amz-checksum-crc32: y/Q5Jg==",
                "x-amz-checksum-type: FULL_OBJECT",
                "cache-control: private, max-age=0",
                "x-amz-meta-title: =?UTF-8?B?Y2Fmw6k=?=",
                "last-modified:",
            ] {
                assert!(response.contains(field), "missing {field}");
            }

            assert!(!response.contains("x-amz-tagging-count"));

            assert_eq!(
                response.split_once("\r\n\r\n").unwrap().1,
                if method == "GET" { "123456789" } else { "" }
            );
        }

        status(
            &fixture.exchange(&request("DELETE", "/photos/object", "", &[]))?,
            204,
        );

        status(
            &fixture.exchange(&request("GET", "/photos/object", "", &[]))?,
            404,
        );

        status(
            &fixture.exchange(&request("DELETE", "/photos/object", "", &[]))?,
            204,
        );

        Ok(())
    })
}

#[test]
fn not_modified_projects_cache_validators_only_from_the_authorized_snapshot() -> TestResult {
    with_fixture(|fixture| {
        let expires = "Wed, 21 Oct 2037 07:28:00 GMT";

        status(
            &fixture.exchange(&request(
                "PUT",
                "/photos/object",
                "value",
                &[
                    ("cache-control", "private, max-age=60"),
                    ("expires", expires),
                ],
            ))?,
            200,
        );

        let selected = fixture
            .runtime
            .block_on(fixture.connection.head_object(read("object")))?;

        for method in ["GET", "HEAD"] {
            report_case(format_args!("method={method:?}"));

            let response = fixture.exchange(&request(
                method,
                "/photos/object",
                "",
                &[("if-none-match", &selected.etag)],
            ))?;

            status(&response, 304);
            assert!(response.contains(&format!("etag: {}\r\n", selected.etag)));
            assert!(response.contains("cache-control: private, max-age=60\r\n"));
            assert!(response.contains(&format!("expires: {expires}\r\n")));
            assert!(response.contains("date:"));
            assert_eq!(response.split_once("\r\n\r\n").unwrap().1, "");
            assert!(!response.contains("content-length:"));
            assert!(!response.contains("x-amz-meta-"));
        }

        fixture.database.execute("DELETE FROM access_keys", [])?;

        let response = fixture.exchange(&request(
            "GET",
            "/photos/object",
            "",
            &[("if-none-match", &selected.etag)],
        ))?;

        status(&response, 403);
        assert!(!response.contains("etag:"));
        assert!(!response.contains("cache-control:"));
        assert!(!response.contains("expires:"));
        Ok(())
    })
}

#[test]
fn signed_raw_targets_persist_distinct_keys_and_reject_ambiguous_mutations() -> TestResult {
    with_fixture(|fixture| {
        for (index, (wire, logical)) in [
            ("/photos/a%2Fb", "a/b"),
            ("/photos/a%252Fb", "a%2Fb"),
            ("/photos/a%2Bb", "a+b"),
            ("/photos/a/../b//", "a/../b//"),
            ("/photos/%C3%A9", "é"),
        ]
        .iter()
        .enumerate()
        {
            report_case(format_args!(
                "index={index:?}, wire={wire:?}, logical={logical:?}"
            ));

            let payload = format!("distinct-{index}");

            status(
                &fixture.exchange(&request("PUT", wire, &payload, &[]))?,
                200,
            );

            assert_eq!(bytes(fixture, logical), payload.as_bytes());

            for mode in ["header", "query"] {
                report_case(format_args!(
                    "index={index:?}, wire={wire:?}, logical={logical:?}, mode={mode:?}"
                ));

                let response = fixture.exchange(&get(wire, mode))?;
                status(&response, 200);
                assert_eq!(response.split_once("\r\n\r\n").unwrap().1, payload);
            }

            let valid = String::from_utf8(request("PUT", wire, "altered", &[]))?;

            for inserted in [
                "authorization: invalid",
                "host: another",
                "content-length: 8",
                "transfer-encoding: chunked",
            ] {
                report_case(format_args!(
                    "index={index:?}, wire={wire:?}, logical={logical:?}, inserted={inserted:?}"
                ));

                let altered = valid.replace(
                    "connection: close",
                    &format!("{inserted}\r\nconnection: keep-alive"),
                );

                let mut pipeline = altered.into_bytes();
                pipeline.extend(request("DELETE", wire, "", &[]));
                let (response, outcome) = fixture.exchange_outcome(&pipeline)?;

                assert_eq!(
                    outcome.is_err(),
                    matches!(inserted, "content-length: 8" | "transfer-encoding: chunked")
                );

                status(&response, 400);
                assert_eq!(response.matches("HTTP/1.1 ").count(), 1);
                assert_eq!(bytes(fixture, logical), payload.as_bytes());
            }
        }

        Ok(())
    })
}

#[test]
fn metadata_contention_returns_service_unavailable_and_preserves_current_bytes() -> TestResult {
    with_fixture(|fixture| {
        status(
            &fixture.exchange(&request("PUT", "/photos/busy", "predecessor", &[]))?,
            200,
        );

        fixture.database.execute_batch("BEGIN IMMEDIATE")?;
        let response = fixture.exchange(&request("PUT", "/photos/busy", "replacement", &[]))?;
        fixture.database.execute_batch("ROLLBACK")?;
        status(&response, 503);
        assert!(response.contains("<Code>ServiceUnavailable</Code>"));
        assert_eq!(bytes(fixture, "busy"), b"predecessor");
        Ok(())
    })
}

#[test]
fn malformed_delete_envelopes_cannot_remove_the_selected_object() -> TestResult {
    with_fixture(|fixture| {
        status(
            &fixture.exchange(&request("PUT", "/photos/object", "predecessor", &[]))?,
            200,
        );

        for digest in ["not-base64", "AAAAAAAAAAAAAAAAAAAAAA=="] {
            report_case(format_args!("digest={digest:?}"));

            status(
                &fixture.exchange(&request(
                    "DELETE",
                    "/photos/object",
                    "",
                    &[("content-md5", digest)],
                ))?,
                400,
            );

            assert_eq!(bytes(fixture, "object"), b"predecessor");
        }

        for body in ["unexpected", ""] {
            report_case(format_args!("body={body:?}"));

            let mut raw = request("DELETE", "/photos/object", body, &[]);

            if body.is_empty() {
                let raw_text = String::from_utf8(raw)?;

                raw = raw_text
                    .replace("content-length: 0", "transfer-encoding: chunked")
                    .into_bytes();

                // A nonempty chunk violates DELETE's empty-body contract even when
                // its signed digest declares empty bytes. No terminal chunk follows.
                raw.extend_from_slice(b"1\r\nx\r\n");
            }

            let response = fixture.exchange(&raw)?;
            status(&response, 400);
            assert_eq!(bytes(fixture, "object"), b"predecessor");
        }

        Ok(())
    })
}

#[test]
fn checksum_headers_trailers_and_signed_terminal_failures_preserve_predecessor() -> TestResult {
    with_fixture(|fixture| {
        for algorithm in ["CRC32", "CRC32C", "CRC64NVME", "SHA1", "SHA256"] {
            for transport in ["header", "trailer"] {
                report_case(format_args!(
                    "algorithm={algorithm:?}, transport={transport:?}"
                ));

                let mode = format!("flex:{algorithm}:{transport}");
                let raw = integrity::raw(&mode);
                status(&fixture.exchange(&raw)?, 200);
                assert_eq!(bytes(fixture, "integrity"), b"123456789");

                let stored = fixture
                    .runtime
                    .block_on(fixture.connection.head_object(read("integrity")))?;

                assert!(stored.representation.fields().content_encoding.is_none());
            }

            for transport in ["header-bad", "trailer-bad", "trailer-truncated"] {
                report_case(format_args!(
                    "algorithm={algorithm:?}, transport={transport:?}"
                ));

                let response =
                    fixture.exchange(&integrity::raw(&format!("flex:{algorithm}:{transport}")))?;

                status(&response, 400);
                assert_eq!(bytes(fixture, "integrity"), b"123456789");
            }
        }

        for mode in [
            "bad-md5",
            "missing-trailer",
            "duplicate-trailer",
            "extra-decoded",
            "short-decoded",
            "missing-final-crlf",
            "missing-completion",
        ] {
            report_case(format_args!("mode={mode:?}"));

            status(&fixture.exchange(&integrity::raw(mode))?, 400);
            assert_eq!(bytes(fixture, "integrity"), b"123456789");
        }

        Ok(())
    })
}

#[test]
fn decoder_failures_keep_protocol_codes_and_preserve_predecessor() -> TestResult {
    with_fixture(|fixture| {
        status(
            &fixture.exchange(&request("PUT", "/photos/integrity", "predecessor", &[]))?,
            200,
        );

        for (mode, expected_status, expected_code) in [
            ("extra-decoded", 400, "IncompleteBody"),
            ("short-decoded", 400, "IncompleteBody"),
            ("missing-completion", 400, "IncompleteBody"),
            ("signed-missing-completion", 400, "IncompleteBody"),
            ("signed-truncated", 400, "IncompleteBody"),
            ("signed-bad-data", 403, "SignatureDoesNotMatch"),
            ("signed-bad-completion", 403, "SignatureDoesNotMatch"),
            ("bad-header", 400, "BadDigest"),
            ("bad-trailer", 400, "BadDigest"),
        ] {
            report_case(format_args!(
                "mode={mode:?}, expected_status={expected_status:?}, expected_code={expected_code:?}"
            ));

            let response = fixture.exchange(&integrity::raw(mode))?;
            status(&response, expected_status);

            assert!(
                response.contains(&format!("<Code>{expected_code}</Code>")),
                "{mode}: {response}"
            );

            assert_eq!(bytes(fixture, "integrity"), b"predecessor", "{mode}");
        }

        Ok(())
    })
}

#[test]
fn current_access_precedes_conditions_and_missing_key_disclosure() -> TestResult {
    with_fixture(|fixture| {
        status(
            &fixture.exchange(&request("PUT", "/photos/object", "value", &[]))?,
            200,
        );

        let etag = "\"2063c1608d6e0baf80249c42e2be5804\"";

        status(
            &fixture.exchange(&request(
                "GET",
                "/photos/object",
                "",
                &[("if-none-match", etag)],
            ))?,
            304,
        );

        for method in ["GET", "HEAD"] {
            report_case(format_args!("method={method:?}"));

            let response = fixture.exchange(&request(
                method,
                "/photos/object",
                "",
                &[("if-match", "\"wrong\"")],
            ))?;

            status(&response, 412);

            if method == "HEAD" {
                assert!(response.ends_with("\r\n\r\n"));
            }
        }

        fixture.database.execute("UPDATE principals SET policy_generation=randomblob(16), policy_json=?1", [
            r#"{"Version":"2012-10-17","Statement":{"Effect":"Allow","Action":"s3:PutObject","Resource":"*"}}"#,
        ])?;

        status(
            &fixture.exchange(&request(
                "GET",
                "/photos/object",
                "",
                &[("if-none-match", etag)],
            ))?,
            403,
        );

        status(
            &fixture.exchange(&request(
                "PUT",
                "/photos/object",
                "replacement",
                &[("if-match", "*")],
            ))?,
            403,
        );

        assert_eq!(bytes(fixture, "object"), b"value");

        fixture.database.execute("UPDATE principals SET policy_generation=randomblob(16), policy_json=?1", [
            r#"{"Version":"2012-10-17","Statement":{"Effect":"Allow","Action":"s3:GetObject","Resource":"*"}}"#,
        ])?;

        status(
            &fixture.exchange(&request("GET", "/photos/missing", "", &[]))?,
            403,
        );

        status(&fixture.exchange(unsigned())?, 403);
        Ok(())
    })
}

#[test]
fn unimplemented_options_and_ambiguous_pipeline_cannot_change_storage() -> TestResult {
    with_fixture(|fixture| {
        status(
            &fixture.exchange(&request("PUT", "/photos/object", "value", &[]))?,
            200,
        );

        for (method, header, value) in [
            ("PUT", "x-amz-server-side-encryption", "AES256"),
            ("PUT", "x-amz-storage-class", "GLACIER"),
            ("HEAD", "x-amz-request-payer", "requester"),
            ("DELETE", "x-amz-if-match-size", "5"),
        ] {
            report_case(format_args!(
                "method={method:?}, header={header:?}, value={value:?}"
            ));

            status(
                &fixture.exchange(&request(
                    method,
                    "/photos/object",
                    if method == "PUT" { "changed" } else { "" },
                    &[(header, value)],
                ))?,
                501,
            );

            assert_eq!(bytes(fixture, "object"), b"value");
        }

        let raw = String::from_utf8(request("PUT", "/photos/object", "changed", &[]))?.replace(
            "connection: close",
            "authorization: invalid\r\nconnection: keep-alive",
        );

        let mut pipeline = raw.into_bytes();
        pipeline.extend(request("DELETE", "/photos/object", "", &[]));
        let response = fixture.exchange(&pipeline)?;
        status(&response, 400);
        assert_eq!(response.matches("HTTP/1.1 ").count(), 1);
        assert_eq!(bytes(fixture, "object"), b"value");
        Ok(())
    })
}

async fn reserved(fixture: &Fixture<'_>, key: &str, count: i64) {
    tokio::time::timeout(Duration::from_secs(5), async {
        loop {
            let actual: i64 = fixture
                .database
                .query_row(
                    "SELECT count(*) FROM blobs WHERE state='reserved' AND target_key=?1",
                    [key.as_bytes()],
                    |row| row.get(0),
                )
                .unwrap();

            if actual == count {
                return;
            }

            tokio::time::sleep(Duration::from_millis(2)).await;
        }
    })
    .await
    .unwrap();
}

#[test]
fn upload_revocation_on_each_side_of_actual_publication_has_current_authority() -> TestResult {
    with_fixture(|fixture| {
        for revoke_before in [true, false] {
            report_case(format_args!("revoke_before={revoke_before:?}"));

            status(
                &fixture.exchange(&request("PUT", "/photos/object", "predecessor", &[]))?,
                200,
            );

            let raw = request("PUT", "/photos/object", "replacement", &[]);

            fixture.runtime.block_on(async {
                let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
                let address = listener.local_addr().unwrap();

                let client = async {
                    let mut socket = tokio::net::TcpStream::connect(address).await.unwrap();
                    let split = raw.len() - 5;
                    socket.write_all(&raw[..split]).await.unwrap();
                    reserved(fixture, "object", 1).await;

                    if revoke_before {
                        fixture
                            .database
                            .execute("DELETE FROM access_keys", [])
                            .unwrap();
                    }

                    socket.write_all(&raw[split..]).await.unwrap();
                    let mut response = String::new();
                    socket.read_to_string(&mut response).await.unwrap();
                    status(&response, if revoke_before { 403 } else { 200 });

                    if !revoke_before {
                        // The acknowledged core publication precedes this committed
                        // revocation; it is not retroactively undone.
                        fixture
                            .database
                            .execute("DELETE FROM access_keys", [])
                            .unwrap();
                    }
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
            });

            assert_eq!(
                bytes(fixture, "object"),
                if revoke_before {
                    b"predecessor".as_slice()
                } else {
                    b"replacement".as_slice()
                }
            );

            status(&fixture.exchange(&get("/photos/object", "header"))?, 403);

            fixture.database.execute(
                "INSERT INTO access_keys(access_key_id, principal_id, secret_access_key, created_at_ms) VALUES(?1,zeroblob(16),?2,0)",
                [APP_KEY, SECRET],
            )?;
        }

        Ok(())
    })
}

#[test]
fn simultaneous_create_only_uploads_publish_exactly_one_payload() -> TestResult {
    with_fixture(|fixture| {
        let first = request("PUT", "/photos/race", "first", &[("if-none-match", "*")]);
        let second = request("PUT", "/photos/race", "second", &[("if-none-match", "*")]);

        fixture.runtime.block_on(async {
            let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
            let address = listener.local_addr().unwrap();

            let client = async {
                let mut first_socket = tokio::net::TcpStream::connect(address).await.unwrap();
                let mut second_socket = tokio::net::TcpStream::connect(address).await.unwrap();

                first_socket
                    .write_all(&first[..first.len() - 1])
                    .await
                    .unwrap();

                second_socket
                    .write_all(&second[..second.len() - 1])
                    .await
                    .unwrap();

                reserved(fixture, "race", 2).await;

                first_socket
                    .write_all(&first[first.len() - 1..])
                    .await
                    .unwrap();

                second_socket
                    .write_all(&second[second.len() - 1..])
                    .await
                    .unwrap();

                let mut first_response = String::new();
                let mut second_response = String::new();

                first_socket
                    .read_to_string(&mut first_response)
                    .await
                    .unwrap();

                second_socket
                    .read_to_string(&mut second_response)
                    .await
                    .unwrap();

                let mut statuses = [
                    first_response.split_whitespace().nth(1).unwrap().to_owned(),
                    second_response
                        .split_whitespace()
                        .nth(1)
                        .unwrap()
                        .to_owned(),
                ];

                statuses.sort();
                assert_eq!(statuses, ["200", "412"]);
            };

            let server = async {
                let (first, _) = listener.accept().await.unwrap();
                let (second, _) = listener.accept().await.unwrap();

                let (first, second) = futures_util::future::join(
                    fixture.service.serve(first),
                    fixture.service.serve(second),
                )
                .await;

                first.unwrap();
                second.unwrap();
            };

            tokio::time::timeout(
                Duration::from_secs(10),
                futures_util::future::join(client, server),
            )
            .await
            .unwrap();
        });

        assert!(
            [b"first".as_slice(), b"second".as_slice()]
                .contains(&bytes(fixture, "race").as_slice())
        );

        Ok(())
    })
}

#[test]
fn tag_count_and_tag_conditions_follow_current_authorized_read_snapshot() -> TestResult {
    with_fixture(|fixture| {
        status(
            &fixture.exchange(&request("PUT", "/photos/object", "value", &[]))?,
            200,
        );

        let tags = mikrio_core::object::ObjectTags::new([("visibility".into(), "public".into())])?;

        fixture
            .runtime
            .block_on(fixture.connection.replace_object_tags(read("object"), tags))?;

        for method in ["GET", "HEAD"] {
            report_case(format_args!("method={method:?}"));

            let response = fixture.exchange(&request(method, "/photos/object", "", &[]))?;
            status(&response, 200);
            assert!(response.contains("x-amz-tagging-count: 1"));
        }

        fixture.database.execute("UPDATE principals SET policy_generation=randomblob(16), policy_json=?1", [
            r#"{"Version":"2012-10-17","Statement":{"Effect":"Allow","Action":"s3:GetObject","Resource":"*","Condition":{"StringEquals":{"s3:ExistingObjectTag/visibility":"public"}}}}"#,
        ])?;

        let response = fixture.exchange(&get("/photos/object", "header"))?;
        status(&response, 200);
        assert!(!response.contains("x-amz-tagging-count"));

        fixture.runtime.block_on(
            fixture
                .connection
                .replace_object_tags(read("object"), mikrio_core::object::ObjectTags::default()),
        )?;

        for method in ["GET", "HEAD"] {
            report_case(format_args!("method={method:?}"));

            let response = fixture.exchange(&request(
                method,
                "/photos/object",
                "",
                &[("if-none-match", "*")],
            ))?;

            status(&response, 403);

            if method == "HEAD" {
                assert!(response.ends_with("\r\n\r\n"));
            }
        }

        Ok(())
    })
}
