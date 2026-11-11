//! Authenticated tag routes over real SQL and payload storage.

use std::fmt::Write as _;

use super::*;

const ALL: &str =
    r#"{"Version":"2012-10-17","Statement":{"Effect":"Allow","Action":"s3:*","Resource":"*"}}"#;
const PRIVATE: &str =
    "<Tagging><TagSet><Tag><Key>visibility</Key><Value>private</Value></Tag></TagSet></Tagging>";
const PUBLIC: &str =
    "<Tagging><TagSet><Tag><Key>visibility</Key><Value>public</Value></Tag></TagSet></Tagging>";
const EMPTY: &str = "<Tagging><TagSet/></Tagging>";

fn policy(fixture: &Fixture<'_>, document: &str) -> TestResult {
    fixture.database.execute(
        "UPDATE principals SET policy_generation=randomblob(16), policy_json=?1",
        [document],
    )?;

    Ok(())
}

fn tags(
    fixture: &Fixture<'_>,
) -> Result<mikrio_core::object::ObjectTags, Box<dyn std::error::Error>> {
    Ok(fixture
        .runtime
        .block_on(fixture.connection.get_object_tags(read("object")))?)
}

#[test]
fn presigned_inline_tags_keep_query_values_and_enforce_tag_permission() -> TestResult {
    with_fixture(|fixture| {
        policy(fixture, ALL)?;
        let path = "/photos/object?x-amz-tagging=Name%3Da%252Bb%2Bspace";

        status(
            &fixture.exchange(&request_with_mode("PUT", path, "payload", &[], "query"))?,
            200,
        );

        assert_eq!(
            tags(fixture)?.entries().get("Name").map(String::as_str),
            Some("a+b space")
        );

        policy(
            fixture,
            r#"{"Version":"2012-10-17","Statement":{"Effect":"Allow","Action":"s3:PutObject","Resource":"*"}}"#,
        )?;

        for path in [path, "/photos/object?x-amz-tagging="] {
            report_case(format_args!("path={path:?}"));

            status(
                &fixture.exchange(&request_with_mode("PUT", path, "denied", &[], "query"))?,
                403,
            );

            assert_eq!(bytes(fixture, "object"), b"payload");
        }

        Ok(())
    })
}

#[test]
fn presigned_tagging_verifies_hoisted_xml_checksum_before_mutation() -> TestResult {
    with_fixture(|fixture| {
        policy(fixture, ALL)?;

        status(
            &fixture.exchange(&request("PUT", "/photos/object", "payload", &[]))?,
            200,
        );

        status(
            &fixture.exchange(&request("PUT", "/photos/object?tagging", PRIVATE, &[]))?,
            200,
        );

        let rejected = fixture.exchange(&request_with_mode(
            "PUT", "/photos/object?tagging&x-amz-checksum-crc32=AAAAAA%3D%3D&x-amz-sdk-checksum-algorithm=CRC32", EMPTY, &[], "query",
        ))?;

        status(&rejected, 400);
        assert!(rejected.contains("<Code>BadDigest</Code>"), "{rejected}");
        assert_eq!(tags(fixture)?.entries()["visibility"], "private");

        status(
            &fixture.exchange(&request_with_mode(
                "PUT",
                "/photos/object?tagging&x-amz-checksum-crc32=wsibng%3D%3D&x-amz-sdk-checksum-algorithm=CRC32",
                EMPTY,
                &[],
                "query",
            ))?,
            200,
        );

        assert!(tags(fixture)?.entries().is_empty());
        Ok(())
    })
}

#[test]
fn tag_reads_validate_supplied_empty_body_integrity() -> TestResult {
    with_fixture(|fixture| {
        policy(fixture, ALL)?;

        status(
            &fixture.exchange(&request("PUT", "/photos/object", "payload", &[]))?,
            200,
        );

        let rejected = fixture.exchange(&request(
            "GET",
            "/photos/object?tagging",
            "",
            &[("content-md5", "AAAAAAAAAAAAAAAAAAAAAA==")],
        ))?;

        status(&rejected, 400);
        assert!(rejected.contains("<Code>BadDigest</Code>"), "{rejected}");
        Ok(())
    })
}

