use std::fmt::Write as _;

use super::*;

fn element<'a>(response: &'a str, name: &str) -> &'a str {
    response
        .split_once(&format!("<{name}>"))
        .unwrap()
        .1
        .split_once(&format!("</{name}>"))
        .unwrap()
        .0
}

#[test]
fn completion_envelope_accepts_the_full_part_limit_and_rejects_overflow() -> TestResult {
    with_fixture(|fixture| {
        status(
            &fixture.exchange(&request("PUT", "/photos/object", "before", &[]))?,
            200,
        );

        let created = fixture.exchange(&request("POST", "/photos/object?uploads", "", &[]))?;
        status(&created, 200);
        let path = format!("/photos/object?uploadId={}", element(&created, "UploadId"));

        for (count, expected) in [(10_000, "InvalidPart"), (10_001, "InvalidRequest")] {
            report_case(format_args!("count={count:?}, expected={expected:?}"));

            let mut parts = String::new();

            for number in 1..=count {
                write!(
                    parts,
                    "<Part><PartNumber>{number}</PartNumber><ETag>\"900150983cd24fb0d6963f7d28e17f72\"</ETag></Part>"
                )?;
            }

            let body = format!("<CompleteMultipartUpload>{parts}</CompleteMultipartUpload>");
            assert!(body.len() > 64 * 1024);
            let response = fixture.exchange(&request("POST", &path, &body, &[]))?;
            status(&response, 400);

            assert!(
                response.contains(&format!("<Code>{expected}</Code>")),
                "{count}: {response}"
            );

            assert_eq!(bytes(fixture, "object"), b"before");
        }

        let listed = fixture.exchange(&request("GET", &path, "", &[]))?;
        status(&listed, 200);
        assert!(!listed.contains("<Part>"));
        Ok(())
    })
}

