use super::*;

#[test]
fn bodyless_reads_verify_supplied_digests_before_returning_stored_state() -> TestResult {
    with_fixture(|fixture| {
        fixture.database.execute(
            "UPDATE principals SET policy_generation=randomblob(16), policy_json=?1",
            [ADMIN],
        )?;

        for (path, payload) in [
            ("/photos/object", "retained bytes"),
            ("/photos?policy", ORIGINAL),
            ("/photos?publicAccessBlock", BLOCK),
            ("/photos?ownershipControls", ENFORCED),
            (
                "/photos?cors",
                "<CORSConfiguration><CORSRule><AllowedOrigin>https://app.example</AllowedOrigin><AllowedMethod>GET</AllowedMethod></CORSRule></CORSConfiguration>",
            ),
            (
                "/photos?lifecycle",
                "<LifecycleConfiguration><Rule><Filter/><Status>Disabled</Status><Expiration><Days>1</Days></Expiration></Rule></LifecycleConfiguration>",
            ),
        ] {
            report_case(format_args!("path={path:?}"));

            status(
                &fixture.exchange(&request("PUT", path, payload, &[]))?,
                if path.ends_with("?policy") { 204 } else { 200 },
            );
        }

        let initiated = fixture.exchange(&request("POST", "/photos/upload?uploads", "", &[]))?;
        status(&initiated, 200);

        let upload_id = initiated
            .split_once("<UploadId>")
            .and_then(|(_, value)| value.split_once("</UploadId>"))
            .ok_or("missing upload identity")?
            .0;

        let parts = format!("/photos/upload?uploadId={upload_id}");

        for (method, path, selected) in [
            ("GET", "/", false),
            ("HEAD", "/photos", false),
            ("GET", "/photos?location", false),
            ("GET", "/photos?cors", false),
            ("GET", "/photos?lifecycle", false),
            ("GET", "/photos?policy", false),
            ("GET", "/photos?policyStatus", false),
            ("GET", "/photos?publicAccessBlock", false),
            ("GET", "/photos?ownershipControls", false),
            ("GET", "/photos?acl", false),
            ("GET", "/photos/object?acl", false),
            ("GET", "/photos/object", false),
            ("HEAD", "/photos/object", false),
            ("GET", "/photos?list-type=2", false),
            ("GET", "/photos/object?tagging", false),
            ("GET", "/photos/object?attributes", true),
            ("GET", parts.as_str(), false),
            ("GET", "/photos?uploads", false),
        ] {
            for (name, value, expected, code) in [
                ("content-md5", "1B2M2Y8AsgTpgAmY7PhCfg==", 200, ""),
                ("x-amz-checksum-crc32", "AAAAAA==", 200, ""),
                ("content-md5", "kAFQmDzST7DWlj99KOF/cg==", 400, "BadDigest"),
                ("x-amz-checksum-crc32", "NSRBwg==", 400, "BadDigest"),
                ("content-md5", "", 400, "InvalidDigest"),
                ("x-amz-checksum-type", "COMPOSITE", 501, "NotImplemented"),
                ("x-amz-checksum-crc16", "AAAA", 501, "NotImplemented"),
                (
                    "x-amz-sdk-checksum-algorithm",
                    "CRC32",
                    400,
                    "InvalidRequest",
                ),
            ] {
                report_case(format_args!(
                    "method={method:?}, path={path:?}, selected={selected:?}, name={name:?}, value={value:?}, expected={expected:?}, code={code:?}"
                ));

                let mut headers = vec![(name, value)];

                if selected {
                    headers.push(("x-amz-object-attributes", "ObjectSize"));
                }

                let response = fixture.exchange(&request(method, path, "", &headers))?;
                let status_line = response.lines().next().ok_or("missing HTTP status")?;

                assert!(
                    status_line.contains(&format!(" {expected} ")),
                    "{method} {path} {name}={value:?}: {response}"
                );

                if method == "HEAD" {
                    assert_eq!(response.split_once("\r\n\r\n").unwrap().1, "");
                } else if expected != 200 {
                    assert!(response.contains(&format!("<Code>{code}</Code>")));
                }
            }
        }

        assert_eq!(bytes(fixture, "object"), b"retained bytes");
        Ok(())
    })
}

#[test]
fn bodyless_configuration_read_waits_for_termination_and_rejects_a_late_body() -> TestResult {
    with_fixture(|fixture| {
        fixture.database.execute(
            "UPDATE principals SET policy_generation=randomblob(16), policy_json=?1",
            [ADMIN],
        )?;

        for (method, target) in [
            ("GET", "/photos?policy"),
            ("HEAD", "/photos"),
            ("GET", "/photos/object"),
        ] {
            report_case(format_args!("method={method:?}, target={target:?}"));

            fixture.runtime.block_on(async {
                let (entered, waiting) = tokio::sync::oneshot::channel();

                let (release, terminal) =
                    tokio::sync::oneshot::channel::<Option<Result<Bytes, s3s::StdError>>>();

                let source = Source(Box::pin(
                    stream::once(async move {
                        entered.send(()).unwrap();
                        terminal.await.unwrap()
                    })
                    .filter_map(future::ready),
                ));

                let raw = request_with_mode(method, target, "", &[], "query");
                let source: DynByteStream = Box::pin(source);
                let request = envelope(&raw, source.into())?;

                let mut response = Box::pin(
                    REQUEST_KEY.scope(RefCell::new(None), fixture.service.inner.call(request)),
                );

                let observed = tokio::time::timeout(
                    Duration::from_secs(10),
                    future::select(response.as_mut(), waiting),
                )
                .await?;

                assert!(
                    matches!(observed, future::Either::Right((Ok(()), _))),
                    "{method} {target}: read returned before requesting terminal framing"
                );

                assert!(response.as_mut().now_or_never().is_none());

                release
                    .send(Some(Ok(Bytes::from_static(b"unexpected"))))
                    .unwrap();

                let response = tokio::time::timeout(Duration::from_secs(10), response).await??;
                assert_eq!(response.status(), http::StatusCode::BAD_REQUEST);
                Ok::<_, Box<dyn std::error::Error>>(())
            })?;
        }

        Ok(())
    })
}