#[test]
fn signed_tagging_rejects_tampering_ambiguity_and_hoisted_owner_mismatch() -> TestResult {
    with_fixture(|fixture| {
        policy(fixture, ALL)?;

        status(
            &fixture.exchange(&request("PUT", "/photos/object", "payload", &[]))?,
            200,
        );

        status(
            &fixture.exchange(&request("PUT", "/photos/object?tagging", PRIVATE, &[]))?,
            200,
        );

        for (path, headers) in [
            (
                "/photos/object",
                vec![("x-amz-tagging", "visibility=public")],
            ),
            ("/photos/object?x-amz-tagging=visibility%3Dpublic", vec![]),
        ] {
            report_case(format_args!("path={path:?}, headers={headers:?}"));

            let mode = if headers.is_empty() {
                "query"
            } else {
                "header"
            };

            let signed = request_with_mode("PUT", path, "replacement", &headers, mode);
            let tampered = String::from_utf8(signed)?.replace("public", "secret");
            let response = fixture.exchange(tampered.as_bytes())?;
            status(&response, 403);

            assert!(
                response.contains("<Code>SignatureDoesNotMatch</Code>"),
                "{response}"
            );
        }

        for path in [
            "/photos/object?x-amz-tagging=first%3Done&X-Amz-Tagging=second%3Dtwo",
            "/photos/object?x-amz-tagging=first%3Done",
        ] {
            report_case(format_args!("path={path:?}"));

            let headers = if path.contains("X-Amz-Tagging") {
                vec![]
            } else {
                vec![("x-amz-tagging", "third=three")]
            };

            status(
                &fixture.exchange(&request("PUT", path, "replacement", &headers))?,
                400,
            );
        }

        for method in ["GET", "PUT", "DELETE"] {
            let body = if method == "PUT" { EMPTY } else { "" };

            for (owner, expected) in [("000000000000", 403), ("", 400), ("invalid", 400)] {
                report_case(format_args!(
                    "method={method:?}, owner={owner:?}, expected={expected:?}"
                ));

                let path = format!("/photos/object?tagging&x-amz-expected-bucket-owner={owner}");

                status(
                    &fixture.exchange(&request_with_mode(method, &path, body, &[], "query"))?,
                    expected,
                );
            }
        }

        assert_eq!(tags(fixture)?.entries()["visibility"], "private");
        assert_eq!(bytes(fixture, "object"), b"payload");
        Ok(())
    })
}

#[test]
fn tagging_xml_accepts_maximum_set_and_rejects_larger_sets_and_envelopes() -> TestResult {
    with_fixture(|fixture| {
        policy(fixture, ALL)?;

        status(
            &fixture.exchange(&request("PUT", "/photos/object", "payload", &[]))?,
            200,
        );

        // Non-ASCII scalars exercise UTF-16 limits independently of the XML byte budget.
        let mut entries = String::new();

        for index in 0..10 {
            write!(
                entries,
                "<Tag><Key>{index}{}</Key><Value>{}</Value></Tag>",
                "日".repeat(127),
                "𐐀".repeat(128)
            )?;
        }

        let maximum = format!("<Tagging><TagSet>{entries}</TagSet></Tagging>");

        status(
            &fixture.exchange(&request("PUT", "/photos/object?tagging", &maximum, &[]))?,
            200,
        );

        let expected = tags(fixture)?;
        assert_eq!(expected.count(), 10);

        for invalid in [
            maximum.replace("</TagSet>", "<Tag><Key>eleven</Key><Value/></Tag></TagSet>"),
            format!(
                "<Tagging><TagSet><Tag><Key>{}</Key><Value/></Tag></TagSet></Tagging>",
                "日".repeat(129)
            ),
            format!(
                "<Tagging><TagSet><Tag><Key>key</Key><Value>{}</Value></Tag></TagSet></Tagging>",
                "𐐀".repeat(129)
            ),
        ] {
            report_case(format_args!("invalid={invalid:?}"));

            status(
                &fixture.exchange(&request("PUT", "/photos/object?tagging", &invalid, &[]))?,
                400,
            );

            assert_eq!(tags(fixture)?, expected);
        }

        // Over-limit declarations can reject before the body is read. Send only
        // headers so the response is not lost to a reset with unread client bytes.
        let oversized = format!("<Tagging>{}<TagSet/></Tagging>", " ".repeat(65_536));
        let raw = request("PUT", "/photos/object?tagging", &oversized, &[]);

        let headers_end = raw
            .windows(4)
            .position(|bytes| bytes == b"\r\n\r\n")
            .unwrap()
            + 4;

        status(&fixture.exchange(&raw[..headers_end])?, 400);
        assert_eq!(tags(fixture)?, expected);
        Ok(())
    })
}

