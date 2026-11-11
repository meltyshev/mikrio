//! Wire requests signed independently of the production adapter.

use super::*;

#[test]
fn single_ranges_project_exact_bytes_lengths_and_bodyless_head_errors() -> TestResult {
    with_fixture(|fixture| {
        status(
            &fixture.exchange(&request("PUT", "/photos/object", "testcontent", &[]))?,
            200,
        );

        status(
            &fixture.exchange(&request("PUT", "/photos/empty", "", &[]))?,
            200,
        );

        for method in ["GET", "HEAD"] {
            report_case(format_args!("method={method:?}"));

            for (range, content_range, payload) in [
                ("bytes=4-7", "bytes 4-7/11", "cont"),
                ("bytes=4-", "bytes 4-10/11", "content"),
                ("bytes=-7", "bytes 4-10/11", "content"),
                ("bytes=4-999", "bytes 4-10/11", "content"),
                ("bytes=-999", "bytes 0-10/11", "testcontent"),
            ] {
                report_case(format_args!(
                    "method={method:?}, range={range:?}, content_range={content_range:?}"
                ));

                let response = fixture.exchange(&request(
                    method,
                    "/photos/object",
                    "",
                    &[("range", range), ("x-amz-checksum-mode", "ENABLED")],
                ))?;

                status(&response, if method == "GET" { 206 } else { 200 });
                assert!(response.contains(&format!("content-length: {}\r\n", payload.len())));
                assert!(response.contains(&format!("content-range: {content_range}\r\n")));
                assert!(!response.contains("x-amz-checksum-"));

                assert_eq!(
                    response.split_once("\r\n\r\n").unwrap().1,
                    if method == "GET" { payload } else { "" }
                );
            }

            for (path, range) in [
                ("/photos/object", "bytes=40-50"),
                ("/photos/object", "bytes=-0"),
                ("/photos/empty", "bytes=0-"),
                ("/photos/empty", "bytes=-1"),
            ] {
                report_case(format_args!(
                    "method={method:?}, path={path:?}, range={range:?}"
                ));

                let response = fixture.exchange(&request(method, path, "", &[("range", range)]))?;
                status(&response, 416);

                if method == "HEAD" {
                    assert_eq!(response.split_once("\r\n\r\n").unwrap().1, "");
                } else {
                    assert!(response.contains("<Code>InvalidRange</Code>"));
                }
            }

            for range in [
                "",
                "bytes=-",
                "bytes=1-0",
                "bytes=0-1,4-5",
                "bytes=0-18446744073709551616",
            ] {
                report_case(format_args!("method={method:?}, range={range:?}"));

                status(
                    &fixture.exchange(&request(
                        method,
                        "/photos/object",
                        "",
                        &[("range", range)],
                    ))?,
                    400,
                );
            }

            let response = fixture.exchange(&request(
                method,
                "/photos/object",
                "",
                &[("range", "bytes=40-50"), ("if-none-match", "*")],
            ))?;

            status(&response, 304);
            assert_eq!(response.split_once("\r\n\r\n").unwrap().1, "");
        }

        Ok(())
    })
}

#[test]
fn range_endpoints_use_checked_unsigned_parsing_before_length_selection() -> TestResult {
    with_fixture(|fixture| {
        status(
            &fixture.exchange(&request("PUT", "/photos/object", "testcontent", &[]))?,
            200,
        );

        for method in ["GET", "HEAD"] {
            report_case(format_args!("method={method:?}"));

            let response = fixture.exchange(&request(
                method,
                "/photos/object",
                "",
                &[("range", "bytes=0-18446744073709551615")],
            ))?;

            status(&response, if method == "GET" { 206 } else { 200 });
            assert!(response.contains("content-length: 11\r\n"));

            status(
                &fixture.exchange(&request(
                    method,
                    "/photos/object",
                    "",
                    &[("range", "bytes=18446744073709551615-")],
                ))?,
                416,
            );
        }

        Ok(())
    })
}

#[test]
fn all_six_response_overrides_are_signed_detached_and_reject_anonymous_requests() -> TestResult {
    with_fixture(|fixture| {
        status(
            &fixture.exchange(&request(
                "PUT",
                "/photos/object",
                "value",
                &[("content-type", "text/plain"), ("cache-control", "private")],
            ))?,
            200,
        );

        let selected = fixture
            .runtime
            .block_on(fixture.connection.head_object(read("object")))?;

        let target = "/photos/object?response-content-type=application%2Fpdf&response-content-encoding=identity&response-content-disposition=attachment&response-content-language=&response-cache-control=no-store&response-expires=Wed%2C%2021%20Oct%202037%2007%3A28%3A00%20GMT";

        for method in ["GET", "HEAD"] {
            for mode in ["header", "query"] {
                report_case(format_args!("method={method:?}, mode={mode:?}"));

                let response =
                    fixture.exchange(&request_with_mode(method, target, "", &[], mode))?;

                status(&response, 200);

                for field in [
                    "content-type: application/pdf\r\n",
                    "content-encoding: identity\r\n",
                    "content-disposition: attachment\r\n",
                    "content-language: \r\n",
                    "cache-control: no-store\r\n",
                    "expires: Wed, 21 Oct 2037 07:28:00 GMT\r\n",
                ] {
                    assert!(response.contains(field), "missing {field:?}: {response}");
                }

                let signed = String::from_utf8(request_with_mode(method, target, "", &[], mode))?;

                status(
                    &fixture.exchange(
                        signed
                            .replace("application%2Fpdf", "application%2Fzip")
                            .as_bytes(),
                    )?,
                    403,
                );
            }

            for value in ["%0D%0AX-Injected%3Ayes", "%C3%A9"] {
                report_case(format_args!("method={method:?}, value={value:?}"));

                let response = fixture.exchange(&request(
                    method,
                    &format!("/photos/object?response-content-type={value}"),
                    "",
                    &[],
                ))?;

                status(&response, 400);
                assert!(!response.contains("x-injected:"));
            }
        }

        let after = fixture
            .runtime
            .block_on(fixture.connection.head_object(read("object")))?;

        assert_eq!(after.representation, selected.representation);
        assert_eq!(after.generation, selected.generation);
        assert_eq!(after.last_modified, selected.last_modified);

        fixture.database.execute("UPDATE buckets SET block_public_acls=0, ignore_public_acls=0, block_public_policy=0, restrict_public_buckets=0, policy_generation=randomblob(16), policy_json=?1", [r#"{"Version":"2012-10-17","Statement":{"Effect":"Allow","Principal":"*","Action":"s3:GetObject","Resource":"arn:aws:s3:::photos/*"}}"#])?;

        for method in ["GET", "HEAD"] {
            report_case(format_args!("method={method:?}"));

            let raw = format!(
                "{method} /photos/object HTTP/1.1\r\nhost: localhost\r\nconnection: close\r\n\r\n"
            );

            status(&fixture.exchange(raw.as_bytes())?, 200);

            for query in ["response-content-type=", "response-cache-control=public"] {
                report_case(format_args!("method={method:?}, query={query:?}"));

                let raw = format!(
                    "{method} /photos/object?{query} HTTP/1.1\r\nhost: localhost\r\nconnection: close\r\n\r\n"
                );

                status(&fixture.exchange(raw.as_bytes())?, 403);
            }
        }

        Ok(())
    })
}
