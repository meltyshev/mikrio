//! Real signed CORS combinations with SQL fixture setup confined to closed stores.

use super::*;

const ORIGIN: &str = "https://app.example";
const RULE: &str = concat!(
    "<CORSRule><AllowedOrigin>https://app.example</AllowedOrigin>",
    "<AllowedMethod>GET</AllowedMethod><AllowedMethod>HEAD</AllowedMethod>",
    "<AllowedMethod>POST</AllowedMethod><AllowedHeader>range</AllowedHeader>",
    "<AllowedHeader>x-amz-checksum-crc32</AllowedHeader>",
    "<ExposeHeader>ETag</ExposeHeader></CORSRule>"
);

struct CorsFixture<'a> {
    cleanup: &'a cleanup::Scope,
    service: Service,
    runtime: Arc<tokio::runtime::Runtime>,
    _slot: tokio::sync::SemaphorePermit<'static>,
}

impl<'a> CorsFixture<'a> {
    fn run(scenario: impl FnOnce(&CorsFixture<'_>) -> TestResult) -> TestResult {
        cleanup::Scope::run(TestDirectory::new()?.retain_on_failure(), |cleanup| {
            let fixture = CorsFixture::new(cleanup)?;
            scenario(&fixture)
        })
    }

    fn new(cleanup: &'a cleanup::Scope) -> Result<Self, Box<dyn std::error::Error>> {
        let runtime = Arc::clone(&cleanup.runtime);
        let directory = &cleanup.directory;
        let slot = runtime.block_on(FIXTURES.acquire())?;
        let path = directory.path().join("store");
        let connection = cleanup.track(runtime.block_on(Connection::open(path.clone(), None))?);
        runtime.block_on(connection.create_bucket(BucketName::parse("photos")?))?;
        runtime.block_on(connection.close())?;
        let database = rusqlite::Connection::open(path.join("metadata.sqlite3"))?;

        database.execute(
            "INSERT INTO principals(principal_id,name,is_enabled,created_at_ms,policy_generation,policy_json)
             VALUES(zeroblob(16),'app',1,0,randomblob(16),?1)",
            [r#"{"Version":"2012-10-17","Statement":{"Effect":"Allow","Action":"s3:*","Resource":"*"}}"#],
        )?;

        database.execute(
            "INSERT INTO access_keys(access_key_id,principal_id,secret_access_key,created_at_ms)
             VALUES(?1,zeroblob(16),?2,0)",
            [APP_KEY, SECRET],
        )?;

        database.close().map_err(|(_, error)| error)?;
        let connection = cleanup.track(runtime.block_on(Connection::open(path, None))?);
        let service = Service::new(connection.clone(), None)?;

        Ok(Self {
            cleanup,
            service,
            runtime,
            _slot: slot,
        })
    }

    fn exchange(&self, request: &[u8]) -> Result<String, Box<dyn std::error::Error>> {
        let (response, outcome) = super::super::exchange(&self.runtime, &self.service, request)?;
        outcome?;
        Ok(response)
    }
}

fn header<'a>(response: &'a str, name: &str) -> Option<&'a str> {
    response
        .split_once("\r\n\r\n")
        .unwrap()
        .0
        .lines()
        .filter_map(|line| line.split_once(':'))
        .find_map(|(candidate, value)| candidate.eq_ignore_ascii_case(name).then(|| value.trim()))
}

fn body(response: &str) -> &str {
    response.split_once("\r\n\r\n").unwrap().1
}

fn allowed(response: &str, expose: &str) {
    assert_eq!(
        header(response, "access-control-allow-origin"),
        Some(ORIGIN)
    );

    assert_eq!(
        header(response, "access-control-allow-credentials"),
        Some("true")
    );

    assert_eq!(
        header(response, "access-control-expose-headers"),
        Some(expose)
    );

    assert_eq!(header(response, "vary"), Some("Origin"));
}

