//! Raw option presence must survive the codec's empty-header conversion.

use super::*;

fn unchanged(fixture: &Fixture<'_>, sequence: i64) -> TestResult {
    let current: i64 = fixture.database.query_row(
        "SELECT seq FROM sqlite_sequence WHERE name = 'blobs'",
        [],
        |row| row.get(0),
    )?;

    assert_eq!(current, sequence);
    assert_eq!(bytes(fixture, "object"), b"predecessor");
    Ok(())
}

#[test]
fn unsupported_write_semantics_preserve_objects_uploads_and_parts() -> TestResult {
    with_fixture(|fixture| {
        fixture.database.execute(
            "UPDATE principals SET policy_generation=randomblob(16), policy_json=?1",
            [r#"{"Version":"2012-10-17","Statement":{"Effect":"Allow","Action":"s3:*","Resource":"*"}}"#],
        )?;

        for (key, payload) in [("source", "abc"), ("object", "predecessor")] {
            status(
                &fixture.exchange(&request("PUT", &format!("/photos/{key}"), payload, &[]))?,
                200,
            );
        }

        let created = fixture.exchange(&request("POST", "/photos/object?uploads", "", &[]))?;
        status(&created, 200);

        let upload_id = created
            .split_once("<UploadId>")
            .unwrap()
            .1
            .split_once("</UploadId>")
            .unwrap()
            .0;

        let upload = format!("/photos/object?uploadId={upload_id}");
        let part = format!("{upload}&partNumber=1");

        status(&fixture.exchange(&request("PUT", &part, "abc", &[]))?, 200);

        let sequence = fixture.database.query_row(
            "SELECT seq FROM sqlite_sequence WHERE name = 'blobs'",
            [],
            |row| row.get(0),
        )?;

        let before = fixture
            .runtime
            .block_on(fixture.connection.head_object(read("object")))?;

        let selection = "<CompleteMultipartUpload><Part><PartNumber>1</PartNumber><ETag>\"900150983cd24fb0d6963f7d28e17f72\"</ETag></Part></CompleteMultipartUpload>";

        for (operation, method, target, body, copy) in [
            ("put", "PUT", "/photos/object", "replacement", false),
            ("copy", "PUT", "/photos/object", "", true),
            ("initiate", "POST", "/photos/object?uploads", "", false),
            ("part", "PUT", part.as_str(), "replacement", false),
            ("part-copy", "PUT", part.as_str(), "", true),
            ("complete", "POST", upload.as_str(), selection, false),
        ] {
            for (name, value, expected, code) in [
                (
                    "x-amz-server-side-encryption",
                    "AES256",
                    501,
                    "NotImplemented",
                ),
                (
                    "x-amz-server-side-encryption-customer-algorithm",
                    "AES256",
                    501,
                    "NotImplemented",
                ),
                ("x-amz-storage-class", "GLACIER", 501, "NotImplemented"),
                ("x-amz-acl", "private", 400, "AccessControlListNotSupported"),
                (
                    "x-amz-acl",
                    "public-read",
                    400,
                    "AccessControlListNotSupported",
                ),
                (
                    "x-amz-grant-read",
                    "uri=\"http://acs.amazonaws.com/groups/global/AllUsers\"",
                    400,
                    "AccessControlListNotSupported",
                ),
            ] {
                report_case(format_args!(
                    "operation={operation}, option={name}, value={value}"
                ));

                let mut headers = vec![(name, value)];

                if copy {
                    headers.push(("x-amz-copy-source", "/photos/source"));
                }

                let response = fixture.exchange(&request(method, target, body, &headers))?;
                status(&response, expected);
                assert!(response.contains(&format!("<Code>{code}</Code>")));
                unchanged(fixture, sequence)?;

                let current = fixture
                    .runtime
                    .block_on(fixture.connection.head_object(read("object")))?;

                assert_eq!(current.generation, before.generation);
                assert_eq!(current.blob_id, before.blob_id);
            }
        }

        let uploads = fixture.exchange(&request("GET", "/photos?uploads", "", &[]))?;
        status(&uploads, 200);
        assert_eq!(uploads.matches("<Upload>").count(), 1);

        let parts = fixture.exchange(&request("GET", &upload, "", &[]))?;
        status(&parts, 200);
        assert_eq!(parts.matches("<Part>").count(), 1);
        assert!(parts.contains("<Size>3</Size>"));
        assert!(parts.contains("900150983cd24fb0d6963f7d28e17f72"));

        status(
            &fixture.exchange(&request("POST", &upload, selection, &[]))?,
            200,
        );

        assert_eq!(bytes(fixture, "object"), b"abc");
        Ok(())
    })
}

#[test]
fn unsupported_version_selection_and_source_encryption_cannot_return_or_publish_state() -> TestResult
{
    with_fixture(|fixture| {
        fixture.database.execute(
            "UPDATE principals SET policy_generation=randomblob(16), policy_json=?1",
            [r#"{"Version":"2012-10-17","Statement":{"Effect":"Allow","Action":"s3:*","Resource":"*"}}"#],
        )?;

        status(
            &fixture.exchange(&request("PUT", "/photos/object", "predecessor", &[]))?,
            200,
        );

        let sequence = fixture.database.query_row(
            "SELECT seq FROM sqlite_sequence WHERE name = 'blobs'",
            [],
            |row| row.get(0),
        )?;

        for (method, target, body, headers) in [
            ("GET", "/photos/object?versionId=version", "", vec![]),
            ("HEAD", "/photos/object?versionId=version", "", vec![]),
            ("DELETE", "/photos/object?versionId=version", "", vec![]),
            ("GET", "/photos/object?acl&versionId=version", "", vec![]),
            (
                "GET",
                "/photos/object?tagging&versionId=version",
                "",
                vec![],
            ),
            (
                "PUT",
                "/photos/object?tagging&versionId=version",
                "<Tagging><TagSet/></Tagging>",
                vec![],
            ),
            (
                "DELETE",
                "/photos/object?tagging&versionId=version",
                "",
                vec![],
            ),
            (
                "GET",
                "/photos/object?attributes&versionId=version",
                "",
                vec![("x-amz-object-attributes", "ETag")],
            ),
            (
                "PUT",
                "/photos/object",
                "",
                vec![("x-amz-copy-source", "/photos/object?versionId=version")],
            ),
            (
                "PUT",
                "/photos/object",
                "",
                vec![
                    ("x-amz-copy-source", "/photos/object"),
                    (
                        "x-amz-copy-source-server-side-encryption-customer-algorithm",
                        "AES256",
                    ),
                ],
            ),
        ] {
            report_case(format_args!(
                "method={method}, target={target}, headers={headers:?}"
            ));

            let response = fixture.exchange(&request(method, target, body, &headers))?;
            status(&response, 501);

            if method == "HEAD" {
                assert_eq!(response.split_once("\r\n\r\n").unwrap().1, "");
            } else {
                assert!(response.contains("<Code>NotImplemented</Code>"));
            }

            unchanged(fixture, sequence)?;
        }

        Ok(())
    })
}

#[test]
fn empty_unsupported_headers_cannot_disappear_before_storage_admission() -> TestResult {
    with_fixture(|fixture| {
        status(
            &fixture.exchange(&request("PUT", "/photos/object", "predecessor", &[]))?,
            200,
        );

        let sequence = fixture.database.query_row(
            "SELECT seq FROM sqlite_sequence WHERE name = 'blobs'",
            [],
            |row| row.get(0),
        )?;

        let cases: &[(&[&str], &[&str], u16)] = &[
            (
                &["PUT"],
                &[
                    "x-amz-server-side-encryption-bucket-key-enabled",
                    "x-amz-object-lock-legal-hold",
                    "x-amz-object-lock-mode",
                    "x-amz-object-lock-retain-until-date",
                    "x-amz-request-payer",
                    "x-amz-server-side-encryption-customer-algorithm",
                    "x-amz-server-side-encryption-customer-key",
                    "x-amz-server-side-encryption-customer-key-md5",
                    "x-amz-server-side-encryption-context",
                    "x-amz-server-side-encryption-aws-kms-key-id",
                    "x-amz-server-side-encryption",
                    "x-amz-website-redirect-location",
                    "x-amz-write-offset-bytes",
                ],
                501,
            ),
            (
                &["PUT"],
                &[
                    "x-amz-grant-full-control",
                    "x-amz-grant-read",
                    "x-amz-grant-read-acp",
                    "x-amz-grant-write-acp",
                ],
                400,
            ),
            (
                &["GET", "HEAD"],
                &[
                    "x-amz-request-payer",
                    "x-amz-server-side-encryption-customer-algorithm",
                    "x-amz-server-side-encryption-customer-key",
                    "x-amz-server-side-encryption-customer-key-md5",
                ],
                501,
            ),
            (
                &["DELETE"],
                &[
                    "x-amz-bypass-governance-retention",
                    "x-amz-if-match-last-modified-time",
                    "x-amz-if-match-size",
                    "x-amz-mfa",
                    "x-amz-request-payer",
                ],
                501,
            ),
        ];

        for &(methods, headers, expected) in cases {
            for &method in methods {
                for &header in headers {
                    report_case(format_args!(
                        "method={method:?}, header={header:?}, expected={expected}"
                    ));

                    let response = fixture.exchange(&request(
                        method,
                        "/photos/object",
                        if method == "PUT" { "replacement" } else { "" },
                        &[(header, "")],
                    ))?;

                    assert!(
                        response.starts_with(&format!("HTTP/1.1 {expected} ")),
                        "{method} with empty {header}: expected {expected}, got {}",
                        response.lines().next().unwrap_or("empty")
                    );

                    unchanged(fixture, sequence)?;
                }
            }
        }

        Ok(())
    })
}

#[test]
fn empty_supported_control_headers_fail_validation_instead_of_defaulting() -> TestResult {
    with_fixture(|fixture| {
        status(
            &fixture.exchange(&request(
                "PUT",
                "/photos/object",
                "predecessor",
                &[
                    ("x-amz-acl", "bucket-owner-full-control"),
                    ("x-amz-storage-class", "STANDARD"),
                ],
            ))?,
            200,
        );

        let sequence = fixture.database.query_row(
            "SELECT seq FROM sqlite_sequence WHERE name = 'blobs'",
            [],
            |row| row.get(0),
        )?;

        for (method, header) in [
            ("PUT", "x-amz-acl"),
            ("PUT", "x-amz-storage-class"),
            ("PUT", "expires"),
            ("GET", "x-amz-checksum-mode"),
            ("HEAD", "x-amz-checksum-mode"),
            ("GET", "range"),
            ("HEAD", "range"),
            ("GET", "if-modified-since"),
            ("HEAD", "if-modified-since"),
            ("GET", "if-unmodified-since"),
            ("HEAD", "if-unmodified-since"),
        ] {
            report_case(format_args!("method={method:?}, header={header:?}"));

            let response = fixture.exchange(&request(
                method,
                "/photos/object",
                if method == "PUT" { "replacement" } else { "" },
                &[(header, "")],
            ))?;

            assert!(
                response.starts_with("HTTP/1.1 400 "),
                "{method} with empty {header}: {}",
                response.lines().next().unwrap_or("empty")
            );

            unchanged(fixture, sequence)?;
        }

        Ok(())
    })
}

#[test]
fn persistent_representation_preserves_explicit_empty_headers_and_absence() -> TestResult {
    with_fixture(|fixture| {
        let headers = [
            ("content-type", ""),
            ("content-encoding", ""),
            ("content-disposition", ""),
            ("content-language", ""),
            ("cache-control", ""),
        ];

        for (key, supplied) in [("empty", headers.as_slice()), ("absent", &[][..])] {
            report_case(format_args!("key={key:?}, supplied={supplied:?}"));

            let path = format!("/photos/{key}");

            status(
                &fixture.exchange(&request("PUT", &path, "value", supplied))?,
                200,
            );

            let selected = fixture
                .runtime
                .block_on(fixture.connection.head_object(read(key)))?;

            let fields = selected.representation.fields();

            for (name, field) in [
                ("content-type", &fields.content_type),
                ("content-encoding", &fields.content_encoding),
                ("content-disposition", &fields.content_disposition),
                ("content-language", &fields.content_language),
                ("cache-control", &fields.cache_control),
            ] {
                assert_eq!(
                    field.as_deref(),
                    (key == "empty").then_some(""),
                    "{key} {name}"
                );
            }

            for method in ["GET", "HEAD"] {
                report_case(format_args!(
                    "key={key:?}, supplied={supplied:?}, method={method:?}"
                ));

                let response = fixture.exchange(&request(method, &path, "", &[]))?;
                status(&response, 200);

                for (header, _) in headers {
                    let actual =
                        response
                            .split_once("\r\n\r\n")
                            .unwrap()
                            .0
                            .lines()
                            .find_map(|line| {
                                let (name, value) = line.split_once(':')?;
                                (name == header).then(|| value.trim())
                            });

                    let expected = if key == "empty" {
                        Some("")
                    } else if header == "content-type" {
                        Some("binary/octet-stream")
                    } else {
                        None
                    };

                    assert_eq!(actual, expected, "{method} {key} {header}");
                }
            }
        }

        Ok(())
    })
}

#[test]
fn raw_integrity_assertions_cannot_be_dropped_or_defaulted() -> TestResult {
    with_fixture(|fixture| {
        status(
            &fixture.exchange(&request("PUT", "/photos/object", "predecessor", &[]))?,
            200,
        );

        let sequence = fixture.database.query_row(
            "SELECT seq FROM sqlite_sequence WHERE name = 'blobs'",
            [],
            |row| row.get(0),
        )?;

        for (header, expected, code) in [
            ("content-md5", 400, "InvalidDigest"),
            ("x-amz-checksum-crc32", 400, "InvalidDigest"),
            ("x-amz-checksum-crc32c", 400, "InvalidDigest"),
            ("x-amz-checksum-crc64nvme", 400, "InvalidDigest"),
            ("x-amz-checksum-sha1", 400, "InvalidDigest"),
            ("x-amz-checksum-sha256", 400, "InvalidDigest"),
            ("x-amz-checksum-md5", 501, "NotImplemented"),
            ("x-amz-checksum-sha512", 501, "NotImplemented"),
            ("x-amz-checksum-xxhash128", 501, "NotImplemented"),
            ("x-amz-checksum-xxhash3", 501, "NotImplemented"),
            ("x-amz-checksum-xxhash64", 501, "NotImplemented"),
            ("x-amz-checksum-algorithm", 501, "NotImplemented"),
            ("x-amz-sdk-checksum-algorithm", 501, "NotImplemented"),
        ] {
            report_case(format_args!(
                "header={header:?}, expected={expected:?}, code={code:?}"
            ));

            let response = fixture.exchange(&request(
                "PUT",
                "/photos/object",
                "replacement",
                &[(header, "")],
            ))?;

            status(&response, expected);
            assert!(response.contains(&format!("<Code>{code}</Code>")));
            unchanged(fixture, sequence)?;
        }

        for (headers, code) in [
            (
                vec![("x-amz-sdk-checksum-algorithm", "SHA256")],
                "InvalidRequest",
            ),
            (
                vec![("x-amz-sdk-checksum-algorithm", "SHA512")],
                "NotImplemented",
            ),
            (
                vec![
                    ("x-amz-sdk-checksum-algorithm", "SHA256"),
                    ("x-amz-checksum-crc32", "y/Q5Jg=="),
                ],
                "BadDigest",
            ),
            (
                vec![
                    ("x-amz-sdk-checksum-algorithm", "SHA256"),
                    ("x-amz-checksum-algorithm", "CRC32"),
                    ("x-amz-checksum-crc32", "y/Q5Jg=="),
                ],
                "BadDigest",
            ),
        ] {
            report_case(format_args!("headers={headers:?}, code={code:?}"));

            let response =
                fixture.exchange(&request("PUT", "/photos/object", "123456789", &headers))?;

            status(&response, if code == "NotImplemented" { 501 } else { 400 });
            assert!(response.contains(&format!("<Code>{code}</Code>")));
            unchanged(fixture, sequence)?;
        }

        // The independent signer takes a map, so duplicate array entries would
        // collapse before reaching HTTP. Add the second field to actual wire bytes.
        let raw = String::from_utf8(request(
            "PUT",
            "/photos/object",
            "123456789",
            &[
                ("x-amz-sdk-checksum-algorithm", "CRC32"),
                ("x-amz-checksum-crc32", "y/Q5Jg=="),
            ],
        ))?;

        let repeated = raw.replacen(
            "x-amz-sdk-checksum-algorithm: CRC32\r\n",
            "x-amz-sdk-checksum-algorithm: CRC32\r\nx-amz-sdk-checksum-algorithm: SHA256\r\n",
            1,
        );

        assert_ne!(repeated, raw);
        let duplicate = fixture.exchange(repeated.as_bytes())?;
        status(&duplicate, 400);
        // Singleton ambiguity is rejected by the transport guard before S3 parsing.
        assert_eq!(duplicate.split_once("\r\n\r\n").unwrap().1, "");
        assert!(duplicate.to_ascii_lowercase().contains("connection: close"));
        unchanged(fixture, sequence)?;

        status(
            &fixture.exchange(&request(
                "PUT",
                "/photos/object",
                "123456789",
                &[
                    ("x-amz-sdk-checksum-algorithm", "CRC32"),
                    ("x-amz-checksum-algorithm", "CRC32"),
                    ("x-amz-checksum-crc32", "y/Q5Jg=="),
                ],
            ))?,
            200,
        );

        assert_eq!(bytes(fixture, "object"), b"123456789");
        Ok(())
    })
}

#[test]
fn unsupported_checksum_assertions_cannot_publish_or_return_stored_state() -> TestResult {
    with_fixture(|fixture| {
        status(
            &fixture.exchange(&request("PUT", "/photos/object", "predecessor", &[]))?,
            200,
        );

        let sequence = fixture.database.query_row(
            "SELECT seq FROM sqlite_sequence WHERE name = 'blobs'",
            [],
            |row| row.get(0),
        )?;

        for (header, value) in [
            ("x-amz-checksum-type", "COMPOSITE"),
            ("x-amz-checksum-type", ""),
            ("x-amz-checksum-crc16", "AAAA"),
            ("x-amz-checksum-crc16", ""),
        ] {
            for method in ["PUT", "GET", "HEAD"] {
                for hoisted in [false, true] {
                    report_case(format_args!(
                        "header={header:?}, value={value:?}, method={method:?}, hoisted={hoisted:?}"
                    ));

                    let target = if hoisted {
                        format!("/photos/object?{header}={value}")
                    } else {
                        "/photos/object".to_owned()
                    };

                    let headers = [(header, value)];

                    let response = fixture.exchange(&request_with_mode(
                        method,
                        &target,
                        if method == "PUT" { "replacement" } else { "" },
                        if hoisted { &[] } else { &headers },
                        if hoisted { "query" } else { "header" },
                    ))?;

                    assert!(
                        response.starts_with("HTTP/1.1 501 "),
                        "{method} {header}={value:?}, hoisted={hoisted}: {response}"
                    );

                    if method == "HEAD" {
                        assert_eq!(response.split_once("\r\n\r\n").unwrap().1, "");
                    } else {
                        assert!(response.contains("<Code>NotImplemented</Code>"));
                    }

                    unchanged(fixture, sequence)?;
                }
            }
        }

        for hoisted in [false, true] {
            report_case(format_args!("hoisted={hoisted:?}"));

            let target = if hoisted {
                "/photos/object?x-amz-checksum-mode=ENABLED"
            } else {
                "/photos/object"
            };

            let response = fixture.exchange(&request_with_mode(
                "PUT",
                target,
                "replacement",
                if hoisted {
                    &[]
                } else {
                    &[("x-amz-checksum-mode", "ENABLED")]
                },
                if hoisted { "query" } else { "header" },
            ))?;

            status(&response, 501);
            assert!(response.contains("<Code>NotImplemented</Code>"));
            unchanged(fixture, sequence)?;
        }

        Ok(())
    })
}

#[test]
fn decoded_content_length_requires_complete_unsigned_decimal_without_publication() -> TestResult {
    with_fixture(|fixture| {
        let raw_request = |length, streamed| {
            let mut headers = vec![("x-amz-decoded-content-length", length)];

            let body = if streamed {
                headers.extend([
                    ("content-encoding", "aws-chunked"),
                    ("x-amz-content-sha256", "STREAMING-UNSIGNED-PAYLOAD-TRAILER"),
                    ("x-amz-trailer", "x-amz-checksum-crc32"),
                ]);

                // Independent CRC32 vector for 123456789; the valid control must
                // publish these decoded bytes before malformed variants are tested.
                "9\r\n123456789\r\n0\r\nx-amz-checksum-crc32:y/Q5Jg==\r\n\r\n"
            } else {
                "replacement"
            };

            request("PUT", "/photos/object", body, &headers)
        };

        for (length, streamed, expected) in
            [("0000", false, "replacement"), ("0009", true, "123456789")]
        {
            report_case(format_args!(
                "length={length:?}, streamed={streamed:?}, expected={expected:?}"
            ));

            status(&fixture.exchange(&raw_request(length, streamed))?, 200);
            assert_eq!(bytes(fixture, "object"), expected.as_bytes());
        }

        status(
            &fixture.exchange(&request("PUT", "/photos/object", "predecessor", &[]))?,
            200,
        );

        let sequence = fixture.database.query_row(
            "SELECT seq FROM sqlite_sequence WHERE name = 'blobs'",
            [],
            |row| row.get(0),
        )?;

        for streamed in [false, true] {
            for length in [
                "9tail",
                "9.0",
                "9e0",
                "+9",
                "-9",
                "",
                "18446744073709551616",
            ] {
                report_case(format_args!("streamed={streamed:?}, length={length:?}"));

                let rejected = fixture.exchange(&raw_request(length, streamed))?;
                status(&rejected, 400);

                assert!(
                    rejected.contains("<Code>InvalidRequest</Code>"),
                    "{length:?}, streamed {streamed}"
                );

                unchanged(fixture, sequence)?;
            }
        }

        Ok(())
    })
}

#[test]
fn malformed_decoded_length_bytes_cannot_be_ignored_as_absent() -> TestResult {
    with_fixture(|fixture| {
        fixture.database.execute(
            "UPDATE buckets SET policy_generation=randomblob(16),policy_json=?1,
             block_public_acls=0,ignore_public_acls=0,block_public_policy=0,restrict_public_buckets=0",
            [r#"{"Version":"2012-10-17","Statement":{"Effect":"Allow","Principal":"*","Action":"s3:GetObject","Resource":"arn:aws:s3:::photos/*"}}"#],
        )?;

        let raw_request = |length: &[u8]| {
            let mut raw = b"GET /photos/object HTTP/1.1\r\nhost: localhost:9000\r\ncontent-length: 0\r\nconnection: close\r\nx-amz-decoded-content-length: ".to_vec();
            raw.extend_from_slice(length);
            raw.extend_from_slice(b"\r\n\r\n");
            raw
        };

        status(
            &fixture.exchange(&request("PUT", "/photos/object", "predecessor", &[]))?,
            200,
        );

        let allowed = fixture.exchange(&raw_request(b"0"))?;
        status(&allowed, 200);
        assert_eq!(allowed.split_once("\r\n\r\n").unwrap().1, "predecessor");

        let sequence = fixture.database.query_row(
            "SELECT seq FROM sqlite_sequence WHERE name = 'blobs'",
            [],
            |row| row.get(0),
        )?;

        for length in [b"\xff".as_slice(), b"9\xff"] {
            report_case(format_args!("length={length:?}"));

            let rejected = fixture.exchange(&raw_request(length))?;
            status(&rejected, 400);
            assert!(rejected.contains("<Code>InvalidRequest</Code>"));
            unchanged(fixture, sequence)?;
        }

        Ok(())
    })
}
