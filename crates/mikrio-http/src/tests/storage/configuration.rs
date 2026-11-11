//! Ordered terminal-body failures against real configuration transactions.

use std::pin::Pin;
use std::task::{Context, Poll};

use futures_util::{FutureExt, Stream, future, stream};
use hyper::body::Bytes;
use mikrio_core::PublicAccessBlock;
use mikrio_core::access::BucketAccessRequest;
use s3s::stream::{ByteStream, DynByteStream};

use super::*;

mod cors;
mod integrity;
mod lifecycle;
mod principals;
mod transport;

const POLICY: &str = r#"{"Version":"2012-10-17","Statement":{"Effect":"Deny","Principal":"*","Action":"s3:GetObject","Resource":"*"}}"#;
const ORIGINAL: &str = r#"{"Version":"2012-10-17","Statement":{"Effect":"Deny","Principal":"*","Action":"s3:DeleteObject","Resource":"*"}}"#;
const BLOCK: &str = "<PublicAccessBlockConfiguration><BlockPublicPolicy>false</BlockPublicPolicy></PublicAccessBlockConfiguration>";
const ENFORCED: &str = "<OwnershipControls><Rule><ObjectOwnership>BucketOwnerEnforced</ObjectOwnership></Rule></OwnershipControls>";
const ADMIN: &str =
    r#"{"Version":"2012-10-17","Statement":{"Effect":"Allow","Action":"s3:*","Resource":"*"}}"#;

struct Source(Pin<Box<dyn Stream<Item = Result<Bytes, s3s::StdError>> + Send + Sync>>);

impl Stream for Source {
    type Item = Result<Bytes, s3s::StdError>;

    fn poll_next(mut self: Pin<&mut Self>, context: &mut Context<'_>) -> Poll<Option<Self::Item>> {
        self.0.as_mut().poll_next(context)
    }
}

impl ByteStream for Source {}

fn envelope(raw: &[u8], body: s3s::Body) -> Result<s3s::HttpRequest, Box<dyn std::error::Error>> {
    let text = std::str::from_utf8(raw)?;

    let (headers, _) = text
        .split_once("\r\n\r\n")
        .ok_or("missing header boundary")?;

    let mut lines = headers.split("\r\n");

    let mut start = lines
        .next()
        .ok_or("missing request line")?
        .split_whitespace();

    let mut request = http::Request::builder()
        .method(start.next().ok_or("missing method")?)
        .uri(start.next().ok_or("missing target")?);

    for line in lines {
        let (name, value) = line.split_once(':').ok_or("invalid fixture header")?;
        request = request.header(name, value.trim());
    }

    Ok(request.body(body)?)
}

fn bucket() -> BucketAccessRequest {
    BucketAccessRequest {
        bucket: BucketName::parse("photos").unwrap(),
        expected_owner: None,
    }
}

#[test]
fn xml_integer_scalars_require_complete_in_range_decimal_values() {
    use s3s::xml::{DeError, Deserializer};

    for (value, expected) in [
        ("-2147483648", Some(i32::MIN)),
        ("+2147483647", Some(i32::MAX)),
        ("0001", Some(1)),
        ("1tail", None),
        ("1.5", None),
        ("1e3", None),
        ("2147483648", None),
        ("-2147483649", None),
    ] {
        let xml = format!("<Value>{value}</Value>");
        let mut decoder = Deserializer::new_strict(xml.as_bytes());
        let actual = decoder.named_element("Value", Deserializer::content::<i32>);

        match expected {
            Some(expected) => assert_eq!(actual.unwrap(), expected, "i32 {value}"),
            None => assert!(
                matches!(actual, Err(DeError::InvalidContent)),
                "i32 {value}: {actual:?}"
            ),
        }
    }

    for (value, expected) in [
        ("-9223372036854775808", Some(i64::MIN)),
        ("+9223372036854775807", Some(i64::MAX)),
        ("0001", Some(1)),
        ("1tail", None),
        ("1.5", None),
        ("1e3", None),
        ("9223372036854775808", None),
        ("-9223372036854775809", None),
    ] {
        let xml = format!("<Value>{value}</Value>");
        let mut decoder = Deserializer::new_strict(xml.as_bytes());
        let actual = decoder.named_element("Value", Deserializer::content::<i64>);

        match expected {
            Some(expected) => assert_eq!(actual.unwrap(), expected, "i64 {value}"),
            None => assert!(
                matches!(actual, Err(DeError::InvalidContent)),
                "i64 {value}: {actual:?}"
            ),
        }
    }
}