#[test]
fn cors_covers_listing_range_override_and_bodyless_condition_responses_with_current_rules()
-> TestResult {
    CorsFixture::run(|fixture| {
        let second = RULE.replace(
            "<ExposeHeader>ETag</ExposeHeader>",
            "<ExposeHeader>Content-Range</ExposeHeader>",
        );

        let configuration = format!("<CORSConfiguration>{RULE}{second}</CORSConfiguration>");

        status(
            &fixture.exchange(&request("PUT", "/photos?cors", &configuration, &[]))?,
            200,
        );

        status(
            &fixture.exchange(&request("PUT", "/photos/object", "123456789", &[]))?,
            200,
        );

        let listing = fixture.exchange(&request(
            "GET",
            "/photos?list-type=2&prefix=obj",
            "",
            &[("origin", ORIGIN)],
        ))?;

        status(&listing, 200);
        assert!(body(&listing).contains("<Key>object</Key>"));
        allowed(&listing, "ETag");

        for method in ["GET", "HEAD"] {
            report_case(format_args!("method={method:?}"));

            let response = fixture.exchange(&request(
                method,
                "/photos/object?response-content-type=text%2Fplain",
                "",
                &[
                    ("origin", ORIGIN),
                    ("range", "bytes=2-5"),
                    ("x-amz-checksum-mode", "ENABLED"),
                ],
            ))?;

            status(&response, if method == "GET" { 206 } else { 200 });
            allowed(&response, "ETag");
            assert_eq!(header(&response, "content-range"), Some("bytes 2-5/9"));
            assert_eq!(header(&response, "content-length"), Some("4"));
            assert_eq!(header(&response, "content-type"), Some("text/plain"));
            assert!(header(&response, "x-amz-checksum-crc64nvme").is_none());
            assert_eq!(body(&response), if method == "GET" { "3456" } else { "" });

            for (condition, value, expected) in [
                ("if-none-match", "*", 304),
                ("if-match", "\"wrong\"", 412),
                ("range", "bytes=40-50", 416),
            ] {
                report_case(format_args!(
                    "method={method:?}, condition={condition:?}, value={value:?}, expected={expected:?}"
                ));

                let response = fixture.exchange(&request(
                    method,
                    "/photos/object",
                    "",
                    &[("origin", ORIGIN), (condition, value)],
                ))?;

                status(&response, expected);
                allowed(&response, "ETag");

                if method == "HEAD" || expected == 304 {
                    assert_eq!(body(&response), "");
                }
            }
        }

        let peer = fixture
            .cleanup
            .track(fixture.runtime.block_on(Connection::open(
                fixture.cleanup.directory.path().join("store"),
                None,
            ))?);

        let service = Service::new(peer.clone(), None)?;
        let configuration = format!("<CORSConfiguration>{second}</CORSConfiguration>");

        let (response, outcome) = super::super::exchange(
            &fixture.runtime,
            &service,
            &request("PUT", "/photos?cors", &configuration, &[]),
        )?;

        outcome?;
        status(&response, 200);
        fixture.runtime.block_on(peer.close())?;

        let response = fixture.exchange(&request(
            "GET",
            "/photos/object",
            "",
            &[("origin", ORIGIN), ("range", "bytes=0-0")],
        ))?;

        status(&response, 206);
        allowed(&response, "Content-Range");

        for origin in ["https://denied.example", "invalid origin"] {
            report_case(format_args!("origin={origin:?}"));

            let response = fixture.exchange(&request(
                "GET",
                "/photos?list-type=2",
                "",
                &[("origin", origin)],
            ))?;

            status(&response, 200);
            assert!(header(&response, "access-control-allow-origin").is_none());
            assert_eq!(header(&response, "vary"), Some("Origin"));
        }

        let raw = format!(
            concat!(
                "GET /photos/object HTTP/1.1\r\nHost: localhost\r\nConnection: close\r\n",
                "Origin: {}\r\n\r\n"
            ),
            ORIGIN
        );

        let response = fixture.exchange(raw.as_bytes())?;
        status(&response, 403);
        allowed(&response, "Content-Range");
        Ok(())
    })
}

