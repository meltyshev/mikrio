//! Body-stream probes through the authenticated service and observation sink.

use std::cell::RefCell;
use std::pin::Pin;
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::task::{Context, Poll};

use futures_util::{FutureExt, Stream, StreamExt, stream};
use hyper::body::Bytes;
use s3s::stream::{ByteStream, DynByteStream};

use super::{Fixture, Observations, Service, TestResult, form, report_case, status};
use crate::access::REQUEST_KEY;

mod boundary;
mod policy;
mod projection;

#[test]
fn post_semantic_fields_and_preconditions_cannot_be_silently_ignored() -> TestResult {
    Fixture::run(false, |fixture| {
        for mode in ["field-toolkit", "field-ignored", "field-storage-class"] {
            report_case(format_args!("mode={mode:?}"));

            status(&fixture.exchange(&form(mode))?, 204);
        }

        for (mode, expected, code) in [
            ("field-unknown-amz", 501, "NotImplemented"),
            ("field-encryption", 501, "NotImplemented"),
            ("field-owner", 501, "NotImplemented"),
            ("field-content-language", 501, "NotImplemented"),
            ("field-empty-metadata-name", 400, "InvalidArgument"),
            ("field-if-match", 400, "InvalidArgument"),
            ("field-if-none-match", 400, "InvalidArgument"),
            ("field-content-length", 400, "InvalidArgument"),
            ("field-outer-if-match", 400, "InvalidArgument"),
            ("field-outer-if-none-match", 400, "InvalidArgument"),
        ] {
            report_case(format_args!(
                "mode={mode:?}, expected={expected:?}, code={code:?}"
            ));

            let response = fixture.exchange(&form(mode))?;
            status(&response, expected);

            assert!(
                response.contains(&format!("<Code>{code}</Code>")),
                "{mode}: {response}"
            );

            let observed = fixture.observed.lock().unwrap();

            assert_eq!(
                observed.admitted.len(),
                3,
                "{mode} must fail before dispatch"
            );

            assert_eq!(observed.finishes, 3, "{mode} must not report completion");
        }

        Ok(())
    })
}

type StreamError = Box<dyn std::error::Error + Send + Sync>;

struct Source(Pin<Box<dyn Stream<Item = Result<Bytes, StreamError>> + Send + Sync>>);

impl Stream for Source {
    type Item = Result<Bytes, StreamError>;

    fn poll_next(mut self: Pin<&mut Self>, context: &mut Context<'_>) -> Poll<Option<Self::Item>> {
        self.0.as_mut().poll_next(context)
    }
}

impl ByteStream for Source {}

fn request(source: Source, length: Option<usize>) -> s3s::HttpRequest {
    let mut request = http::Request::builder()
        .method("POST")
        .uri("/photos")
        .header("host", "localhost:9000")
        .header(
            "content-type",
            "multipart/form-data; boundary=mikrio-independent-form",
        );

    if let Some(length) = length {
        request = request.header("content-length", length);
    }

    let body: DynByteStream = Box::pin(source);
    request.body(s3s::Body::from(body)).unwrap()
}

fn body() -> Vec<u8> {
    let request = form("ignored-tail");

    let start = request
        .windows(4)
        .position(|window| window == b"\r\n\r\n")
        .unwrap()
        + 4;

    request[start..].to_vec()
}

#[test]
fn form_boundaries_survive_every_split_and_truncation_fails() -> TestResult {
    Fixture::run(false, |fixture| {
        fixture.runtime.block_on(async {
            let body = body();

            for split in 0..=body.len() {
                let chunks = [body[..split].to_vec(), body[split..].to_vec()];
                let source = stream::iter(chunks.into_iter().map(|chunk| Ok(Bytes::from(chunk))));
                let request = request(Source(Box::pin(source)), Some(body.len()));

                let response = REQUEST_KEY
                    .scope(RefCell::new(None), fixture.service.inner.call(request))
                    .await
                    .unwrap();

                assert_eq!(
                    response.status(),
                    http::StatusCode::NO_CONTENT,
                    "split {split}"
                );
            }

            let before = fixture.observed.lock().unwrap().finishes;

            for end in 0..body.len() {
                report_case(format_args!("end={end:?}"));

                let source = stream::iter([Ok(Bytes::copy_from_slice(&body[..end]))]);
                let request = request(Source(Box::pin(source)), Some(end));

                let response = REQUEST_KEY
                    .scope(RefCell::new(None), fixture.service.inner.call(request))
                    .await
                    .unwrap();

                assert!(response.status().is_client_error(), "truncation at {end}");
                assert_eq!(fixture.observed.lock().unwrap().finishes, before);
            }
        });

        Ok(())
    })
}

