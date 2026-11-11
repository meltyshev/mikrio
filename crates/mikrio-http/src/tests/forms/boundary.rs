//! Explicit configured-codec ceiling execution, without a durable payload file.

use std::cell::RefCell;
use std::io::Write as _;
use std::sync::{Arc, Mutex};

use futures_util::{StreamExt, stream};
use hyper::body::Bytes;

use super::{Fixture, Service, Source, TestResult, body, report_case, request};
use crate::access::REQUEST_KEY;

const FILE_LIMIT: usize = 5 * 1024 * 1024 * 1024;
const CHUNK_SIZE: usize = 1024 * 1024;

#[derive(Default)]
struct Observation {
    bytes: usize,
    maximum_chunk: usize,
    finishes: usize,
    rejected: bool,
}

struct CountingSink(Arc<Mutex<Observation>>);

#[async_trait::async_trait]
impl s3s::S3 for CountingSink {
    async fn post_object(
        &self,
        request: s3s::S3Request<s3s::dto::PostObjectInput>,
    ) -> s3s::S3Result<s3s::S3Response<s3s::dto::PostObjectOutput>> {
        let mut body = request.input.body.unwrap();

        while let Some(chunk) = body.next().await {
            let Ok(chunk) = chunk else {
                self.0.lock().unwrap().rejected = true;
                return Err(s3s::s3_error!(MalformedPOSTRequest));
            };

            assert!(chunk.iter().all(|byte| *byte == b'x'));

            let mut observed = self.0.lock().unwrap();
            observed.bytes += chunk.len();
            observed.maximum_chunk = observed.maximum_chunk.max(chunk.len());
        }

        self.0.lock().unwrap().finishes += 1;
        Ok(s3s::S3Response::new(s3s::dto::PostObjectOutput::default()))
    }
}

#[test]
#[ignore = "explicit 30 GiB streaming workload; select this test by name with --ignored"]
fn configured_file_ceiling_streams_below_at_and_above_with_bounded_chunks() -> TestResult {
    Fixture::run(false, |fixture| {
        fixture.runtime.block_on(async {
            // The independently signed policy has no file-length range. Changing
            // file bytes leaves its signature valid and isolates the configured cap.
            let encoded = body();

            let offset = encoded
                .windows(7)
                .position(|bytes| bytes == b"payload")
                .unwrap();

            let prefix = Bytes::copy_from_slice(&encoded[..offset]);
            let tail = Bytes::copy_from_slice(&encoded[offset + 7..]);
            let payload = Bytes::from(vec![b'x'; CHUNK_SIZE]);

            for known_length in [false, true] {
                for size in [FILE_LIMIT - 1, FILE_LIMIT, FILE_LIMIT + 1] {
                    report_case(format_args!("known_length={known_length:?}, size={size:?}"));

                    let observed = Arc::new(Mutex::new(Observation::default()));

                    let service = Service::with_backend(
                        fixture.connection.clone(),
                        None,
                        CountingSink(Arc::clone(&observed)),
                    )
                    .unwrap();

                    let chunks = std::iter::once(prefix.clone())
                        .chain(std::iter::repeat_n(payload.clone(), size / CHUNK_SIZE))
                        .chain((size % CHUNK_SIZE != 0).then(|| payload.slice(..size % CHUNK_SIZE)))
                        .chain(std::iter::once(tail.clone()));

                    let source = Source(Box::pin(stream::iter(chunks.map(Ok))));
                    let length = known_length.then_some(prefix.len() + size + tail.len());

                    let response = REQUEST_KEY
                        .scope(
                            RefCell::new(None),
                            service.inner.call(request(source, length)),
                        )
                        .await
                        .unwrap();

                    let state = observed.lock().unwrap();

                    assert!(state.maximum_chunk <= CHUNK_SIZE);
                    assert!(state.bytes <= FILE_LIMIT);

                    if size <= FILE_LIMIT {
                        assert_eq!(response.status(), http::StatusCode::NO_CONTENT);
                        assert_eq!(state.bytes, size);
                        assert_eq!(state.finishes, 1);
                        assert!(!state.rejected);
                    } else {
                        assert_eq!(response.status(), http::StatusCode::BAD_REQUEST);
                        assert_eq!(state.finishes, 0);
                        assert!(state.rejected);
                        assert_eq!(state.bytes, FILE_LIMIT);
                    }

                    let outer_length = length.map_or_else(|| "null".to_owned(), |value| value.to_string());

                    writeln!(
                        std::io::stdout().lock(),
                        "POST_CODEC_BOUNDARY {{\"configured_file_limit\":{FILE_LIMIT},\"offered_file_bytes\":{size},\"outer_content_length\":{outer_length},\"consumed_file_bytes\":{},\"maximum_chunk_bytes\":{},\"finishes\":{},\"status\":{}}}",
                        state.bytes, state.maximum_chunk, state.finishes, response.status().as_u16(),
                    )?;
                }
            }

            Ok::<(), std::io::Error>(())
        })?;

        Ok(())
    })
}