#[test]
fn cors_preserves_batch_item_errors_envelope_failures_and_preflight_boundaries() -> TestResult {
    CorsFixture::run(|fixture| {
        let configuration = format!("<CORSConfiguration>{RULE}</CORSConfiguration>");

        status(
            &fixture.exchange(&request("PUT", "/photos?cors", &configuration, &[]))?,
            200,
        );

        status(
            &fixture.exchange(&request("PUT", "/photos/object", "before", &[]))?,
            200,
        );

        let payload = concat!(
            "<Delete><Object><Key>object</Key><ETag>wrong</ETag></Object>",
            "<Object><Key>missing</Key></Object></Delete>"
        );

        for (digest, expected) in [("mf/cTQ==", 200), ("AAAAAA==", 400)] {
            report_case(format_args!("digest={digest:?}, expected={expected:?}"));

            let response = fixture.exchange(&request(
                "POST",
                "/photos?delete",
                payload,
                &[("origin", ORIGIN), ("x-amz-checksum-crc32", digest)],
            ))?;

            status(&response, expected);
            allowed(&response, "ETag");

            if expected == 200 {
                assert_eq!(body(&response).matches("<Deleted>").count(), 1);
                assert_eq!(body(&response).matches("<Error>").count(), 1);
                assert!(body(&response).contains("<Code>PreconditionFailed</Code>"));
            } else {
                assert!(body(&response).contains("<Code>BadDigest</Code>"));
            }
        }

        for (requested, expected) in [
            ("X-Amz-Checksum-Crc32", 200),
            ("x-amz-checksum-crc32, x-denied", 403),
        ] {
            report_case(format_args!(
                "requested={requested:?}, expected={expected:?}"
            ));

            let raw = format!(
                concat!(
                    "OPTIONS /photos?delete HTTP/1.1\r\nHost: localhost\r\nConnection: close\r\n",
                    "Origin: {}\r\nAccess-Control-Request-Method: POST\r\n",
                    "Access-Control-Request-Headers: {}\r\n\r\n"
                ),
                ORIGIN, requested
            );

            let response = fixture.exchange(raw.as_bytes())?;
            status(&response, expected);

            assert_eq!(
                header(&response, "access-control-allow-origin"),
                if expected == 200 { Some(ORIGIN) } else { None }
            );

            if expected == 200 {
                assert_eq!(body(&response), "");

                assert_eq!(
                    header(&response, "access-control-allow-methods"),
                    Some("POST")
                );
            }
        }

        let remaining = fixture.exchange(&request("GET", "/photos/object", "", &[]))?;
        status(&remaining, 200);
        assert_eq!(body(&remaining), "before");

        status(
            &fixture.exchange(&request("DELETE", "/photos?cors", "", &[]))?,
            204,
        );

        let response = fixture.exchange(&request(
            "POST",
            "/photos?delete",
            payload,
            &[("origin", ORIGIN), ("x-amz-checksum-crc32", "mf/cTQ==")],
        ))?;

        status(&response, 200);
        assert!(header(&response, "access-control-allow-origin").is_none());
        Ok(())
    })
}