#[test]
fn part_headers_cannot_hide_duplicates_or_malformed_parameters() -> TestResult {
    Fixture::run(false, |fixture| {
        fixture.runtime.block_on(async {
            let original = String::from_utf8(body()).unwrap();
            let file_header = "Content-Disposition: form-data; name=\"file\"; filename=\"a.txt\"";
            let harmless_headers = "X-One: 1\r\nX-Two: 2\r\nX-Three: 3\r\n";
            let valid = original.replace(file_header, &format!("{harmless_headers}{file_header}"));
            let source = stream::iter([Ok(Bytes::from(valid))]);

            let response = REQUEST_KEY
                .scope(
                    RefCell::new(None),
                    fixture
                        .service
                        .inner
                        .call(request(Source(Box::pin(source)), None)),
                )
                .await
                .unwrap();

            assert_eq!(response.status(), http::StatusCode::NO_CONTENT);

            for invalid in [
                format!("{file_header}\r\n{harmless_headers}{file_header}"),
                format!("{file_header}; NAME=\"other\""),
                format!("{file_header}; filename=\"other.txt\""),
                format!("{file_header}; unparsed"),
                format!("{file_header}unparsed"),
                format!("{file_header}\r\n{harmless_headers}not a header"),
                "Content-Disposition: form-data; name=\"file\"; name*=UTF-8''other".to_owned(),
                "Content-Disposition: form-data; name=\"file\"; filename=\"unterminated".to_owned(),
                "Content-Disposition: form-data\r\nContent-Disposition: form-data; name=\"file\""
                    .to_owned(),
            ] {
                report_case(format_args!("invalid={invalid:?}"));

                let malformed = original.replace(file_header, &invalid);
                let source = stream::iter([Ok(Bytes::from(malformed))]);

                let response = REQUEST_KEY
                    .scope(
                        RefCell::new(None),
                        fixture
                            .service
                            .inner
                            .call(request(Source(Box::pin(source)), None)),
                    )
                    .await
                    .unwrap();

                assert!(response.status().is_client_error(), "accepted {invalid:?}");
                assert_eq!(fixture.observed.lock().unwrap().finishes, 1);
                assert_eq!(fixture.observed.lock().unwrap().admitted.len(), 1);
            }
        });

        Ok(())
    })
}

#[test]
fn policy_requires_exact_security_constraints_and_present_matching_fields() -> TestResult {
    Fixture::run(false, |fixture| {
        for mode in ["policy-content-types", "policy-empty-metadata"] {
            report_case(format_args!("mode={mode:?}"));

            status(&fixture.exchange(&form(mode))?, 204);
        }

        for mode in [
            "policy-content-types-mismatch",
            "policy-missing-metadata",
            "policy-missing-bucket",
            "policy-bucket-prefix",
            "policy-signing-prefix",
            "policy-unknown-operator",
            "policy-invalid-field-reference",
            "policy-duplicate-member",
        ] {
            report_case(format_args!("mode={mode:?}"));

            let response = fixture.exchange(&form(mode))?;
            assert!(response.starts_with("HTTP/1.1 4"), "accepted {mode}");
            assert_eq!(fixture.observed.lock().unwrap().finishes, 2);
            assert_eq!(fixture.observed.lock().unwrap().admitted.len(), 2);
        }

        Ok(())
    })
}

struct PausedState {
    admitted: tokio::sync::Notify,
    proceed: tokio::sync::Notify,
    expected_chunk: Bytes,
    observed: Arc<std::sync::Mutex<Observations>>,
}

struct PausedSink(Arc<PausedState>);

#[async_trait::async_trait]
impl s3s::S3 for PausedSink {
    async fn post_object(
        &self,
        request: s3s::S3Request<s3s::dto::PostObjectInput>,
    ) -> s3s::S3Result<s3s::S3Response<s3s::dto::PostObjectOutput>> {
        self.0.admitted.notify_one();
        self.0.proceed.notified().await;
        let mut body = request.input.body.unwrap();
        let mut total = 0_u64;

        while let Some(chunk) = body.next().await {
            let chunk = chunk.map_err(|_| s3s::s3_error!(MalformedPOSTRequest))?;
            assert_eq!(chunk.len(), self.0.expected_chunk.len());

            assert_eq!(
                chunk.as_ptr(),
                self.0.expected_chunk.as_ptr(),
                "whole chunks share their allocation"
            );

            total += u64::try_from(chunk.len()).unwrap();
        }

        let mut observed = self.0.observed.lock().unwrap();
        observed.bytes += total;
        observed.finishes += 1;
        Ok(s3s::S3Response::new(s3s::dto::PostObjectOutput::default()))
    }
}