#[test]
fn unknown_checksum_assertions_preserve_multipart_parts_and_predecessor() -> TestResult {
    with_fixture(|fixture| {
        fixture.database.execute(
            "UPDATE principals SET policy_generation=randomblob(16), policy_json=?1",
            [r#"{"Version":"2012-10-17","Statement":{"Effect":"Allow","Action":"s3:*","Resource":"*"}}"#],
        )?;

        status(
            &fixture.exchange(&request("PUT", "/photos/object", "predecessor", &[]))?,
            200,
        );

        let created = fixture.exchange(&request("POST", "/photos/object?uploads", "", &[]))?;
        status(&created, 200);

        let path = format!("/photos/object?uploadId={}", element(&created, "UploadId"));
        let part_path = format!("{path}&partNumber=1");

        status(
            &fixture.exchange(&request("PUT", &part_path, "abc", &[]))?,
            200,
        );

        let sequence: i64 = fixture.database.query_row(
            "SELECT seq FROM sqlite_sequence WHERE name = 'blobs'",
            [],
            |row| row.get(0),
        )?;

        let selection = "<CompleteMultipartUpload><Part><PartNumber>1</PartNumber><ETag>\"900150983cd24fb0d6963f7d28e17f72\"</ETag></Part></CompleteMultipartUpload>";

        for (method, target, body) in [
            ("POST", "/photos/object?uploads", ""),
            ("PUT", part_path.as_str(), "replacement"),
            ("GET", path.as_str(), ""),
            ("GET", "/photos?uploads", ""),
            ("POST", path.as_str(), selection),
            ("DELETE", path.as_str(), ""),
        ] {
            for hoisted in [false, true] {
                report_case(format_args!(
                    "method={method:?}, target={target:?}, body={body:?}, hoisted={hoisted:?}"
                ));

                let target = if hoisted {
                    format!("{target}&x-amz-checksum-crc16=AAAA")
                } else {
                    target.to_owned()
                };

                let response = fixture.exchange(&request_with_mode(
                    method,
                    &target,
                    body,
                    if hoisted {
                        &[]
                    } else {
                        &[("x-amz-checksum-crc16", "AAAA")]
                    },
                    if hoisted { "query" } else { "header" },
                ))?;

                assert!(
                    response.starts_with("HTTP/1.1 501 "),
                    "{method} {target}, hoisted={hoisted}: {response}"
                );

                assert!(response.contains("<Code>NotImplemented</Code>"));
                assert_eq!(bytes(fixture, "object"), b"predecessor");
            }
        }

        let current: i64 = fixture.database.query_row(
            "SELECT seq FROM sqlite_sequence WHERE name = 'blobs'",
            [],
            |row| row.get(0),
        )?;

        assert_eq!(
            current, sequence,
            "rejected assertions cannot reserve payloads"
        );

        let uploads = fixture.exchange(&request("GET", "/photos?uploads", "", &[]))?;
        status(&uploads, 200);
        assert_eq!(uploads.matches("<Upload>").count(), 1);

        let parts = fixture.exchange(&request("GET", &path, "", &[]))?;
        status(&parts, 200);
        assert_eq!(parts.matches("<Part>").count(), 1);
        assert_eq!(element(&parts, "Size"), "3");

        status(
            &fixture.exchange(&request("POST", &path, selection, &[]))?,
            200,
        );

        assert_eq!(bytes(fixture, "object"), b"abc");
        Ok(())
    })
}

#[test]
fn presigned_creation_preserves_metadata_and_rejects_conflicting_carriers() -> TestResult {
    with_fixture(|fixture| {
        status(
            &fixture.exchange(&request("PUT", "/photos/source", "abc", &[]))?,
            200,
        );

        let query =
            "x-amz-meta-origin=query%2Bvalue&x-amz-meta-title=%3D%3FUTF-8%3FB%3FY2Fmw6k%3D%3F%3D";

        let copy_path = format!(
            "/photos/copied?x-amz-copy-source=%2Fphotos%2Fsource&x-amz-metadata-directive=REPLACE&{query}"
        );

        let copied = fixture.exchange(&request_with_mode("PUT", &copy_path, "", &[], "query"))?;
        status(&copied, 200);

        let copied = fixture
            .runtime
            .block_on(fixture.connection.head_object(read("copied")))?;

        assert_eq!(
            copied.metadata.entries().get("origin").map(String::as_str),
            Some("query+value")
        );

        assert_eq!(
            copied.metadata.entries().get("title").map(String::as_str),
            Some("café")
        );

        let created = fixture.exchange(&request_with_mode(
            "POST",
            &format!("/photos/resumed?uploads&{query}"),
            "",
            &[],
            "query",
        ))?;

        status(&created, 200);

        let path = format!("/photos/resumed?uploadId={}", element(&created, "UploadId"));

        status(
            &fixture.exchange(&request("PUT", &format!("{path}&partNumber=1"), "abc", &[]))?,
            200,
        );

        let selection = "<CompleteMultipartUpload><Part><PartNumber>1</PartNumber><ETag>\"900150983cd24fb0d6963f7d28e17f72\"</ETag></Part></CompleteMultipartUpload>";

        status(
            &fixture.exchange(&request("POST", &path, selection, &[]))?,
            200,
        );

        let completed = fixture
            .runtime
            .block_on(fixture.connection.head_object(read("resumed")))?;

        assert_eq!(completed.metadata.entries(), copied.metadata.entries());

        for path in [
            format!("{copy_path}&X-Amz-Meta-Origin=conflicting"),
            format!("{copy_path}&x-amz-meta-=empty-key"),
        ] {
            report_case(format_args!("path={path:?}"));

            let response = fixture.exchange(&request_with_mode("PUT", &path, "", &[], "query"))?;
            status(&response, 400);
        }

        let response = fixture.exchange(&request(
            "PUT",
            &copy_path,
            "",
            &[("x-amz-meta-origin", "header")],
        ))?;

        status(&response, 400);
        assert_eq!(bytes(fixture, "copied"), b"abc");
        Ok(())
    })
}

#[test]
fn presigned_creation_rejects_unsupported_semantics_before_publication() -> TestResult {
    with_fixture(|fixture| {
        status(
            &fixture.exchange(&request("PUT", "/photos/source", "abc", &[]))?,
            200,
        );

        for (name, value) in [
            ("x-amz-server-side-encryption", "AES256"),
            ("x-amz-storage-class", "GLACIER"),
            ("x-amz-object-lock-mode", "COMPLIANCE"),
            ("x-amz-website-redirect-location", "%2Fredirect"),
        ] {
            for (method, path) in [
                (
                    "PUT",
                    format!("/photos/copied?x-amz-copy-source=%2Fphotos%2Fsource&{name}={value}"),
                ),
                ("POST", format!("/photos/resumed?uploads&{name}={value}")),
            ] {
                report_case(format_args!(
                    "name={name:?}, value={value:?}, method={method:?}, path={path:?}"
                ));

                let response =
                    fixture.exchange(&request_with_mode(method, &path, "", &[], "query"))?;

                status(&response, 501);

                assert!(
                    response.contains("<Code>NotImplemented</Code>"),
                    "{response}"
                );
            }
        }

        status(
            &fixture.exchange(&request("HEAD", "/photos/copied", "", &[]))?,
            404,
        );

        Ok(())
    })
}

#[test]
fn authenticated_multipart_workflow_uses_real_parts_and_completion() -> TestResult {
    with_fixture(|fixture| {
        fixture.database.execute("UPDATE principals SET policy_generation=randomblob(16), policy_json=?1", [
            r#"{"Version":"2012-10-17","Statement":{"Effect":"Allow","Action":"s3:*","Resource":"*"}}"#,
        ])?;

        status(
            &fixture.exchange(&request("PUT", "/photos/source", "abc", &[]))?,
            200,
        );

        let created = fixture.exchange(&request(
            "POST",
            "/photos/object?uploads",
            "",
            &[("x-amz-meta-origin", "initiation")],
        ))?;

        status(&created, 200);
        let upload_id = element(&created, "UploadId");
        let path = format!("/photos/object?uploadId={upload_id}");
        let part_path = format!("{path}&partNumber=7");
        let uploads = fixture.exchange(&request("GET", "/photos?uploads", "", &[]))?;
        status(&uploads, 200);
        assert!(uploads.contains(upload_id));

        status(
            &fixture.exchange(&request("PUT", &part_path, "before", &[]))?,
            200,
        );

        let bad = fixture.exchange(&request(
            "PUT",
            &part_path,
            "bad",
            &[("content-md5", "AAAAAAAAAAAAAAAAAAAAAA==")],
        ))?;

        status(&bad, 400);
        assert!(bad.contains("<Code>BadDigest</Code>"));

        let copied = fixture.exchange(&request(
            "PUT",
            &part_path,
            "",
            &[("x-amz-copy-source", "/photos/source")],
        ))?;

        status(&copied, 200);
        assert!(copied.contains("<CopyPartResult>"));
        assert!(copied.contains("<ChecksumCRC64NVME>BeXKuz/B+us=</ChecksumCRC64NVME>"));

        let listed = fixture.exchange(&request("GET", &path, "", &[]))?;
        status(&listed, 200);
        assert!(listed.contains("<PartNumber>7</PartNumber>"));
        assert!(listed.contains("<Size>3</Size>"));

        let selection = "<CompleteMultipartUpload><Part><PartNumber>7</PartNumber><ETag>\"900150983cd24fb0d6963f7d28e17f72\"</ETag></Part></CompleteMultipartUpload>";

        let mismatch = fixture.exchange(&request(
            "POST",
            &path,
            selection,
            &[("x-amz-checksum-type", "COMPOSITE")],
        ))?;

        status(&mismatch, 400);

        let completed = fixture.exchange(&request(
            "POST",
            &path,
            selection,
            &[
                ("x-amz-checksum-crc64nvme", "BeXKuz/B+us="),
                ("x-amz-checksum-type", "FULL_OBJECT"),
                ("x-amz-mp-object-size", "3"),
            ],
        ))?;

        status(&completed, 200);

        assert!(
            completed.contains("<CompleteMultipartUploadResult "),
            "{completed}"
        );

        assert_eq!(bytes(fixture, "object"), b"abc");

        assert_eq!(
            fixture
                .runtime
                .block_on(fixture.connection.head_object(read("object")))?
                .metadata
                .entries()["origin"],
            "initiation"
        );

        let retry = fixture.exchange(&request("POST", &path, selection, &[]))?;
        status(&retry, 404);
        assert!(retry.contains("<Code>NoSuchUpload</Code>"));

        let created = fixture.exchange(&request("POST", "/photos/abandoned?uploads", "", &[]))?;
        status(&created, 200);

        let path = format!(
            "/photos/abandoned?uploadId={}",
            element(&created, "UploadId")
        );

        status(
            &fixture.exchange(&request("DELETE", &path, "unexpected", &[]))?,
            400,
        );

        status(&fixture.exchange(&request("GET", &path, "", &[]))?, 200);
        status(&fixture.exchange(&request("DELETE", &path, "", &[]))?, 204);
        status(&fixture.exchange(&request("GET", &path, "", &[]))?, 404);
        Ok(())
    })
}

#[test]
fn presigned_copy_preserves_hoisted_source_and_assertions_and_rejects_ambiguity() -> TestResult {
    with_fixture(|fixture| {
        status(
            &fixture.exchange(&request("PUT", "/photos/source%2B%252f", "abc", &[]))?,
            200,
        );

        let path = "/photos/copied?x-amz-copy-source=%2Fphotos%2Fsource%252B%25252f&x-amz-checksum-algorithm=SHA256";
        let response = fixture.exchange(&request_with_mode("PUT", path, "", &[], "query"))?;

        assert!(
            response.starts_with("HTTP/1.1 200 "),
            "presigned copy: {response}"
        );

        assert_eq!(bytes(fixture, "copied"), b"abc");
        assert!(response.contains("<ChecksumSHA256>"));

        let denied = format!("{path}&x-amz-copy-source-if-match=%22different%22");
        let denied = fixture.exchange(&request_with_mode("PUT", &denied, "", &[], "query"))?;

        assert!(
            denied.starts_with("HTTP/1.1 412 "),
            "source assertion: {denied}"
        );

        let duplicate = format!("{path}&X-Amz-Copy-Source=%2Fphotos%2Fother");

        let duplicate =
            fixture.exchange(&request_with_mode("PUT", &duplicate, "", &[], "query"))?;

        assert!(
            duplicate.starts_with("HTTP/1.1 400 "),
            "duplicate: {duplicate}"
        );

        let conflict = fixture.exchange(&request_with_mode(
            "PUT",
            path,
            "",
            &[("x-amz-copy-source", "/photos/other")],
            "query",
        ))?;

        assert!(
            conflict.starts_with("HTTP/1.1 403 "),
            "unsigned header/query: {conflict}"
        );

        assert!(conflict.contains("<Code>AccessDenied</Code>"));

        let conflict = fixture.exchange(&request(
            "PUT",
            path,
            "",
            &[("x-amz-copy-source", "/photos/other")],
        ))?;

        assert!(
            conflict.starts_with("HTTP/1.1 400 "),
            "signed header/query: {conflict}"
        );

        assert_eq!(bytes(fixture, "copied"), b"abc");
        Ok(())
    })
}

#[test]
fn omitted_negotiation_verifies_sdk_part_integrity_without_renegotiation() -> TestResult {
    with_fixture(|fixture| {
        fixture.database.execute("UPDATE principals SET policy_generation=randomblob(16), policy_json=?1", [
            r#"{"Version":"2012-10-17","Statement":{"Effect":"Allow","Action":"s3:*","Resource":"*"}}"#,
        ])?;

        let created = fixture.exchange(&request("POST", "/photos/default?uploads", "", &[]))?;
        status(&created, 200);
        let path = format!("/photos/default?uploadId={}", element(&created, "UploadId"));
        let part = format!("{path}&partNumber=1");

        let uploaded = fixture.exchange(&request(
            "PUT",
            &part,
            "abc",
            &[
                ("x-amz-sdk-checksum-algorithm", "CRC32"),
                ("x-amz-checksum-crc32", "NSRBwg=="),
            ],
        ))?;

        status(&uploaded, 200);
        assert!(uploaded.contains("x-amz-checksum-crc64nvme: BeXKuz/B+us="));

        let bad = fixture.exchange(&request(
            "PUT",
            &part,
            "bad",
            &[
                ("x-amz-sdk-checksum-algorithm", "CRC32"),
                ("x-amz-checksum-crc32", "NSRBwg=="),
            ],
        ))?;

        status(&bad, 400);
        assert!(bad.contains("<Code>BadDigest</Code>"));

        let selection = "<CompleteMultipartUpload><Part><PartNumber>1</PartNumber><ETag>\"900150983cd24fb0d6963f7d28e17f72\"</ETag></Part></CompleteMultipartUpload>";
        let completed = fixture.exchange(&request("POST", &path, selection, &[]))?;
        status(&completed, 200);
        assert!(completed.contains("<Location>http://"), "{completed}");
        assert!(completed.contains("<ChecksumCRC64NVME>BeXKuz/B+us=</ChecksumCRC64NVME>"));
        assert_eq!(bytes(fixture, "default"), b"abc");

        let created = fixture.exchange(&request(
            "POST",
            "/photos/explicit?uploads",
            "",
            &[("x-amz-checksum-algorithm", "CRC64NVME")],
        ))?;

        status(&created, 200);

        let part = format!(
            "/photos/explicit?uploadId={}&partNumber=1",
            element(&created, "UploadId")
        );

        let rejected = fixture.exchange(&request(
            "PUT",
            &part,
            "abc",
            &[("x-amz-checksum-crc32", "NSRBwg==")],
        ))?;

        status(&rejected, 400);
        Ok(())
    })
}
