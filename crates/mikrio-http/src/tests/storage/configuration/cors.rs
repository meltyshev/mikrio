use super::*;

const CORS: &str = "<CORSConfiguration><CORSRule><ID>browser</ID><AllowedOrigin>https://app.example</AllowedOrigin><AllowedMethod>PUT</AllowedMethod><AllowedMethod>GET</AllowedMethod><AllowedMethod>HEAD</AllowedMethod><AllowedHeader>content-type</AllowedHeader><ExposeHeader>ETag</ExposeHeader><MaxAgeSeconds>0</MaxAgeSeconds></CORSRule></CORSConfiguration>";

#[test]
fn cors_xml_preserves_referenced_carriage_returns_and_normalizes_literal_line_endings() -> TestResult
{
    with_fixture(|fixture| {
        fixture.database.execute(
            "UPDATE principals SET policy_generation=randomblob(16), policy_json=?1",
            [ADMIN],
        )?;

        for (encoded, expected, serialized) in [
            (
                "前&#13;中&#13;\n后&amp;&lt;&gt;&quot;&apos;",
                "前\r中\r\n后&<>\"'",
                "前&#13;中&#13;\n后&amp;&lt;&gt;&quot;&apos;",
            ),
            (
                "before\rmiddle\r\nafter",
                "before\nmiddle\nafter",
                "before\nmiddle\nafter",
            ),
            (
                "<![CDATA[before\rmiddle\r\nafter]]>",
                "before\nmiddle\nafter",
                "before\nmiddle\nafter",
            ),
        ] {
            report_case(format_args!(
                "encoded={encoded:?}, expected={expected:?}, serialized={serialized:?}"
            ));

            let configuration = CORS.replace("<ID>browser</ID>", &format!("<ID>{encoded}</ID>"));

            status(
                &fixture.exchange(&request("PUT", "/photos?cors", &configuration, &[]))?,
                200,
            );

            let stored = fixture
                .runtime
                .block_on(fixture.connection.get_bucket_cors(bucket()))?;

            assert_eq!(stored.rules()[0].id.as_deref(), Some(expected));

            let response = fixture.exchange(&request("GET", "/photos?cors", "", &[]))?;
            status(&response, 200);
            assert!(body(&response).contains(&format!("<ID>{serialized}</ID>")));
            assert!(!body(&response).contains('\r'));
        }

        Ok(())
    })
}

#[test]
fn invalid_xml_numeric_references_preserve_the_cors_configuration() -> TestResult {
    with_fixture(|fixture| {
        fixture.database.execute(
            "UPDATE principals SET policy_generation=randomblob(16), policy_json=?1",
            [ADMIN],
        )?;

        status(
            &fixture.exchange(&request("PUT", "/photos?cors", CORS, &[]))?,
            200,
        );

        let original = fixture.exchange(&request("GET", "/photos?cors", "", &[]))?;

        for reference in ["&#+65;", "&#x+41;", "&#;", "&#x;", "&#0;", "&#xD800;"] {
            report_case(format_args!("reference={reference:?}"));

            let configuration = CORS.replace("<ID>browser</ID>", &format!("<ID>{reference}</ID>"));

            let rejected =
                fixture.exchange(&request("PUT", "/photos?cors", &configuration, &[]))?;

            status(&rejected, 400);

            assert!(
                rejected.contains("<Code>MalformedXML</Code>"),
                "{reference}"
            );

            assert_eq!(
                body(&fixture.exchange(&request("GET", "/photos?cors", "", &[]))?),
                body(&original),
                "{reference}"
            );
        }

        Ok(())
    })
}

#[test]
fn cors_max_age_rejects_trailing_numeric_text_without_replacing_rules() -> TestResult {
    with_fixture(|fixture| {
        fixture.database.execute(
            "UPDATE principals SET policy_generation=randomblob(16), policy_json=?1",
            [ADMIN],
        )?;

        status(
            &fixture.exchange(&request("PUT", "/photos?cors", CORS, &[]))?,
            200,
        );

        let original = fixture.exchange(&request("GET", "/photos?cors", "", &[]))?;

        for value in ["1tail", "1.5", "1e3", "-1tail", "2147483648", "-2147483649"] {
            report_case(format_args!("value={value:?}"));

            let configuration =
                CORS.replace("<MaxAgeSeconds>0", &format!("<MaxAgeSeconds>{value}"));

            let rejected =
                fixture.exchange(&request("PUT", "/photos?cors", &configuration, &[]))?;

            status(&rejected, 400);
            assert!(rejected.contains("<Code>MalformedXML</Code>"), "{value}");

            assert_eq!(
                body(&fixture.exchange(&request("GET", "/photos?cors", "", &[]))?),
                body(&original),
                "{value}"
            );
        }

        Ok(())
    })
}