#[test]
fn buffered_and_streamed_envelopes_obey_the_same_size_and_length_limits() -> TestResult {
    with_fixture(|fixture| {
        fixture.database.execute(
            "UPDATE principals SET policy_generation=randomblob(16), policy_json=?1",
            [ADMIN],
        )?;

        fixture.runtime.block_on(async {
            for streamed in [false, true] {
                for case in ["oversized", "empty-short", "nonempty-short"] {
                    report_case(format_args!("streamed={streamed:?}, case={case:?}"));

                    let body = match case {
                        "oversized" => " ".repeat(64 * 1024 + 1),
                        "empty-short" => String::new(),
                        _ => POLICY.to_owned(),
                    };

                    let raw = request("PUT", "/photos?policy", &body, &[]);
                    let raw = String::from_utf8(raw)?;

                    // Content-Length is deliberately not a signed field in this
                    // independent producer; the payload signature remains valid.
                    let raw = if case == "oversized" {
                        raw
                    } else {
                        raw.replace(
                            &format!("content-length: {}\r\n", body.len()),
                            &format!("content-length: {}\r\n", body.len() + 1),
                        )
                    };

                    let bytes = Bytes::from(body);

                    let body = if streamed {
                        let stream: DynByteStream =
                            Box::pin(Source(Box::pin(stream::iter([Ok(bytes)]))));

                        stream.into()
                    } else {
                        bytes.into()
                    };

                    let request = envelope(raw.as_bytes(), body)?;

                    let mut response = REQUEST_KEY
                        .scope(RefCell::new(None), fixture.service.inner.call(request))
                        .await?;

                    assert_eq!(
                        response.status(),
                        http::StatusCode::BAD_REQUEST,
                        "{case} streamed={streamed}"
                    );

                    let code = if case == "oversized" {
                        "MaxMessageLengthExceeded"
                    } else {
                        "IncompleteBody"
                    };

                    let bytes = response.body_mut().store_all_limited(4096).await.unwrap();

                    assert!(
                        std::str::from_utf8(&bytes)?.contains(&format!("<Code>{code}</Code>")),
                        "{case} streamed={streamed}"
                    );

                    assert_eq!(
                        fixture
                            .connection
                            .get_bucket_policy(bucket())
                            .await
                            .unwrap_err()
                            .failure(),
                        mikrio_core::OperationFailure::NoSuchBucketPolicy
                    );
                }
            }

            Ok::<_, Box<dyn std::error::Error>>(())
        })?;

        Ok(())
    })
}