#[test]
fn file_chunks_obey_backpressure_and_tail_completion_owns_eof() -> TestResult {
    Fixture::run(false, |fixture| {
        fixture.runtime.block_on(async {
            let body = body();

            let payload_offset = body
                .windows(7)
                .position(|window| window == b"payload")
                .unwrap();

            let prefix = Bytes::copy_from_slice(&body[..payload_offset]);
            let tail = body[payload_offset + 7..].to_vec();
            let payload = Bytes::from(vec![b'x'; 64 * 1024]);

            let sink = Arc::new(PausedState {
                admitted: tokio::sync::Notify::new(),
                proceed: tokio::sync::Notify::new(),
                expected_chunk: payload.clone(),
                observed: Arc::clone(&fixture.observed),
            });

            let service = Service::with_backend(
                fixture.connection.clone(),
                None,
                PausedSink(Arc::clone(&sink)),
            )
            .unwrap();

            let pulls = Arc::new(AtomicUsize::new(0));
            let observed_pulls = Arc::clone(&pulls);
            let (tail_sender, tail_receiver) = tokio::sync::oneshot::channel::<Vec<u8>>();

            let tail_source =
                stream::once(async move { Ok(Bytes::from(tail_receiver.await.unwrap())) });

            let source = stream::iter(
                std::iter::once(prefix)
                    .chain(std::iter::repeat_n(payload, 512))
                    .map(Ok),
            )
            .inspect(move |_| {
                observed_pulls.fetch_add(1, Ordering::Relaxed);
            })
            .chain(tail_source);

            let request = request(Source(Box::pin(source)), None);
            let response = REQUEST_KEY.scope(RefCell::new(None), service.inner.call(request));
            tokio::pin!(response);

            let admitted = tokio::time::timeout(
                std::time::Duration::from_secs(10),
                futures_util::future::select(response.as_mut(), Box::pin(sink.admitted.notified())),
            )
            .await
            .unwrap();

            assert!(matches!(admitted, futures_util::future::Either::Right(_)));

            assert_eq!(
                pulls.load(Ordering::Relaxed),
                1,
                "admission cannot aggregate file bytes"
            );

            sink.proceed.notify_one();

            assert!(
                response.as_mut().now_or_never().is_none(),
                "a stalled tail is not EOF"
            );

            assert_eq!(pulls.load(Ordering::Relaxed), 513);
            assert_eq!(fixture.observed.lock().unwrap().finishes, 0);
            tail_sender.send(tail).unwrap();

            assert_eq!(
                tokio::time::timeout(std::time::Duration::from_secs(10), response)
                    .await
                    .unwrap()
                    .unwrap()
                    .status(),
                http::StatusCode::NO_CONTENT
            );

            assert_eq!(fixture.observed.lock().unwrap().bytes, 32 * 1024 * 1024);
            assert_eq!(fixture.observed.lock().unwrap().finishes, 1);
        });

        Ok(())
    })
}

#[test]
fn dropping_a_stalled_form_releases_its_source_and_other_requests_progress() -> TestResult {
    Fixture::run(false, |fixture| {
        fixture.runtime.block_on(async {
            let body = body();

            for (completed_requests, prefix_length) in [10, body.len() - 5].into_iter().enumerate()
            {
                report_case(format_args!(
                    "completed_requests={completed_requests:?}, prefix_length={prefix_length:?}"
                ));

                let (reached, waiting) = tokio::sync::oneshot::channel();
                let (resume, resumed) = tokio::sync::oneshot::channel::<Bytes>();
                let prefix = Bytes::copy_from_slice(&body[..prefix_length]);

                let stalled = stream::once(async move {
                    reached.send(()).unwrap();
                    Ok(resumed.await.unwrap())
                });

                let source = stream::iter([Ok(prefix)]).chain(stalled);

                let mut response = Box::pin(
                    REQUEST_KEY.scope(
                        RefCell::new(None),
                        fixture
                            .service
                            .inner
                            .call(request(Source(Box::pin(source)), None)),
                    ),
                );

                let event = tokio::time::timeout(
                    std::time::Duration::from_secs(10),
                    futures_util::future::select(response.as_mut(), waiting),
                )
                .await
                .unwrap();

                assert!(matches!(
                    event,
                    futures_util::future::Either::Right((Ok(()), _))
                ));

                assert_eq!(
                    fixture.observed.lock().unwrap().finishes,
                    completed_requests
                );

                let source = stream::iter([Ok(Bytes::copy_from_slice(&body))]);

                let concurrent = REQUEST_KEY.scope(
                    RefCell::new(None),
                    fixture
                        .service
                        .inner
                        .call(request(Source(Box::pin(source)), None)),
                );

                let completed =
                    tokio::time::timeout(std::time::Duration::from_secs(10), concurrent)
                        .await
                        .unwrap()
                        .unwrap();

                assert_eq!(completed.status(), http::StatusCode::NO_CONTENT);

                assert_eq!(
                    fixture.observed.lock().unwrap().finishes,
                    completed_requests + 1
                );

                drop(response);

                assert!(
                    resume.is_closed(),
                    "cancellation must drop the pending producer"
                );
            }
        });

        Ok(())
    })
}