#[test]
fn cors_envelopes_round_trip_and_invalid_later_rules_preserve_the_predecessor() -> TestResult {
    with_fixture(|fixture| {
        fixture.database.execute(
            "UPDATE principals SET policy_generation=randomblob(16), policy_json=?1",
            [ADMIN],
        )?;

        let missing = fixture.exchange(&request("GET", "/photos?cors", "", &[]))?;
        status(&missing, 404);
        assert!(missing.contains("<Code>NoSuchCORSConfiguration</Code>"));

        status(
            &fixture.exchange(&request("PUT", "/photos?cors", CORS, &[]))?,
            200,
        );

        let original = fixture.exchange(&request("GET", "/photos?cors", "", &[]))?;
        status(&original, 200);
        assert!(original.contains("<MaxAgeSeconds>0</MaxAgeSeconds>"));
        assert!(original.contains("<ExposeHeader>ETag</ExposeHeader>"));

        for invalid in [
            format!("{CORS}tail"),
            CORS.replace("</CORSConfiguration>", "<CORSRule><AllowedOrigin>*</AllowedOrigin><AllowedMethod>OPTIONS</AllowedMethod></CORSRule></CORSConfiguration>"),
            CORS.replace("<MaxAgeSeconds>0", "<MaxAgeSeconds>2147483648"),
            CORS.replace("<AllowedHeader>content-type", "<AllowedHeader>x-*-*"),
            CORS.replace("</CORSRule>", "<Unknown>ignored?</Unknown></CORSRule>"),
        ] {
            report_case(format_args!("invalid={invalid:?}"));

            status(&fixture.exchange(&request("PUT", "/photos?cors", &invalid, &[]))?, 400);
            assert_eq!(body(&fixture.exchange(&request("GET", "/photos?cors", "", &[]))?), body(&original));
        }

        status(
            &fixture.exchange(&request(
                "PUT",
                "/photos?cors",
                CORS,
                &[("content-md5", "1B2M2Y8AsgTpgAmY7PhCfg==")],
            ))?,
            400,
        );

        assert_eq!(
            body(&fixture.exchange(&request("GET", "/photos?cors", "", &[]))?),
            body(&original)
        );

        fixture.database.execute("UPDATE principals SET policy_generation=randomblob(16), policy_json=?1", [r#"{"Version":"2012-10-17","Statement":{"Effect":"Allow","Action":"s3:GetObject","Resource":"*"}}"#])?;

        for (method, payload) in [("PUT", CORS), ("GET", ""), ("DELETE", "")] {
            report_case(format_args!("method={method:?}"));

            status(
                &fixture.exchange(&request(method, "/photos?cors", payload, &[]))?,
                403,
            );
        }

        fixture.database.execute(
            "UPDATE principals SET policy_generation=randomblob(16), policy_json=?1",
            [ADMIN],
        )?;

        assert_eq!(
            body(&fixture.exchange(&request("GET", "/photos?cors", "", &[]))?),
            body(&original)
        );

        for seconds in [i32::MIN, -1, i32::MAX] {
            report_case(format_args!("seconds={seconds:?}"));

            let configuration =
                CORS.replace("<MaxAgeSeconds>0", &format!("<MaxAgeSeconds>{seconds}"));

            status(
                &fixture.exchange(&request("PUT", "/photos?cors", &configuration, &[]))?,
                200,
            );

            let output = fixture.exchange(&request("GET", "/photos?cors", "", &[]))?;
            status(&output, 200);
            assert!(output.contains(&format!("<MaxAgeSeconds>{seconds}</MaxAgeSeconds>")));
        }

        status(
            &fixture.exchange(&request("DELETE", "/photos?cors", "", &[]))?,
            204,
        );

        status(
            &fixture.exchange(&request("DELETE", "/photos?cors", "", &[]))?,
            204,
        );

        status(
            &fixture.exchange(&request("GET", "/photos?cors", "", &[]))?,
            404,
        );

        Ok(())
    })
}

#[test]
fn actual_preflight_never_grants_unsigned_storage_access_and_config_changes_are_current()
-> TestResult {
    with_fixture(|fixture| {
        fixture.database.execute(
            "UPDATE principals SET policy_generation=randomblob(16), policy_json=?1",
            [ADMIN],
        )?;

        status(
            &fixture.exchange(&request("PUT", "/photos?cors", CORS, &[]))?,
            200,
        );

        let preflight = |headers: &str| {
            format!("OPTIONS /photos/item HTTP/1.1\r\nHost: localhost\r\nConnection: close\r\nOrigin: https://app.example\r\nAccess-Control-Request-Method: PUT\r\n{headers}\r\n").into_bytes()
        };

        let allowed = fixture.exchange(&preflight(
            "Access-Control-Request-Headers: Content-Type\r\n",
        ))?;

        status(&allowed, 200);
        assert_eq!(body(&allowed), "");

        assert_eq!(
            header(&allowed, "access-control-allow-origin"),
            Some("https://app.example")
        );

        assert_eq!(header(&allowed, "access-control-max-age"), Some("0"));

        for (headers, expected) in [
            ("Access-Control-Request-Headers: Content-Type\r\n", 200),
            ("Access-Control-Request-Headers: x-denied\r\n", 403),
            ("Content-Length: 1\r\n\r\nx", 400),
        ] {
            report_case(format_args!("headers={headers:?}, expected={expected}"));

            let request = String::from_utf8(preflight(headers))?
                .replace("/photos/item ", "/photos/item?X-Amz-Signature=invalid ");

            let response = fixture.exchange(request.as_bytes())?;
            status(&response, expected);
            assert!(header(&response, "x-amz-request-id").is_some());

            assert_eq!(
                header(&response, "access-control-allow-origin"),
                (expected == 200).then_some("https://app.example")
            );
        }

        status(
            &fixture.exchange(&preflight("Access-Control-Request-Headers: x-denied\r\n"))?,
            403,
        );

        let denied = fixture.exchange(b"PUT /photos/item HTTP/1.1\r\nHost: localhost\r\nConnection: close\r\nOrigin: https://app.example\r\nContent-Length: 0\r\n\r\n")?;
        status(&denied, 403);

        assert_eq!(
            header(&denied, "access-control-allow-origin"),
            Some("https://app.example")
        );

        status(
            &fixture.exchange(&request("HEAD", "/photos/item", "", &[]))?,
            404,
        );

        status(
            &fixture.exchange(&request(
                "PUT",
                "/photos/item",
                "complete",
                &[("origin", "https://app.example")],
            ))?,
            200,
        );

        let read = fixture.exchange(&request(
            "GET",
            "/photos/item",
            "",
            &[("origin", "https://app.example")],
        ))?;

        status(&read, 200);

        assert!(matches!(
            body(&read),
            "complete" | "8\r\ncomplete\r\n0\r\n\r\n"
        ));

        assert_eq!(header(&read, "access-control-expose-headers"), Some("ETag"));

        status(
            &fixture.exchange(&request("DELETE", "/photos?cors", "", &[]))?,
            204,
        );

        status(&fixture.exchange(&preflight(""))?, 403);

        let read = fixture.exchange(&request(
            "GET",
            "/photos/item",
            "",
            &[("origin", "https://app.example")],
        ))?;

        status(&read, 200);
        assert!(header(&read, "access-control-allow-origin").is_none());
        Ok(())
    })
}

#[test]
fn cors_preserves_conditional_error_and_corrupt_configuration_response_phases() -> TestResult {
    with_fixture(|fixture| {
        fixture.database.execute(
            "UPDATE principals SET policy_generation=randomblob(16), policy_json=?1",
            [ADMIN],
        )?;

        status(
            &fixture.exchange(&request("PUT", "/photos?cors", CORS, &[]))?,
            200,
        );

        let published = fixture.exchange(&request("PUT", "/photos/item", "original", &[]))?;
        status(&published, 200);
        let etag = header(&published, "etag").expect("published ETag");

        for (method, path, condition, expected) in [
            ("GET", "/photos/item", ("if-none-match", etag), 304),
            ("HEAD", "/photos/item", ("if-none-match", etag), 304),
            ("GET", "/photos/item", ("if-match", "\"other\""), 412),
            ("HEAD", "/photos/missing", ("if-match", "*"), 404),
        ] {
            report_case(format_args!(
                "method={method:?}, path={path:?}, condition={condition:?}, expected={expected:?}"
            ));

            let response = fixture.exchange(&request(
                method,
                path,
                "",
                &[("origin", "https://app.example"), condition],
            ))?;

            status(&response, expected);

            assert_eq!(
                header(&response, "access-control-allow-origin"),
                Some("https://app.example")
            );

            assert_eq!(
                header(&response, "access-control-expose-headers"),
                Some("ETag")
            );

            if method == "HEAD" || expected == 304 {
                assert_eq!(body(&response), "");
            }
        }

        let failed_write = fixture.exchange(&request(
            "PUT",
            "/photos/item",
            "replacement",
            &[("origin", "https://app.example"), ("if-none-match", "*")],
        ))?;

        status(&failed_write, 412);

        assert_eq!(
            header(&failed_write, "access-control-allow-origin"),
            Some("https://app.example")
        );

        assert_eq!(bytes(fixture, "item"), b"original");

        for origin in ["https://denied.example", "invalid origin"] {
            report_case(format_args!("origin={origin:?}"));

            let response =
                fixture.exchange(&request("GET", "/photos/item", "", &[("origin", origin)]))?;

            status(&response, 200);
            assert!(header(&response, "access-control-allow-origin").is_none());
            assert_eq!(header(&response, "vary"), Some("Origin"));
        }

        for (origin, method, requested, expected) in [
            ("https://denied.example", "PUT", "content-type", 403),
            ("https://app.example", "DELETE", "content-type", 403),
            ("https://app.example", "put", "content-type", 403),
            ("https://app.example", "PUT", "content-type, x-denied", 403),
            ("https://app.example", "PUT", "content-type,", 400),
        ] {
            report_case(format_args!(
                "origin={origin:?}, method={method:?}, requested={requested:?}, expected={expected:?}"
            ));

            let response = fixture.exchange(format!("OPTIONS /photos/item HTTP/1.1\r\nHost: localhost\r\nConnection: close\r\nOrigin: {origin}\r\nAccess-Control-Request-Method: {method}\r\nAccess-Control-Request-Headers: {requested}\r\n\r\n").as_bytes())?;
            status(&response, expected);
            assert!(header(&response, "access-control-allow-origin").is_none());

            assert_eq!(
                header(&response, "vary"),
                Some("Origin, Access-Control-Request-Method, Access-Control-Request-Headers")
            );
        }

        let duplicate = fixture.exchange(b"OPTIONS /photos/item HTTP/1.1\r\nHost: localhost\r\nOrigin: https://app.example\r\nOrigin: https://denied.example\r\nAccess-Control-Request-Method: PUT\r\n\r\n")?;
        status(&duplicate, 400);
        assert!(header(&duplicate, "access-control-allow-origin").is_none());
        assert_eq!(header(&duplicate, "connection"), Some("close"));

        fixture.database.execute(
            "UPDATE buckets SET cors_rules_json='{}' WHERE name='photos'",
            [],
        )?;

        let read = fixture.exchange(&request(
            "GET",
            "/photos/item",
            "",
            &[("origin", "https://app.example")],
        ))?;

        status(&read, 200);
        assert!(header(&read, "access-control-allow-origin").is_none());
        assert_eq!(bytes(fixture, "item"), b"original");
        let denied = fixture.exchange(b"GET /photos/item HTTP/1.1\r\nHost: localhost\r\nConnection: close\r\nOrigin: https://app.example\r\n\r\n")?;
        status(&denied, 403);
        assert!(denied.contains("<Code>AccessDenied</Code>"));
        assert!(header(&denied, "access-control-allow-origin").is_none());
        Ok(())
    })
}

fn body(response: &str) -> &str {
    response
        .split_once("\r\n\r\n")
        .expect("response header boundary")
        .1
}

fn header<'a>(response: &'a str, name: &str) -> Option<&'a str> {
    response
        .split_once("\r\n\r\n")
        .expect("response header boundary")
        .0
        .lines()
        .filter_map(|line| line.split_once(':'))
        .find_map(|(candidate, value)| candidate.eq_ignore_ascii_case(name).then(|| value.trim()))
}

#[test]
fn cors_delete_consumes_empty_framing_before_mutation() -> TestResult {
    with_fixture(|fixture| {
        fixture.database.execute(
            "UPDATE principals SET policy_generation=randomblob(16), policy_json=?1",
            [ADMIN],
        )?;

        status(
            &fixture.exchange(&request("PUT", "/photos?cors", CORS, &[]))?,
            200,
        );

        status(
            &fixture.exchange(&request("DELETE", "/photos?cors", "unexpected", &[]))?,
            400,
        );

        status(
            &fixture.exchange(&request("GET", "/photos?cors", "", &[]))?,
            200,
        );

        Ok(())
    })
}