#[test]
fn tagging_xml_round_trips_and_invalid_envelopes_preserve_tags_and_payload_facts() -> TestResult {
    with_fixture(|fixture| {
        policy(fixture, ALL)?;

        status(
            &fixture.exchange(&request(
                "PUT",
                "/photos/object",
                "payload",
                &[("content-type", "text/plain")],
            ))?,
            200,
        );

        let before = fixture
            .runtime
            .block_on(fixture.connection.head_object(read("object")))?;

        let xml = "<Tagging xmlns=\"http://s3.amazonaws.com/doc/2006-03-01/\"><TagSet><Tag><Key>Name</Key><Value/></Tag><Tag><Key>日本語</Key><Value>&#x10400;</Value></Tag></TagSet></Tagging>";

        status(
            &fixture.exchange(&request("PUT", "/photos/object?tagging", xml, &[]))?,
            200,
        );

        let expected = tags(fixture)?;
        assert_eq!(expected.entries()["Name"], "");
        assert_eq!(expected.entries()["日本語"], "𐐀");
        let response = fixture.exchange(&request("GET", "/photos/object?tagging", "", &[]))?;
        status(&response, 200);

        assert!(
            response.contains("<Key>Name</Key><Value></Value>")
                || response.contains("<Key>Name</Key><Value/>")
        );

        assert!(response.contains("<Key>日本語</Key>"));

        for bad in [
            "<Tagging/>",
            "<Tagging><TagSet/><TagSet/></Tagging>",
            "<Tagging><TagSet/><Unknown/></Tagging>",
            "<Tagging><TagSet><Tag><Key>key</Key></Tag></TagSet></Tagging>",
            "<Tagging><TagSet><Tag><Value>value</Value></Tag></TagSet></Tagging>",
            "<Tagging><TagSet><Tag><Key>key</Key><Key>other</Key><Value/></Tag></TagSet></Tagging>",
            "<Tagging><TagSet><Tag><Key>key</Key><Value/><Value/></Tag></TagSet></Tagging>",
            "<!DOCTYPE Tagging [<!ENTITY external SYSTEM \"file:///etc/passwd\">]><Tagging><TagSet/></Tagging>",
            "<Tagging><TagSet/></Tagging><Tagging><TagSet/></Tagging>",
        ] {
            report_case(format_args!("bad={bad:?}"));

            let rejected = fixture.exchange(&request("PUT", "/photos/object?tagging", bad, &[]))?;
            status(&rejected, 400);
            assert!(rejected.contains("<Code>MalformedXML</Code>"), "{rejected}");
            assert_eq!(tags(fixture)?, expected);
        }

        for bad in [
            "<Tagging><TagSet><Tag><Key>duplicate</Key><Value>one</Value></Tag><Tag><Key>duplicate</Key><Value>two</Value></Tag></TagSet></Tagging>",
            "<Tagging><TagSet><Tag><Key>AWS:reserved</Key><Value/></Tag></TagSet></Tagging>",
            "<Tagging><TagSet><Tag><Key/><Value/></Tag></TagSet></Tagging>",
            "<Tagging><TagSet><Tag><Key>key</Key><Value>&#9;</Value></Tag></TagSet></Tagging>",
        ] {
            report_case(format_args!("bad={bad:?}"));

            let rejected = fixture.exchange(&request("PUT", "/photos/object?tagging", bad, &[]))?;
            status(&rejected, 400);
            assert!(rejected.contains("<Code>InvalidTag</Code>"), "{rejected}");
            assert_eq!(tags(fixture)?, expected);
        }

        for headers in [
            vec![("content-md5", "AAAAAAAAAAAAAAAAAAAAAA==")],
            vec![("x-amz-sdk-checksum-algorithm", "SHA256")],
            vec![
                ("x-amz-sdk-checksum-algorithm", "CRC32"),
                ("x-amz-checksum-crc32", "AAAAAA=="),
            ],
        ] {
            report_case(format_args!("headers={headers:?}"));

            status(
                &fixture.exchange(&request("PUT", "/photos/object?tagging", EMPTY, &headers))?,
                400,
            );

            assert_eq!(tags(fixture)?, expected);
        }

        for method in ["GET", "DELETE"] {
            report_case(format_args!("method={method:?}"));

            status(
                &fixture.exchange(&request(
                    method,
                    "/photos/object?tagging",
                    "unexpected",
                    &[],
                ))?,
                400,
            );

            assert_eq!(tags(fixture)?, expected);
        }

        status(
            &fixture.exchange(&request(
                "DELETE",
                "/photos/object?tagging",
                "",
                &[("if-match", "*")],
            ))?,
            501,
        );

        status(
            &fixture.exchange(&request(
                "GET",
                "/photos/object?tagging&versionId=version",
                "",
                &[],
            ))?,
            501,
        );

        assert_eq!(tags(fixture)?, expected);

        let after = fixture
            .runtime
            .block_on(fixture.connection.head_object(read("object")))?;

        assert_eq!(after.etag, before.etag);
        assert_eq!(after.last_modified, before.last_modified);
        assert_eq!(after.checksums, before.checksums);
        assert_eq!(after.representation, before.representation);
        assert_eq!(bytes(fixture, "object"), b"payload");

        status(
            &fixture.exchange(&request(
                "PUT",
                "/photos/object?tagging",
                EMPTY,
                &[
                    ("content-md5", "MiK6ZqQ1QvDEa+XYsdLcQg=="),
                    ("x-amz-sdk-checksum-algorithm", "CRC32"),
                    ("x-amz-checksum-crc32", "wsibng=="),
                ],
            ))?,
            200,
        );

        assert!(tags(fixture)?.entries().is_empty());

        for attempt in 0..2 {
            report_case(format_args!("delete_attempt={attempt}"));

            status(
                &fixture.exchange(&request("DELETE", "/photos/object?tagging", "", &[]))?,
                204,
            );
        }

        Ok(())
    })
}

