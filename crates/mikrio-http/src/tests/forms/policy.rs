//! Published signature and bounded policy interpretation through the real codec.

use std::cell::RefCell;
use std::fmt::Write as _;
use std::sync::Arc;

use futures_util::{StreamExt, stream};
use hyper::body::Bytes;
use s3s::auth::SimpleAuth;
use s3s::config::{S3Config, StaticConfigProvider};
use s3s::service::S3ServiceBuilder;

use super::{Fixture, Source, TestResult, body, form, report_case, request, status};
use crate::access::REQUEST_KEY;

struct NoDispatch;

#[async_trait::async_trait]
impl s3s::S3 for NoDispatch {
    async fn post_object(
        &self,
        _: s3s::S3Request<s3s::dto::PostObjectInput>,
    ) -> s3s::S3Result<s3s::S3Response<s3s::dto::PostObjectOutput>> {
        Err(s3s::s3_error!(InternalError))
    }
}

#[test]
fn published_post_signature_authenticates_exact_policy_bytes_before_expiry_rejection() -> TestResult
{
    let vector: serde_json::Value = serde_json::from_str(include_str!(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/../../tests/contract/post-signature.json"
    )))?;

    let field = |name: &str| vector[name].as_str().unwrap();
    let mut builder = S3ServiceBuilder::new(NoDispatch);

    builder.set_auth(SimpleAuth::from_single(
        field("access_key"),
        field("secret_key"),
    ));

    // Only this isolated codec fixture accepts the historical signing date.
    // Expiration remains enforced, and the production service keeps its skew bound.
    let mut config = S3Config::default();
    config.presigned_url_max_skew_time_secs = u32::MAX;

    builder.set_config(Arc::new(StaticConfigProvider::new(Arc::new(config))));
    let service = builder.build();

    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()?;

    runtime.block_on(async {
        for (signature, policy, expected) in [
            (
                field("signature").to_owned(),
                field("policy").to_owned(),
                "AccessDenied",
            ),
            (
                "0".repeat(64),
                field("policy").to_owned(),
                "SignatureDoesNotMatch",
            ),
            // Appending JSON whitespace keeps decoded meaning, but changes the
            // signed base64 bytes. Re-serialization cannot repair this signature.
            (
                field("signature").to_owned(),
                format!("{}IA==", field("policy")),
                "SignatureDoesNotMatch",
            ),
        ] {
            let mut encoded = String::new();

            for (name, value) in [
                ("key", "user/user1/a.jpg"),
                ("policy", policy.as_str()),
                ("x-amz-algorithm", "AWS4-HMAC-SHA256"),
                ("x-amz-credential", field("credential")),
                ("x-amz-date", field("date")),
                ("x-amz-signature", signature.as_str()),
            ] {
                write!(
                    encoded,
                    "--mikrio-independent-form\r\nContent-Disposition: form-data; name=\"{name}\"\r\n\r\n{value}\r\n"
                )?;
            }

            encoded.push_str("--mikrio-independent-form\r\nContent-Disposition: form-data; name=\"file\"; filename=\"a.jpg\"\r\n\r\npayload\r\n--mikrio-independent-form--\r\n");

            let source = Source(Box::pin(stream::iter([Ok(Bytes::from(encoded))])));
            let mut request = request(source, None);
            *request.uri_mut() = "/sigv4examplebucket".parse()?;
            let response = service.call(request).await?;
            assert_eq!(response.status(), http::StatusCode::FORBIDDEN);

            let mut response = response.into_body();
            let mut bytes = Vec::new();

            while let Some(chunk) = response.next().await {
                bytes.extend_from_slice(&chunk.map_err(std::io::Error::other)?);
                assert!(bytes.len() <= 4096);
            }

            let xml = std::str::from_utf8(&bytes)?;

            assert!(
                xml.contains(&format!("<Code>{expected}</Code>")),
                "unexpected code: {xml}"
            );

            if expected == "AccessDenied" {
                assert!(xml.contains("<Message>Request has expired</Message>"));
            }
        }

        Ok::<_, Box<dyn std::error::Error>>(())
    })
}

#[test]
fn filename_expansion_is_single_pass_literal_and_uses_only_the_final_component() -> TestResult {
    Fixture::run(false, |fixture| {
        fixture.runtime.block_on(async {
            let original = String::from_utf8(body())?;
            let header = "name=\"file\"; filename=\"a.txt\"";

            for (filename, key, expected) in [
                ("name=\"file\"", "album/${filename}", "album/"),
                (
                    "name=\"file\"; filename=\"folder/a.txt\"",
                    "album/${filename}",
                    "album/a.txt",
                ),
                (
                    "name=\"file\"; filename=\"C:\\folder\\a.txt\"",
                    "album/${filename}",
                    "album/a.txt",
                ),
                (
                    "name=\"file\"; filename=\"a%2Fb +?#&.txt\"",
                    "album/${filename}",
                    "album/a%2Fb +?#&.txt",
                ),
                (
                    "name=\"file\"; filename=\"雪-e\u{301}.txt\"",
                    "album/${filename}",
                    "album/雪-e\u{301}.txt",
                ),
                (
                    "name=\"file\"; filename=\"..\"",
                    "album/${filename}",
                    "album/..",
                ),
                (
                    "name=\"file\"; filename=\"${filename}\"",
                    "album/${filename}/${filename}",
                    "album/${filename}/${filename}",
                ),
                (
                    "name=\"file\"; filename=\"a.txt\"",
                    "album/${filename}/${filename}",
                    "album/a.txt/a.txt",
                ),
            ] {
                report_case(format_args!("filename={filename}, expected={expected}"));

                let encoded = original
                    .replace(header, filename)
                    .replace("album/${filename}", key);

                let source = Source(Box::pin(stream::iter([Ok(Bytes::from(encoded))])));

                let response = REQUEST_KEY
                    .scope(
                        RefCell::new(None),
                        fixture.service.inner.call(request(source, None)),
                    )
                    .await?;

                assert_eq!(response.status(), http::StatusCode::NO_CONTENT);
                let observed = fixture.observed.lock().unwrap();

                assert_eq!(
                    observed.admitted.last(),
                    Some(&("photos".to_owned(), expected.to_owned()))
                );
            }

            Ok::<_, Box<dyn std::error::Error>>(())
        })
    })
}

#[test]
fn malformed_policy_types_duplicates_depth_and_form_expansion_fail_before_dispatch() -> TestResult {
    Fixture::run(false, |fixture| {
        for mode in [
            "policy-duplicate-member",
            "policy-duplicate-expiration",
            "policy-duplicate-conditions",
            "policy-condition-boolean",
            "policy-range-boolean",
            "policy-range-fraction",
            "policy-range-negative",
            "policy-range-overflow",
            "policy-range-reversed",
            "policy-deep-condition",
            "policy-field-over-limit",
            "form-parts-over-limit",
            "form-expansion-over-limit",
        ] {
            report_case(format_args!("mode={mode}"));
            let response = fixture.exchange(&form(mode))?;
            assert!(response.starts_with("HTTP/1.1 4"), "accepted {mode}");
            let observed = fixture.observed.lock().unwrap();
            assert_eq!(observed.admitted, []);
            assert_eq!(observed.finishes, 0);
        }

        let response = fixture.exchange(&form("valid"))?;
        status(&response, 204);
        assert_eq!(fixture.observed.lock().unwrap().finishes, 1);
        Ok(())
    })
}