#[test]
fn valid_prefix_waits_for_terminal_validation_before_configuration_mutation() -> TestResult {
    with_fixture(|fixture| {
        fixture.database.execute(
            "UPDATE principals SET policy_generation=randomblob(16), policy_json=?1",
            [ADMIN],
        )?;

        fixture.runtime.block_on(async {
            for (method, target, prefix) in [
                ("PUT", "/photos?policy", POLICY),
                ("PUT", "/photos?publicAccessBlock", BLOCK),
                ("PUT", "/photos?ownershipControls", ENFORCED),
                ("DELETE", "/photos?policy", ""),
                ("DELETE", "/photos?publicAccessBlock", ""),
            ] {
                for terminal in ["valid", "malformed", "truncated", "transport", "canceled"] {
                    report_case(format_args!("method={method:?}, target={target:?}, prefix={prefix:?}, terminal={terminal:?}"));

                    if terminal == "truncated" && method == "DELETE" {
                        continue;
                    }

                    fixture
                        .connection
                        .put_bucket_policy(bucket(), ORIGINAL.into())
                        .await?;

                    fixture
                        .connection
                        .put_public_access_block(
                            bucket(),
                            PublicAccessBlock {
                                block_public_acls: true,
                                ignore_public_acls: true,
                                block_public_policy: true,
                                restrict_public_buckets: true,
                            },
                        )
                        .await?;

                    let original = fixture.connection.get_public_access_block(bucket()).await?;
                    let expected_tail = if method == "PUT" { "\n" } else { "" };
                    let raw = request(method, target, &format!("{prefix}{expected_tail}"), &[]);
                    let (entered, waiting) = tokio::sync::oneshot::channel();

                    let (release, released) =
                        tokio::sync::oneshot::channel::<Option<Result<Bytes, s3s::StdError>>>();

                    let tail = stream::once(async move {
                        entered.send(()).unwrap();
                        released.await.unwrap()
                    })
                    .filter_map(future::ready);

                    let source = stream::iter([Ok(Bytes::from_static(prefix.as_bytes()))]).chain(tail);
                    let source: DynByteStream = Box::pin(Source(Box::pin(source)));
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
                        "{method} {target} {terminal}: terminal source was not demanded"
                    );

                    assert!(response.as_mut().now_or_never().is_none());

                    assert_eq!(
                        fixture
                            .connection
                            .get_bucket_policy(bucket())
                            .await?
                            .document,
                        ORIGINAL
                    );

                    assert_eq!(
                        fixture.connection.get_public_access_block(bucket()).await?,
                        original
                    );

                    if terminal == "canceled" {
                        drop(response);
                        assert!(release.is_closed(), "canceled request retained its source");

                        assert_eq!(
                            fixture
                                .connection
                                .get_bucket_policy(bucket())
                                .await?
                                .document,
                            ORIGINAL
                        );

                        assert_eq!(
                            fixture.connection.get_public_access_block(bucket()).await?,
                            original
                        );

                        continue;
                    }

                    let tail: Option<Result<Bytes, s3s::StdError>> = match terminal {
                        "valid" => Some(Ok(Bytes::from_static(expected_tail.as_bytes()))),
                        "malformed" => Some(Ok(Bytes::from_static(b"!"))),
                        "truncated" => None,
                        "transport" => Some(Err(Box::new(std::io::Error::new(
                            std::io::ErrorKind::UnexpectedEof,
                            "owned terminal failure",
                        )))),
                        _ => return Err("invalid fixture terminal mode".into()),
                    };

                    release.send(tail).unwrap();

                    let mut response =
                        tokio::time::timeout(Duration::from_secs(10), response).await??;

                    if terminal == "valid" {
                        assert!(response.status().is_success(), "{method} {target}");

                        if method == "PUT" && target.ends_with("policy") {
                            assert_eq!(
                                fixture
                                    .connection
                                    .get_bucket_policy(bucket())
                                    .await?
                                    .document,
                                format!("{POLICY}\n")
                            );
                        }

                        if method == "DELETE" && target.ends_with("policy") {
                            assert_eq!(
                                fixture
                                    .connection
                                    .get_bucket_policy(bucket())
                                    .await
                                    .unwrap_err()
                                    .failure(),
                                mikrio_core::OperationFailure::NoSuchBucketPolicy
                            );
                        }

                        if target.ends_with("publicAccessBlock") {
                            let current = fixture.connection.get_public_access_block(bucket()).await;

                            if method == "DELETE" {
                                assert_eq!(
                                    current.unwrap_err().failure(),
                                    mikrio_core::OperationFailure::NoSuchPublicAccessBlockConfiguration
                                );
                            } else {
                                assert!(!current?.block_public_policy);
                            }
                        }

                        continue;
                    }

                    let (status, code) = match (method, terminal) {
                        ("DELETE", _) => (http::StatusCode::BAD_REQUEST, "InvalidRequest"),
                        (_, "transport") => (http::StatusCode::INTERNAL_SERVER_ERROR, "InternalError"),
                        (_, "truncated") => (http::StatusCode::BAD_REQUEST, "IncompleteBody"),
                        _ => (http::StatusCode::BAD_REQUEST, "BadDigest"),
                    };

                    assert_eq!(response.status(), status, "{method} {target} {terminal}");
                    let bytes = response.body_mut().store_all_limited(4096).await.unwrap();
                    assert!(std::str::from_utf8(&bytes)?.contains(&format!("<Code>{code}</Code>")));

                    assert_eq!(
                        fixture
                            .connection
                            .get_bucket_policy(bucket())
                            .await?
                            .document,
                        ORIGINAL
                    );

                    assert_eq!(
                        fixture.connection.get_public_access_block(bucket()).await?,
                        original
                    );
                }
            }

            Ok::<_, Box<dyn std::error::Error>>(())
        })?;

        Ok(())
    })
}