#[test]
fn tagging_permissions_use_old_and_requested_values_and_distinguish_empty_from_absent() -> TestResult
{
    with_fixture(|fixture| {
        policy(fixture, ALL)?;

        status(
            &fixture.exchange(&request("PUT", "/photos/object", "payload", &[]))?,
            200,
        );

        status(
            &fixture.exchange(&request("PUT", "/photos/object?tagging", PRIVATE, &[]))?,
            200,
        );

        policy(
            fixture,
            r#"{"Version":"2012-10-17","Statement":{"Effect":"Allow","Action":"s3:GetObjectTagging","Resource":"*"}}"#,
        )?;

        status(
            &fixture.exchange(&request("GET", "/photos/object?tagging", "", &[]))?,
            200,
        );

        status(
            &fixture.exchange(&request("GET", "/photos/object", "", &[]))?,
            403,
        );

        status(
            &fixture.exchange(&request("PUT", "/photos/object?tagging", PUBLIC, &[]))?,
            403,
        );

        status(
            &fixture.exchange(&request("DELETE", "/photos/object?tagging", "", &[]))?,
            403,
        );

        assert_eq!(tags(fixture)?.entries()["visibility"], "private");

        policy(
            fixture,
            r#"{"Version":"2012-10-17","Statement":{"Effect":"Allow","Action":"s3:PutObjectTagging","Resource":"*","Condition":{"StringEquals":{"s3:ExistingObjectTag/Visibility":"private","s3:RequestObjectTag/VISIBILITY":"public"},"ForAllValues:StringEquals":{"s3:RequestObjectTagKeys":"visibility"},"Null":{"s3:RequestObjectTagKeys":"false"}}}}"#,
        )?;

        status(
            &fixture.exchange(&request("PUT", "/photos/object?tagging", PUBLIC, &[]))?,
            200,
        );

        status(
            &fixture.exchange(&request("PUT", "/photos/object?tagging", PUBLIC, &[]))?,
            403,
        );

        assert_eq!(tags(fixture)?.entries()["visibility"], "public");

        policy(
            fixture,
            r#"{"Version":"2012-10-17","Statement":{"Effect":"Allow","Action":["s3:PutObjectTagging","s3:DeleteObjectTagging"],"Resource":"*","Condition":{"Null":{"s3:RequestObjectTagKeys":"false"}}}}"#,
        )?;

        status(
            &fixture.exchange(&request("DELETE", "/photos/object?tagging", "", &[]))?,
            403,
        );

        status(
            &fixture.exchange(&request("PUT", "/photos/object?tagging", EMPTY, &[]))?,
            200,
        );

        assert!(tags(fixture)?.entries().is_empty());

        policy(
            fixture,
            r#"{"Version":"2012-10-17","Statement":{"Effect":"Allow","Action":"s3:DeleteObjectTagging","Resource":"*","Condition":{"Null":{"s3:RequestObjectTagKeys":"true"}}}}"#,
        )?;

        status(
            &fixture.exchange(&request("DELETE", "/photos/object?tagging", "", &[]))?,
            204,
        );

        for method in ["GET", "PUT", "DELETE"] {
            report_case(format_args!("method={method:?}"));

            let payload = if method == "PUT" { EMPTY } else { "" };

            status(
                &fixture.exchange(&request(method, "/photos/missing?tagging", payload, &[]))?,
                403,
            );
        }

        policy(fixture, ALL)?;

        for method in ["GET", "PUT", "DELETE"] {
            report_case(format_args!("method={method:?}"));

            let payload = if method == "PUT" { EMPTY } else { "" };

            let missing =
                fixture.exchange(&request(method, "/photos/missing?tagging", payload, &[]))?;

            status(&missing, 404);
            assert!(missing.contains("<Code>NoSuchKey</Code>"));

            status(
                &fixture.exchange(&request(
                    method,
                    "/photos/object?tagging",
                    payload,
                    &[("x-amz-expected-bucket-owner", "000000000000")],
                ))?,
                403,
            );
        }

        Ok(())
    })
}