#[test]
fn cors_covers_lifecycle_copy_multipart_attributes_part_reads_and_post_errors() -> TestResult {
    CorsFixture::run(|fixture| {
        let rule = RULE.replace(
            "<AllowedMethod>POST</AllowedMethod>",
            "<AllowedMethod>POST</AllowedMethod><AllowedMethod>PUT</AllowedMethod><AllowedMethod>DELETE</AllowedMethod>",
        );

        let configuration = format!("<CORSConfiguration>{rule}</CORSConfiguration>");

        status(
            &fixture.exchange(&request("PUT", "/photos?cors", &configuration, &[]))?,
            200,
        );

        let lifecycle = "<LifecycleConfiguration><Rule><Filter/><Status>Disabled</Status><Expiration><Days>1</Days></Expiration></Rule></LifecycleConfiguration>";

        for (method, payload, expected) in [
            ("GET", "", 404),
            ("PUT", lifecycle, 200),
            ("GET", "", 200),
            ("PUT", "<LifecycleConfiguration/>", 400),
            ("DELETE", "", 204),
        ] {
            report_case(format_args!("method={method:?}, expected={expected:?}"));

            let response = fixture.exchange(&request(
                method,
                "/photos?lifecycle",
                payload,
                &[("origin", ORIGIN)],
            ))?;

            status(&response, expected);
            allowed(&response, "ETag");
        }

        status(
            &fixture.exchange(&request("PUT", "/photos/source", "abc", &[]))?,
            200,
        );

        for (source, expected) in [("/photos/source", 200), ("/photos/missing", 404)] {
            report_case(format_args!("source={source:?}, expected={expected:?}"));

            let response = fixture.exchange(&request(
                "PUT",
                "/photos/copied",
                "",
                &[("origin", ORIGIN), ("x-amz-copy-source", source)],
            ))?;

            status(&response, expected);
            allowed(&response, "ETag");
        }

        let created = fixture.exchange(&request(
            "POST",
            "/photos/multipart?uploads",
            "",
            &[("origin", ORIGIN)],
        ))?;

        status(&created, 200);
        allowed(&created, "ETag");

        let upload_id = body(&created)
            .split_once("<UploadId>")
            .unwrap()
            .1
            .split_once("</UploadId>")
            .unwrap()
            .0;

        let path = format!("/photos/multipart?uploadId={upload_id}");

        let copied = fixture.exchange(&request(
            "PUT",
            &format!("{path}&partNumber=1"),
            "",
            &[("origin", ORIGIN), ("x-amz-copy-source", "/photos/source")],
        ))?;

        status(&copied, 200);
        allowed(&copied, "ETag");
        assert!(body(&copied).contains("900150983cd24fb0d6963f7d28e17f72"));

        let selection = "<CompleteMultipartUpload><Part><PartNumber>1</PartNumber><ETag>\"900150983cd24fb0d6963f7d28e17f72\"</ETag></Part></CompleteMultipartUpload>";

        let completed =
            fixture.exchange(&request("POST", &path, selection, &[("origin", ORIGIN)]))?;

        status(&completed, 200);
        allowed(&completed, "ETag");

        let part = fixture.exchange(&request(
            "GET",
            "/photos/multipart?partNumber=1",
            "",
            &[("origin", ORIGIN)],
        ))?;

        status(&part, 206);
        allowed(&part, "ETag");
        assert_eq!(body(&part), "abc");

        for (attributes, expected) in [("ETag,ObjectSize,ObjectParts", 200), ("Unknown", 400)] {
            report_case(format_args!(
                "attributes={attributes:?}, expected={expected:?}"
            ));

            let response = fixture.exchange(&request(
                "GET",
                "/photos/multipart?attributes",
                "",
                &[("origin", ORIGIN), ("x-amz-object-attributes", attributes)],
            ))?;

            status(&response, expected);
            allowed(&response, "ETag");
        }

        for (mode, expected) in [("valid", 204), ("extra-file", 400)] {
            report_case(format_args!("mode={mode:?}, expected={expected:?}"));

            let raw = String::from_utf8(form(mode))?.replacen(
                "\r\n\r\n",
                &format!("\r\nOrigin: {ORIGIN}\r\n\r\n"),
                1,
            );

            let response = fixture.exchange(raw.as_bytes())?;
            status(&response, expected);
            allowed(&response, "ETag");
        }

        Ok(())
    })
}
