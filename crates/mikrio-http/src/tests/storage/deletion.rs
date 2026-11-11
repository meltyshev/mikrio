use std::io::Write;
use std::process::Stdio;

use super::*;

fn batch_request(body: &str, digest: &str, headers: &[(&str, &str)]) -> Vec<u8> {
    let producer = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../tests/contract");
    let mut command = crate::tests::python::command();

    command
        .arg("-c")
        .arg(
            r#"
import base64
import hashlib
import sys
import zlib

sys.path.insert(0, sys.argv[1])
from http_sigv4 import request

body = sys.stdin.buffer.read()
headers = dict(zip(sys.argv[3::2], sys.argv[4::2]))

digests = {
    "md5": ("content-md5", base64.b64encode(hashlib.md5(body).digest()).decode()),
    "crc32": (
        "x-amz-checksum-crc32",
        base64.b64encode(zlib.crc32(body).to_bytes(4, "big")).decode(),
    ),
}

digest = digests.get(sys.argv[2])

if digest is not None:
    headers[digest[0]] = digest[1]

signed = request(
    "POST",
    "/photos",
    "header",
    "mikrio_a00112233445566778899AABBCCDDEEFF",
    "synthetic-http-probe-secret",
    "us-east-1",
    "s3",
    body,
    headers,
    {"delete": ""},
)

sys.stdout.buffer.write(signed)
"#,
        )
        .arg(producer)
        .arg(digest)
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
    assert!(output.status.success(), "independent batch producer failed");
    output.stdout
}