#[test]
fn inline_tagging_is_atomic_preserves_exact_encoding_and_requires_its_own_permission() -> TestResult
{
    with_fixture(|fixture| {
        policy(fixture, ALL)?;

        status(
            &fixture.exchange(&request(
                "PUT",
                "/photos/object",
                "original",
                &[(
                    "x-amz-tagging",
                    "Name=a%2Bb+space&%E6%97%A5%E6%9C%AC%E8%AA%9E=%F0%90%90%80",
                )],
            ))?,
            200,
        );

        let expected = tags(fixture)?;
        assert_eq!(expected.entries()["Name"], "a+b space");
        assert_eq!(expected.entries()["日本語"], "𐐀");

        for malformed in [
            "Name=%",
            "Name=%GG",
            "Name=%FF",
            "Name",
            "Name=value&",
            "Name=a&Name=b",
            "aws%3Aname=value",
        ] {
            report_case(format_args!("malformed={malformed:?}"));

            status(
                &fixture.exchange(&request(
                    "PUT",
                    "/photos/object",
                    "rejected",
                    &[("x-amz-tagging", malformed)],
                ))?,
                400,
            );

            assert_eq!(tags(fixture)?, expected);
            assert_eq!(bytes(fixture, "object"), b"original");
        }

        policy(
            fixture,
            r#"{"Version":"2012-10-17","Statement":{"Effect":"Allow","Action":"s3:PutObject","Resource":"*"}}"#,
        )?;

        for header in ["", "Name=updated"] {
            report_case(format_args!("header={header:?}"));

            status(
                &fixture.exchange(&request(
                    "PUT",
                    "/photos/object",
                    "denied",
                    &[("x-amz-tagging", header)],
                ))?,
                403,
            );

            assert_eq!(tags(fixture)?, expected);
            assert_eq!(bytes(fixture, "object"), b"original");
        }

        status(
            &fixture.exchange(&request("PUT", "/photos/object", "replacement", &[]))?,
            200,
        );

        assert!(tags(fixture)?.entries().is_empty());
        policy(fixture, ALL)?;

        status(
            &fixture.exchange(&request(
                "PUT",
                "/photos/object",
                "explicit empty",
                &[("x-amz-tagging", "")],
            ))?,
            200,
        );

        assert!(tags(fixture)?.entries().is_empty());
        Ok(())
    })
}

