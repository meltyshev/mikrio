//! Shared value validation through signed requests and actual HTTP projection.

use mikrio_core::object::UserMetadata;

use super::*;

fn value_request(mode: &str) -> Vec<u8> {
    let script =
        PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../tests/contract/http_sigv4.py");

    let output = crate::tests::python::command()
        .arg(script)
        .args(["--values", mode])
        .output()
        .unwrap();

    assert!(output.status.success(), "independent value producer failed");
    output.stdout
}

#[test]
fn signed_values_obey_shared_limits_before_body_admission() -> TestResult {
    Fixture::run(false, |fixture| {
        for mode in [
            "metadata-unicode",
            "metadata-empty",
            "metadata-limit",
            "metadata-count-limit",
            "representation-limit",
            "expires-early",
        ] {
            let response = fixture.exchange(&value_request(mode))?;
            assert!(response.starts_with("HTTP/1.1 200 "), "{mode}: {response}");
        }

        for mode in [
            "metadata-over",
            "metadata-count-over",
            "metadata-repeated-over",
            "metadata-control",
            "metadata-empty-key",
            "representation-over",
            "representation-unicode",
            "expires-invalid",
            "expires-outside-range",
        ] {
            report_case(format_args!("mode={mode:?}"));

            let response = fixture.exchange(&value_request(mode))?;
            assert!(response.starts_with("HTTP/1.1 400 "), "{mode}: {response}");

            assert!(
                response.contains("<Code>InvalidArgument</Code>"),
                "{mode}: {response}"
            );

            assert_eq!(fixture.observed.lock().unwrap().admitted.len(), 6);
            assert_eq!(fixture.observed.lock().unwrap().finishes, 6);
        }

        for path in [
            "/photos/../key",
            "/photos/a/../../key",
            "/photos/a//../../key",
        ] {
            report_case(format_args!("path={path:?}"));

            let response = fixture.exchange(&get(path, "header"))?;
            status(&response, 400);
            assert!(response.contains("<Code>InvalidArgument</Code>"));
            assert_eq!(fixture.observed.lock().unwrap().admitted.len(), 6);
        }

        Ok(())
    })
}

struct MetadataProjection(UserMetadata);

#[async_trait::async_trait]
impl s3s::S3 for MetadataProjection {
    async fn put_object(
        &self,
        request: S3Request<PutObjectInput>,
    ) -> S3Result<S3Response<PutObjectOutput>> {
        let metadata = request.input.metadata.as_ref().unwrap();

        let validated = UserMetadata::new(
            metadata
                .iter()
                .map(|(key, value)| (key.clone(), value.clone())),
        )
        .unwrap();

        assert_eq!(validated, self.0);
        let facts = integrity::consume(request).await?;

        Ok(S3Response::new(PutObjectOutput {
            e_tag: Some(facts.etag().parse().unwrap()),
            ..PutObjectOutput::default()
        }))
    }

    async fn get_object(
        &self,
        _request: S3Request<GetObjectInput>,
    ) -> S3Result<S3Response<GetObjectOutput>> {
        Ok(S3Response::new(GetObjectOutput {
            content_length: Some(0),
            metadata: Some(
                self.0
                    .entries()
                    .iter()
                    .map(|(key, value)| (key.clone(), value.clone()))
                    .collect(),
            ),
            body: Some(s3s::Body::empty().into()),
            ..GetObjectOutput::default()
        }))
    }
}

#[test]
fn repeated_metadata_combines_in_signed_order_and_keeps_empty_and_unicode_values() -> TestResult {
    Fixture::run(false, |fixture| {
        for (mode, expected) in [
            ("metadata-repeated", "first,second".into()),
            ("metadata-repeated-empty", ",second".into()),
            ("metadata-repeated-unicode", "café,second".into()),
            (
                "metadata-repeated-limit",
                format!("{},{}", "x".repeat(1022), "y".repeat(1023)),
            ),
        ] {
            report_case(format_args!("mode={mode:?}, expected={expected:?}"));

            fixture.service = Arc::new(Service::with_backend(
                fixture.connection.clone(),
                None,
                MetadataProjection(UserMetadata::new([("id".into(), expected)])?),
            )?);

            let request = value_request(mode);
            status(&fixture.exchange(&request)?, 200);

            if mode == "metadata-repeated" {
                let tampered = String::from_utf8(request)?.replace("second", "changed");
                status(&fixture.exchange(tampered.as_bytes())?, 403);
            }
        }

        Ok(())
    })
}

#[test]
fn stored_unicode_metadata_projects_through_the_upstream_header_codec() -> TestResult {
    Fixture::run(false, |fixture| {
        let metadata = UserMetadata::from_json(r#"{"empty":"","title":"café"}"#)?;

        fixture.service = Arc::new(Service::with_backend(
            fixture.connection.clone(),
            None,
            MetadataProjection(metadata),
        )?);

        let response = fixture.exchange(&get("/photos/key", "header"))?;
        status(&response, 200);
        assert!(response.contains("\r\nx-amz-meta-title: =?UTF-8?B?Y2Fmw6k=?=\r\n"));
        assert!(response.contains("\r\nx-amz-meta-empty: \r\n"));
        assert!(!response.contains("x-amz-missing-meta"));
        Ok(())
    })
}
