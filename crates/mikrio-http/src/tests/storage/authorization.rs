//! Operation-to-action checks on authenticated requests over the real worker.

use super::*;

const ADMIN: &str =
    r#"{"Version":"2012-10-17","Statement":{"Effect":"Allow","Action":"s3:*","Resource":"*"}}"#;
const POLICY: &str = r#"{"Version":"2012-10-17","Statement":{"Effect":"Deny","Principal":"*","Action":"s3:GetObject","Resource":"arn:aws:s3:::photos/blocked"}}"#;
const CORS: &str = "<CORSConfiguration><CORSRule><AllowedOrigin>https://app.example</AllowedOrigin><AllowedMethod>GET</AllowedMethod></CORSRule></CORSConfiguration>";
const LIFECYCLE: &str = "<LifecycleConfiguration><Rule><ID>disabled</ID><Filter/><Status>Disabled</Status><Expiration><Days>7</Days></Expiration></Rule></LifecycleConfiguration>";
const BLOCK: &str = "<PublicAccessBlockConfiguration><BlockPublicPolicy>true</BlockPublicPolicy></PublicAccessBlockConfiguration>";
const OWNERSHIP: &str = "<OwnershipControls><Rule><ObjectOwnership>BucketOwnerEnforced</ObjectOwnership></Rule></OwnershipControls>";
const TAGS: &str =
    "<Tagging><TagSet><Tag><Key>topic</Key><Value>test</Value></Tag></TagSet></Tagging>";
const COMPLETION: &str = "<CompleteMultipartUpload><Part><PartNumber>1</PartNumber><ETag>\"900150983cd24fb0d6963f7d28e17f72\"</ETag></Part></CompleteMultipartUpload>";

struct Case<'a> {
    operation: &'static str,
    actions: &'a [&'a str],
    method: &'static str,
    target: &'static str,
    body: &'static str,
    headers: &'static [(&'static str, &'static str)],
    status: u16,
}

fn policy(fixture: &Fixture<'_>, document: &str) -> TestResult {
    fixture.database.execute(
        "UPDATE principals SET policy_generation=randomblob(16), policy_json=?1",
        [document],
    )?;

    Ok(())
}