#[test]
fn tagging_count_disclosure_and_case_collision_denial_cannot_bypass_read_authorization()
-> TestResult {
    with_fixture(|fixture| {
        policy(fixture, ALL)?;

        status(
            &fixture.exchange(&request("PUT", "/photos/object", "payload", &[]))?,
            200,
        );

        status(
            &fixture.exchange(&request("PUT", "/photos/object?tagging", PUBLIC, &[]))?,
            200,
        );

        for method in ["GET", "HEAD"] {
            report_case(format_args!("method={method:?}"));

            let response = fixture.exchange(&request(method, "/photos/object", "", &[]))?;
            status(&response, 200);
            assert!(response.contains("x-amz-tagging-count: 1"));
            assert!(!response.contains("visibility"));

            let ranged = fixture.exchange(&request(
                method,
                "/photos/object",
                "",
                &[("range", "bytes=0-2")],
            ))?;

            status(&ranged, if method == "GET" { 206 } else { 200 });
            assert!(ranged.contains("x-amz-tagging-count: 1"));

            for (name, value, code) in [
                ("if-none-match", "*", 304),
                ("if-match", "\"different\"", 412),
            ] {
                report_case(format_args!(
                    "method={method:?}, name={name:?}, value={value:?}, code={code:?}"
                ));

                let conditional =
                    fixture.exchange(&request(method, "/photos/object", "", &[(name, value)]))?;

                status(&conditional, code);
                assert!(!conditional.contains("x-amz-tagging-count"));
            }
        }

        policy(
            fixture,
            r#"{"Version":"2012-10-17","Statement":[{"Effect":"Allow","Action":"s3:GetObject","Resource":"*"},{"Effect":"Deny","Action":"s3:GetObjectTagging","Resource":"*"}]}"#,
        )?;

        for method in ["GET", "HEAD"] {
            report_case(format_args!("method={method:?}"));

            let response = fixture.exchange(&request(method, "/photos/object", "", &[]))?;
            status(&response, 200);
            assert!(!response.contains("x-amz-tagging-count"));
        }

        policy(fixture, ALL)?;
        let colliding = "<Tagging><TagSet><Tag><Key>Visibility</Key><Value>private</Value></Tag><Tag><Key>visibility</Key><Value>public</Value></Tag></TagSet></Tagging>";

        status(
            &fixture.exchange(&request("PUT", "/photos/object?tagging", colliding, &[]))?,
            403,
        );

        assert_eq!(tags(fixture)?.entries()["visibility"], "public");

        fixture
            .runtime
            .block_on(fixture.connection.replace_object_tags(
                read("object"),
                mikrio_core::object::ObjectTags::new([
                    ("Visibility".into(), "private".into()),
                    ("visibility".into(), "public".into()),
                ])?,
            ))?;

        for path in ["/photos/object", "/photos/object?tagging"] {
            report_case(format_args!("path={path:?}"));

            status(&fixture.exchange(&request("GET", path, "", &[]))?, 403);
        }

        assert_eq!(tags(fixture)?.count(), 2);

        fixture
            .runtime
            .block_on(fixture.connection.delete_object_tags(read("object")))?;

        for method in ["GET", "HEAD"] {
            report_case(format_args!("method={method:?}"));

            let untagged = fixture.exchange(&request(method, "/photos/object", "", &[]))?;
            status(&untagged, 200);
            assert!(!untagged.contains("x-amz-tagging-count"));
        }

        Ok(())
    })
}