#[test]
fn single_delete_uses_current_condition_policy_and_preserves_denied_objects() -> TestResult {
    with_fixture(|fixture| {
        status(
            &fixture.exchange(&request("PUT", "/photos/conditional", "123456789", &[]))?,
            200,
        );

        fixture.database.execute(
            "UPDATE principals SET policy_generation=randomblob(16), policy_json=?1",
            [r#"{"Version":"2012-10-17","Statement":[{"Effect":"Allow","Action":"s3:DeleteObject","Resource":"*","Condition":{"Null":{"s3:if-match":"false"}}},{"Effect":"Allow","Action":"s3:GetObject","Resource":"*"}]}"#],
        )?;

        for (headers, expected) in [(vec![], 403), (vec![("if-match", "\"different\"")], 412)] {
            let response =
                fixture.exchange(&request("DELETE", "/photos/conditional", "", &headers))?;

            status(&response, expected);
            assert_eq!(bytes(fixture, "conditional"), b"123456789");
        }

        fixture.database.execute(
            "UPDATE principals SET policy_generation=randomblob(16), policy_json=?1",
            [r#"{"Version":"2012-10-17","Statement":{"Effect":"Allow","Action":"s3:DeleteObject","Resource":"*","Condition":{"Null":{"s3:if-match":"false"}}}}"#],
        )?;

        for etag in ["\"25f9e794323b453885f5181f1b624d0b\"", "\"different\""] {
            let response = fixture.exchange(&request(
                "DELETE",
                "/photos/conditional",
                "",
                &[("if-match", etag)],
            ))?;

            status(&response, 403);
            assert!(response.contains("<Code>AccessDenied</Code>"));
            assert_eq!(bytes(fixture, "conditional"), b"123456789");
        }

        status(
            &fixture.exchange(&request(
                "DELETE",
                "/photos/conditional",
                "",
                &[("if-match", "*")],
            ))?,
            204,
        );

        let missing = fixture.exchange(&request(
            "DELETE",
            "/photos/conditional",
            "",
            &[("if-match", "*")],
        ))?;

        status(&missing, 404);
        assert!(missing.contains("<Code>NoSuchKey</Code>"));

        fixture.database.execute(
            "UPDATE principals SET policy_generation=randomblob(16), policy_json=?1",
            [r#"{"Version":"2012-10-17","Statement":{"Effect":"Allow","Action":["s3:GetObject","s3:DeleteObject"],"Resource":"*"}}"#],
        )?;

        let missing = fixture.exchange(&request(
            "DELETE",
            "/photos/conditional",
            "",
            &[("if-match", "\"different\"")],
        ))?;

        status(&missing, 404);
        assert!(missing.contains("<Code>NoSuchKey</Code>"));

        status(
            &fixture.exchange(&request("DELETE", "/photos/conditional", "", &[]))?,
            204,
        );

        assert_eq!(
            fixture
                .runtime
                .block_on(fixture.connection.head_object(read("conditional")))
                .unwrap_err()
                .failure(),
            mikrio_core::OperationFailure::NoSuchKey,
        );

        Ok(())
    })
}

#[test]
fn batch_deletion_checks_each_condition_and_permission_in_input_order() -> TestResult {
    with_fixture(|fixture| {
        for key in ["object", "blocked", "star"] {
            report_case(format_args!("key={key:?}"));

            status(
                &fixture.exchange(&request("PUT", &format!("/photos/{key}"), "123456789", &[]))?,
                200,
            );
        }

        fixture.database.execute("UPDATE principals SET policy_generation=randomblob(16), policy_json=?1", [
            r#"{"Version":"2012-10-17","Statement":[{"Effect":"Allow","Action":["s3:GetObject","s3:DeleteObject"],"Resource":"*"},{"Effect":"Deny","Action":"s3:DeleteObject","Resource":"arn:aws:s3:::photos/blocked"}]}"#,
        ])?;

        let response = fixture.exchange(&batch_request(concat!(
            "<Delete><Object><Key>object</Key><ETag>wrong</ETag></Object>",
            "<Object><Key>blocked</Key><ETag>wrong</ETag></Object>",
            "<Object><Key>object</Key><ETag>25f9e794323b453885f5181f1b624d0b</ETag></Object>",
            "<Object><Key>object</Key><ETag>&quot;25f9e794323b453885f5181f1b624d0b&quot;</ETag></Object>",
            "<Object><Key>object</Key></Object><Object><Key>missing</Key></Object></Delete>"
        ), "crc32", &[]))?;

        status(&response, 200);
        assert_eq!(response.matches("<Deleted>").count(), 3);
        assert_eq!(response.matches("<Error>").count(), 3);

        for code in ["PreconditionFailed", "AccessDenied", "NoSuchKey"] {
            report_case(format_args!("code={code:?}"));

            assert!(response.contains(&format!("<Code>{code}</Code>")));
        }

        assert_eq!(bytes(fixture, "blocked"), b"123456789");

        // A concrete validator needs GetObject as well as DeleteObject. Existence
        // does not grant or require that extra read permission.
        fixture.database.execute("UPDATE principals SET policy_generation=randomblob(16), policy_json=?1", [
            r#"{"Version":"2012-10-17","Statement":{"Effect":"Allow","Action":"s3:DeleteObject","Resource":"*"}}"#,
        ])?;

        let response = fixture.exchange(&batch_request(
            concat!(
                "<Delete><Quiet>true</Quiet><Object><Key>star</Key><ETag>wrong</ETag></Object>",
                "<Object><Key>star</Key><ETag>*</ETag></Object></Delete>"
            ),
            "md5",
            &[],
        ))?;

        status(&response, 200);
        assert!(!response.contains("<Deleted>"));
        assert_eq!(response.matches("<Error>").count(), 1);
        assert!(response.contains("<Code>AccessDenied</Code>"));

        assert_eq!(
            fixture
                .runtime
                .block_on(fixture.connection.head_object(read("star")))
                .unwrap_err()
                .failure(),
            mikrio_core::OperationFailure::NoSuchKey
        );

        Ok(())
    })
}

#[test]
fn batch_deletion_rejects_whole_envelope_faults_before_mutation() -> TestResult {
    with_fixture(|fixture| {
        status(
            &fixture.exchange(&request("PUT", "/photos/keep", "original", &[]))?,
            200,
        );

        let valid = "<Delete><Object><Key>keep</Key></Object></Delete>";

        for malformed in [
            format!("{valid}<Trailing/>"),
            "<Delete><Object><Key>keep</Key><Key>other</Key></Object></Delete>".into(),
            "<Delete/>".into(),
            "<Delete><Object><Key>keep</Key></Object><Object><Key></Key></Object></Delete>".into(),
            format!(
                "<Delete>{}</Delete>",
                "<Object><Key>keep</Key></Object>".repeat(1001)
            ),
        ] {
            report_case(format_args!("malformed={malformed:?}"));

            status(
                &fixture.exchange(&batch_request(&malformed, "md5", &[]))?,
                400,
            );

            assert_eq!(bytes(fixture, "keep"), b"original");
        }

        for headers in [
            vec![],
            vec![("content-md5", "AAAAAAAAAAAAAAAAAAAAAA==")],
            vec![("x-amz-checksum-crc32", "AAAAAA==")],
        ] {
            report_case(format_args!("headers={headers:?}"));

            status(
                &fixture.exchange(&batch_request(valid, "none", &headers))?,
                400,
            );

            assert_eq!(bytes(fixture, "keep"), b"original");
        }

        for name in [
            "x-amz-mfa",
            "x-amz-request-payer",
            "x-amz-bypass-governance-retention",
            "if-match",
        ] {
            report_case(format_args!("name={name:?}"));

            status(
                &fixture.exchange(&batch_request(valid, "md5", &[(name, "")]))?,
                501,
            );
        }

        for field in ["<VersionId/>", "<Size>0</Size>"] {
            report_case(format_args!("field={field:?}"));

            status(
                &fixture.exchange(&batch_request(
                    &format!("<Delete><Object><Key>keep</Key>{field}</Object></Delete>"),
                    "md5",
                    &[],
                ))?,
                501,
            );
        }

        status(
            &fixture.exchange(&batch_request(
                valid,
                "md5",
                &[("x-amz-expected-bucket-owner", "999999999999")],
            ))?,
            403,
        );

        assert_eq!(bytes(fixture, "keep"), b"original");
        Ok(())
    })
}

#[test]
#[allow(
    clippy::print_stderr,
    reason = "retained batch-envelope phase diagnosis"
)]
fn batch_envelope_accepts_maximum_escaped_keys_and_retains_configuration_bound() -> TestResult {
    with_fixture(|fixture| {
        let key = "&amp;".repeat(1024);

        let body = format!(
            "<Delete><Quiet>true</Quiet>{}</Delete>",
            format!("<Object><Key>{key}</Key></Object>").repeat(1000)
        );

        assert!(body.len() > 5 * 1024 * 1000);
        eprintln!("BATCH_ENVELOPE maximum_keys begin");
        let response = fixture.exchange(&batch_request(&body, "md5", &[]))?;
        eprintln!("BATCH_ENVELOPE maximum_keys settled");
        status(&response, 200);
        assert!(!response.contains("<Error>"));
        assert!(!response.contains("<Deleted>"));

        // Reject the declared over-limit body before its expensive XML decoding.
        let mut request = batch_request("<Delete/>", "md5", &[]);
        let request_text = String::from_utf8(request)?;

        request = request_text
            .replace("content-length: 9", "content-length: 8388609")
            .into_bytes();

        assert!(String::from_utf8_lossy(&request).contains("8388609"));
        eprintln!("BATCH_ENVELOPE oversized begin");
        status(&fixture.exchange(&request)?, 400);
        eprintln!("BATCH_ENVELOPE oversized settled");
        let cors = super::request("PUT", "/photos?cors", "<CORSConfiguration/>", &[]);
        let cors = String::from_utf8(cors)?.replace("content-length: 20", "content-length: 65537");
        assert!(cors.contains("65537"));
        status(&fixture.exchange(cors.as_bytes())?, 400);
        Ok(())
    })
}