fn verify(case: &Case<'_>) -> TestResult {
    with_fixture(|fixture| {
        report_case(format_args!(
            "operation={}, actions={:?}",
            case.operation, case.actions
        ));

        policy(fixture, ADMIN)?;

        for (method, target, body) in [
            ("PUT", "/empty", ""),
            ("PUT", "/photos/source", "source bytes"),
            ("PUT", "/photos/object", "predecessor"),
            ("PUT", "/photos?policy", POLICY),
            ("PUT", "/photos?cors", CORS),
            ("PUT", "/photos?lifecycle", LIFECYCLE),
            ("PUT", "/photos?publicAccessBlock", BLOCK),
            ("PUT", "/photos?ownershipControls", OWNERSHIP),
        ] {
            status(
                &fixture.exchange(&request(method, target, body, &[]))?,
                if target == "/photos?policy" { 204 } else { 200 },
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

        let part = format!("/photos/object?uploadId={upload_id}&partNumber=1");
        status(&fixture.exchange(&request("PUT", &part, "abc", &[]))?, 200);
        let target = case.target.replace("UPLOAD", upload_id);
        let mut headers = case.headers.to_vec();

        // These client-supplied values never construct a native capability or
        // replace the authenticated identity/current policy at the worker.
        headers.extend([
            ("forwarded", "for=127.0.0.1;proto=https;host=owner.example"),
            ("x-forwarded-for", "127.0.0.1"),
            ("x-forwarded-proto", "https"),
            ("x-mikrio-authority", "native"),
        ]);

        let raw = if case.operation == "PostObject" {
            let form = String::from_utf8(form("valid"))?;

            form.replacen(
                "\r\n\r\n",
                "\r\nforwarded: for=127.0.0.1;proto=https\r\nx-mikrio-authority: native\r\n\r\n",
                1,
            )
            .into_bytes()
        } else {
            request(case.method, &target, case.body, &headers)
        };

        let allow = serde_json::json!({"Effect":"Allow", "Action":case.actions, "Resource":"*"});

        for statement in [
            serde_json::json!([{"Effect":"Deny", "Action":"s3:*", "Resource":"*"}]),
            serde_json::json!([allow.clone(), {"Effect":"Deny", "Action":case.actions, "Resource":"*"}]),
        ] {
            policy(
                fixture,
                &serde_json::json!({"Version":"2012-10-17", "Statement":statement}).to_string(),
            )?;

            let response = fixture.exchange(&raw)?;

            status(
                &response,
                if case.operation == "DeleteObjects" {
                    200
                } else {
                    403
                },
            );

            if case.method != "HEAD" {
                assert!(
                    response.contains("<Code>AccessDenied</Code>"),
                    "{}",
                    case.operation
                );
            }

            assert_eq!(bytes(fixture, "object"), b"predecessor");
            assert_eq!(bytes(fixture, "source"), b"source bytes");
        }

        policy(
            fixture,
            &serde_json::json!({"Version":"2012-10-17", "Statement":allow}).to_string(),
        )?;

        let response = fixture.exchange(&raw)?;
        status(&response, case.status);

        assert!(
            !response.contains("<Error>"),
            "{}: {response}",
            case.operation
        );

        assert_eq!(bytes(fixture, "source"), b"source bytes");
        fixture.database.execute("DELETE FROM access_keys", [])?;
        let response = fixture.exchange(&raw)?;
        status(&response, 403);

        if case.method != "HEAD" {
            assert!(
                response.contains("<Code>AccessDenied</Code>"),
                "{}",
                case.operation
            );
        }

        assert_eq!(bytes(fixture, "source"), b"source bytes");
        Ok(())
    })
}

#[test]
fn bucket_operations_require_their_named_actions_and_observe_live_key_revocation() -> TestResult {
    for (operation, action, method, target, body, status) in [
        ("CreateBucket", "CreateBucket", "PUT", "/created", "", 200),
        ("ListBuckets", "ListAllMyBuckets", "GET", "/", "", 200),
        ("HeadBucket", "ListBucket", "HEAD", "/photos", "", 200),
        (
            "ListObjectsV2",
            "ListBucket",
            "GET",
            "/photos?list-type=2",
            "",
            200,
        ),
        ("DeleteBucket", "DeleteBucket", "DELETE", "/empty", "", 204),
        (
            "GetBucketLocation",
            "GetBucketLocation",
            "GET",
            "/photos?location",
            "",
            200,
        ),
        (
            "ListMultipartUploads",
            "ListBucketMultipartUploads",
            "GET",
            "/photos?uploads",
            "",
            200,
        ),
    ] {
        let action = format!("s3:{action}");

        verify(&Case {
            operation,
            actions: &[action.as_str()],
            method,
            target,
            body,
            headers: &[],
            status,
        })?;
    }

    Ok(())
}

#[test]
fn configuration_operations_require_their_named_actions_and_observe_live_key_revocation()
-> TestResult {
    for (operation, action, method, target, body, status) in [
        (
            "PutBucketCors",
            "PutBucketCORS",
            "PUT",
            "/photos?cors",
            CORS,
            200,
        ),
        (
            "GetBucketCors",
            "GetBucketCORS",
            "GET",
            "/photos?cors",
            "",
            200,
        ),
        (
            "DeleteBucketCors",
            "PutBucketCORS",
            "DELETE",
            "/photos?cors",
            "",
            204,
        ),
        (
            "PutBucketLifecycleConfiguration",
            "PutLifecycleConfiguration",
            "PUT",
            "/photos?lifecycle",
            LIFECYCLE,
            200,
        ),
        (
            "GetBucketLifecycleConfiguration",
            "GetLifecycleConfiguration",
            "GET",
            "/photos?lifecycle",
            "",
            200,
        ),
        (
            "DeleteBucketLifecycle",
            "PutLifecycleConfiguration",
            "DELETE",
            "/photos?lifecycle",
            "",
            204,
        ),
        (
            "PutBucketPolicy",
            "PutBucketPolicy",
            "PUT",
            "/photos?policy",
            POLICY,
            204,
        ),
        (
            "GetBucketPolicy",
            "GetBucketPolicy",
            "GET",
            "/photos?policy",
            "",
            200,
        ),
        (
            "DeleteBucketPolicy",
            "DeleteBucketPolicy",
            "DELETE",
            "/photos?policy",
            "",
            204,
        ),
        (
            "GetBucketPolicyStatus",
            "GetBucketPolicyStatus",
            "GET",
            "/photos?policyStatus",
            "",
            200,
        ),
        (
            "PutPublicAccessBlock",
            "PutBucketPublicAccessBlock",
            "PUT",
            "/photos?publicAccessBlock",
            BLOCK,
            200,
        ),
        (
            "GetPublicAccessBlock",
            "GetBucketPublicAccessBlock",
            "GET",
            "/photos?publicAccessBlock",
            "",
            200,
        ),
        (
            "DeletePublicAccessBlock",
            "PutBucketPublicAccessBlock",
            "DELETE",
            "/photos?publicAccessBlock",
            "",
            204,
        ),
        (
            "PutBucketOwnershipControls",
            "PutBucketOwnershipControls",
            "PUT",
            "/photos?ownershipControls",
            OWNERSHIP,
            200,
        ),
        (
            "GetBucketOwnershipControls",
            "GetBucketOwnershipControls",
            "GET",
            "/photos?ownershipControls",
            "",
            200,
        ),
        (
            "GetBucketAcl",
            "GetBucketAcl",
            "GET",
            "/photos?acl",
            "",
            200,
        ),
    ] {
        let action = format!("s3:{action}");

        verify(&Case {
            operation,
            actions: &[action.as_str()],
            method,
            target,
            body,
            headers: &[],
            status,
        })?;
    }

    Ok(())
}

#[test]
fn object_operations_require_their_named_actions_and_observe_live_key_revocation() -> TestResult {
    for (operation, actions, method, target, body, headers, status) in [
        (
            "PutObject",
            &["s3:PutObject"][..],
            "PUT",
            "/photos/object",
            "replacement",
            &[][..],
            200,
        ),
        (
            "PostObject",
            &["s3:PutObject"],
            "POST",
            "/photos",
            "",
            &[],
            204,
        ),
        (
            "GetObject",
            &["s3:GetObject"],
            "GET",
            "/photos/object",
            "",
            &[],
            200,
        ),
        (
            "HeadObject",
            &["s3:GetObject"],
            "HEAD",
            "/photos/object",
            "",
            &[],
            200,
        ),
        (
            "DeleteObject",
            &["s3:DeleteObject"],
            "DELETE",
            "/photos/object",
            "",
            &[],
            204,
        ),
        (
            "DeleteObjects",
            &["s3:DeleteObject"],
            "POST",
            "/photos?delete",
            "<Delete><Object><Key>object</Key></Object></Delete>",
            &[("content-md5", "oCgHlpyAMk6nbRtw6t4M6A==")],
            200,
        ),
        (
            "CopyObject",
            &["s3:GetObject", "s3:PutObject"],
            "PUT",
            "/photos/object",
            "",
            &[("x-amz-copy-source", "/photos/source")],
            200,
        ),
        (
            "GetObjectAttributes",
            &["s3:GetObject", "s3:GetObjectAttributes"],
            "GET",
            "/photos/object?attributes",
            "",
            &[("x-amz-object-attributes", "ETag")],
            200,
        ),
        (
            "GetObjectAcl",
            &["s3:GetObjectAcl"],
            "GET",
            "/photos/object?acl",
            "",
            &[],
            200,
        ),
        (
            "PutObjectTagging",
            &["s3:PutObjectTagging"],
            "PUT",
            "/photos/object?tagging",
            TAGS,
            &[],
            200,
        ),
        (
            "GetObjectTagging",
            &["s3:GetObjectTagging"],
            "GET",
            "/photos/object?tagging",
            "",
            &[],
            200,
        ),
        (
            "DeleteObjectTagging",
            &["s3:DeleteObjectTagging"],
            "DELETE",
            "/photos/object?tagging",
            "",
            &[],
            204,
        ),
    ] {
        verify(&Case {
            operation,
            actions,
            method,
            target,
            body,
            headers,
            status,
        })?;
    }

    Ok(())
}

#[test]
fn multipart_operations_require_their_named_actions_and_observe_live_key_revocation() -> TestResult
{
    for (operation, actions, method, target, body, headers, status) in [
        (
            "CreateMultipartUpload",
            &["s3:PutObject"][..],
            "POST",
            "/photos/object?uploads",
            "",
            &[][..],
            200,
        ),
        (
            "UploadPart",
            &["s3:PutObject"],
            "PUT",
            "/photos/object?uploadId=UPLOAD&partNumber=1",
            "replacement",
            &[],
            200,
        ),
        (
            "UploadPartCopy",
            &["s3:GetObject", "s3:PutObject"],
            "PUT",
            "/photos/object?uploadId=UPLOAD&partNumber=1",
            "",
            &[("x-amz-copy-source", "/photos/source")],
            200,
        ),
        (
            "CompleteMultipartUpload",
            &["s3:PutObject"],
            "POST",
            "/photos/object?uploadId=UPLOAD",
            COMPLETION,
            &[],
            200,
        ),
        (
            "ListParts",
            &["s3:ListMultipartUploadParts"],
            "GET",
            "/photos/object?uploadId=UPLOAD",
            "",
            &[],
            200,
        ),
        (
            "AbortMultipartUpload",
            &["s3:AbortMultipartUpload"],
            "DELETE",
            "/photos/object?uploadId=UPLOAD",
            "",
            &[],
            204,
        ),
    ] {
        verify(&Case {
            operation,
            actions,
            method,
            target,
            body,
            headers,
            status,
        })?;
    }

    Ok(())
}
