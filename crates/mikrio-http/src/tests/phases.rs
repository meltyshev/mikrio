//! Deferred protocol responses over bounded, test-only copy work.

use std::future::Future;
use std::task::Poll;

use s3s::access::{S3Access, S3AccessContext};
use s3s::auth::SimpleAuth;
use s3s::dto::{
    CompleteMultipartUploadInput, CompleteMultipartUploadOutput, CopyObjectInput, CopyObjectOutput,
    CopyObjectResult, CopyPartResult, Timestamp, TimestampFormat, UploadPartCopyInput,
    UploadPartCopyOutput,
};

use super::*;

const ETAG: &str = "\"25f9e794323b453885f5181f1b624d0b\"";

struct PhaseAccess;

#[async_trait::async_trait]
impl S3Access for PhaseAccess {
    async fn check(&self, context: &mut S3AccessContext<'_>) -> S3Result<()> {
        if context
            .credentials()
            .is_none_or(|credentials| credentials.access_key != APP_KEY)
        {
            return Err(s3s::s3_error!(AccessDenied));
        }

        match context.s3_op().name() {
            "CopyObject" | "CompleteMultipartUpload" | "UploadPartCopy" | "GetObjectAttributes" => {
                Ok(())
            }
            _ => Err(s3s::s3_error!(NotImplemented)),
        }
    }
}

struct Phases {
    observed: Arc<Mutex<Observations>>,
    gate: Option<Arc<tokio::sync::Notify>>,
}

impl Phases {
    fn admit(&self, bucket: String, key: String) -> S3Result<()> {
        if key == "early-error" {
            return Err(s3s::s3_error!(NoSuchKey));
        }

        self.observed.lock().unwrap().admitted.push((bucket, key));
        Ok(())
    }
}

#[async_trait::async_trait]
impl s3s::S3 for Phases {
    async fn get_object_attributes(
        &self,
        request: S3Request<s3s::dto::GetObjectAttributesInput>,
    ) -> S3Result<S3Response<s3s::dto::GetObjectAttributesOutput>> {
        attributes::response(request)
    }

    async fn copy_object(
        &self,
        request: S3Request<CopyObjectInput>,
    ) -> S3Result<S3Response<CopyObjectOutput>> {
        let key = request.input.key;
        self.admit(request.input.bucket, key.clone())?;
        let gate = self.gate.clone();

        Ok(S3Response::new(CopyObjectOutput {
            future: Some(Box::pin(async move {
                copy_work(&key, gate).await?;

                Ok(CopyObjectOutput {
                    copy_object_result: Some(CopyObjectResult {
                        e_tag: Some(ETAG.parse().unwrap()),
                        last_modified: Some(timestamp()),
                        ..CopyObjectResult::default()
                    }),
                    ..CopyObjectOutput::default()
                })
            })),
            ..CopyObjectOutput::default()
        }))
    }

    async fn complete_multipart_upload(
        &self,
        request: S3Request<CompleteMultipartUploadInput>,
    ) -> S3Result<S3Response<CompleteMultipartUploadOutput>> {
        let key = request.input.key;
        self.admit(request.input.bucket, key.clone())?;
        let gate = self.gate.clone();

        Ok(S3Response::new(CompleteMultipartUploadOutput {
            future: Some(Box::pin(async move {
                copy_work(&key, gate).await?;

                Ok(CompleteMultipartUploadOutput {
                    e_tag: Some(ETAG.parse().unwrap()),
                    ..CompleteMultipartUploadOutput::default()
                })
            })),
            ..CompleteMultipartUploadOutput::default()
        }))
    }

    async fn upload_part_copy(
        &self,
        request: S3Request<UploadPartCopyInput>,
    ) -> S3Result<S3Response<UploadPartCopyOutput>> {
        let key = request.input.key;
        self.admit(request.input.bucket, key.clone())?;

        if key == "deferred-error" {
            let gate = self.gate.clone();

            return Ok(S3Response::new(UploadPartCopyOutput {
                future: Some(Box::pin(async move {
                    copy_work(&key, gate).await?;
                    Ok(UploadPartCopyOutput::default())
                })),
                ..UploadPartCopyOutput::default()
            }));
        }

        Ok(S3Response::new(UploadPartCopyOutput {
            copy_part_result: Some(CopyPartResult {
                e_tag: Some(ETAG.parse().unwrap()),
                last_modified: Some(timestamp()),
                ..CopyPartResult::default()
            }),
            ..UploadPartCopyOutput::default()
        }))
    }
}

fn timestamp() -> Timestamp {
    Timestamp::parse(TimestampFormat::DateTime, "2026-10-09T00:00:00Z").unwrap()
}

pub(super) struct PhaseFixture {
    pub(super) runtime: Arc<tokio::runtime::Runtime>,
    pub(super) service: Arc<Service>,
    pub(super) observed: Arc<Mutex<Observations>>,
}

impl PhaseFixture {
    fn exchange(&self, request: &[u8]) -> Result<String, Box<dyn std::error::Error>> {
        let (response, outcome) = exchange(&self.runtime, &self.service, request)?;
        outcome?;
        Ok(response)
    }
}

pub(super) fn fixture() -> Result<PhaseFixture, Box<dyn std::error::Error>> {
    fixture_with_gate(None)
}

fn fixture_with_gate(
    gate: Option<Arc<tokio::sync::Notify>>,
) -> Result<PhaseFixture, Box<dyn std::error::Error>> {
    // This observation-only backend has no bucket CORS state. Close its native
    // owner explicitly before serving; real CORS tests use the storage fixture.
    let (runtime, connection) = cleanup::Scope::run(TestDirectory::new()?, |cleanup| {
        let connection = cleanup.track(cleanup.runtime.block_on(Connection::open(
            cleanup.directory.path().join("store"),
            None,
        ))?);

        Ok((Arc::clone(&cleanup.runtime), connection))
    })?;

    let observed = Arc::new(Mutex::new(Observations::default()));
    let mut config = S3Config::default();
    config.expected_region = Some("us-east-1".parse()?);
    config.sig_v4_strict_path = true;
    config.enable_sig_v2 = false;
    config.xml_max_body_size = 64 * 1024;

    let mut builder = S3ServiceBuilder::new(Phases {
        observed: Arc::clone(&observed),
        gate,
    });

    builder.set_config(Arc::new(StaticConfigProvider::new(Arc::new(config))));
    builder.set_auth(SimpleAuth::from_single(APP_KEY, SECRET));
    builder.set_access(PhaseAccess);

    Ok(PhaseFixture {
        runtime,
        service: Arc::new(Service {
            inner: builder.build(),
            cors: cors::Cors::new(connection),
            public_endpoint: None,
            connections: tokio::sync::Semaphore::new(64),
        }),
        observed,
    })
}

async fn copy_work(mode: &str, gate: Option<Arc<tokio::sync::Notify>>) -> S3Result<()> {
    let last_progress = Mutex::new(tokio::time::Instant::now());

    let work = async {
        if let Some(gate) = &gate {
            gate.notified().await;
        }

        if mode == "stalled" {
            std::future::pending::<()>().await;
        }

        let mut source = b"123456789".as_slice();
        let mut copied = Vec::new();
        let mut buffer = [0; 3];

        loop {
            tokio::time::sleep(Duration::from_millis(100)).await;

            let count = source
                .read(&mut buffer)
                .await
                .map_err(|_| s3s::s3_error!(InternalError))?;

            if count == 0 {
                break;
            }

            copied.extend_from_slice(&buffer[..count]);
            *last_progress.lock().unwrap() = tokio::time::Instant::now();
        }

        assert_eq!(copied, b"123456789");

        if mode == "deferred-error" {
            return Err(s3s::s3_error!(InternalError));
        }

        Ok(())
    };

    let idle_limit = Duration::from_millis(300);
    let mut work = std::pin::pin!(work);
    let mut deadline = std::pin::pin!(tokio::time::sleep(idle_limit));

    std::future::poll_fn(|context| {
        if let Poll::Ready(result) = work.as_mut().poll(context) {
            return Poll::Ready(result);
        }

        deadline
            .as_mut()
            .reset(*last_progress.lock().unwrap() + idle_limit);

        if deadline.as_mut().poll(context).is_ready() {
            return Poll::Ready(Err(s3s::s3_error!(RequestTimeout)));
        }

        Poll::Pending
    })
    .await
}

fn request(operation: &str, mode: &str) -> Vec<u8> {
    let script =
        PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../tests/contract/http_sigv4.py");

    let output = crate::tests::python::command()
        .arg(script)
        .args(["--response-phase", operation, mode])
        .output()
        .unwrap();

    assert!(output.status.success(), "independent phase request failed");
    output.stdout
}

#[test]
fn deferred_errors_keep_their_response_request_id_across_concurrent_work() -> TestResult {
    let fixture = fixture()?;
    let mut identifiers = std::collections::BTreeSet::new();

    for operation in ["copy", "complete", "part"] {
        report_case(format_args!("operation={operation}"));
        let raw = request(operation, "deferred-error");

        let responses = std::thread::scope(|scope| {
            let first = scope.spawn(|| fixture.exchange(&raw).unwrap());
            let second = scope.spawn(|| fixture.exchange(&raw).unwrap());
            [first.join().unwrap(), second.join().unwrap()]
        });

        for response in responses {
            status(&response, 200);
            assert_eq!(response.matches("HTTP/1.1 ").count(), 1);
            assert_eq!(response.matches("<?xml").count(), 1);
            assert!(response.contains("<Code>InternalError</Code>"));
            let request_id = response_request_id(&response);
            assert!(identifiers.insert(request_id.to_owned()));
            assert!(response.contains(&format!("<RequestId>{request_id}</RequestId>")));
            assert!(!response.contains(APP_KEY));
            assert!(!response.contains(SECRET));
        }
    }

    assert_eq!(fixture.observed.lock().unwrap().admitted.len(), 6);
    Ok(())
}

#[test]
fn response_headers_precede_work_and_cancellation_drops_the_owned_future() -> TestResult {
    for operation in ["copy", "complete"] {
        report_case(format_args!("operation={operation:?}"));

        let gate = Arc::new(tokio::sync::Notify::new());
        let fixture = fixture_with_gate(Some(Arc::clone(&gate)))?;

        fixture.runtime.block_on(async {
            let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await?;
            let mut client = tokio::net::TcpStream::connect(listener.local_addr()?).await?;
            let (server, _) = listener.accept().await?;
            let connection = fixture.service.serve(server);
            tokio::pin!(connection);

            client
                .write_all(&request(operation, "deferred-success"))
                .await?;

            let read_headers = async {
                let mut response = Vec::new();
                let mut chunk = [0; 1024];

                while !response.windows(4).any(|window| window == b"\r\n\r\n") {
                    let count = client.read(&mut chunk).await?;
                    assert_ne!(count, 0);
                    response.extend_from_slice(&chunk[..count]);
                    assert!(response.len() <= 8192);
                }

                let response = String::from_utf8(response)?;
                status(&response, 200);
                assert!(!response.contains("<ETag>"));
                assert_eq!(Arc::strong_count(&gate), 3);
                Ok::<_, Box<dyn std::error::Error>>(())
            };

            tokio::pin!(read_headers);

            tokio::time::timeout(
                Duration::from_secs(2),
                std::future::poll_fn(|context| {
                    assert!(connection.as_mut().poll(context).is_pending());
                    read_headers.as_mut().poll(context)
                }),
            )
            .await??;

            Ok::<_, Box<dyn std::error::Error>>(())
        })?;

        assert_eq!(Arc::strong_count(&gate), 2);
        drop(fixture);
        assert_eq!(Arc::strong_count(&gate), 1);
    }

    Ok(())
}

#[test]
fn deferred_copy_and_completion_preserve_final_errors_and_work_deadlines() -> TestResult {
    let fixture = fixture()?;

    for operation in ["copy", "complete"] {
        report_case(format_args!("operation={operation:?}"));

        status(&fixture.exchange(&request(operation, "early-error"))?, 404);

        for (mode, expected) in [
            ("deferred-success", "<ETag>"),
            ("deferred-error", "<Code>InternalError</Code>"),
            ("stalled", "<Code>RequestTimeout</Code>"),
        ] {
            report_case(format_args!(
                "operation={operation:?}, mode={mode:?}, expected={expected:?}"
            ));

            let response = fixture.exchange(&request(operation, mode))?;
            status(&response, 200);
            assert_eq!(response.matches("<?xml").count(), 1, "{operation}/{mode}");
            assert!(response.contains(expected), "{operation}/{mode}");
            assert!(response.contains("transfer-encoding: chunked"));
        }
    }

    assert_eq!(fixture.observed.lock().unwrap().admitted.len(), 6);
    Ok(())
}

#[test]
fn copy_waits_for_valid_empty_framing_before_backend_admission() -> TestResult {
    let fixture = fixture()?;

    for operation in ["copy", "part"] {
        report_case(format_args!("operation={operation:?}"));

        for mode in ["empty", "chunked-empty"] {
            report_case(format_args!("operation={operation:?}, mode={mode:?}"));

            status(&fixture.exchange(&request(operation, mode))?, 200);
        }

        let admitted = fixture.observed.lock().unwrap().admitted.len();

        for mode in ["nonempty", "chunked-byte", "chunked-invalid"] {
            report_case(format_args!("operation={operation:?}, mode={mode:?}"));

            let response = fixture.exchange(&request(operation, mode))?;
            status(&response, 400);
            assert!(response.contains("<Code>InvalidRequest</Code>"));
            assert_eq!(fixture.observed.lock().unwrap().admitted.len(), admitted);
        }

        fixture.runtime.block_on(async {
            let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await?;
            let mut client = tokio::net::TcpStream::connect(listener.local_addr()?).await?;
            let (server, _) = listener.accept().await?;
            let connection = fixture.service.serve(server);
            tokio::pin!(connection);

            client
                .write_all(&request(operation, "chunked-stalled"))
                .await?;

            assert!(
                tokio::time::timeout(Duration::from_millis(50), &mut connection)
                    .await
                    .is_err()
            );

            assert_eq!(fixture.observed.lock().unwrap().admitted.len(), admitted);
            client.write_all(b"0\r\n").await?;
            client.shutdown().await?;

            let (response, _server) = tokio::time::timeout(Duration::from_secs(2), async {
                let read = async {
                    let mut response = String::new();
                    client.read_to_string(&mut response).await?;
                    Ok::<_, std::io::Error>(response)
                };

                futures_util::future::join(read, &mut connection).await
            })
            .await?;

            if let Ok(response) = response {
                assert!(!response.starts_with("HTTP/1.1 200 "));
            }

            assert_eq!(fixture.observed.lock().unwrap().admitted.len(), admitted);
            Ok::<_, Box<dyn std::error::Error>>(())
        })?;
    }

    Ok(())
}

#[test]
fn delayed_empty_copy_and_part_copy_admit_only_after_the_terminal_chunk() -> TestResult {
    let fixture = fixture()?;

    for operation in ["copy", "part"] {
        let admitted = fixture.observed.lock().unwrap().admitted.len();

        let raw = String::from_utf8(request(operation, "chunked-stalled"))?.replace(
            "connection: close",
            "connection: close\r\nexpect: 100-continue",
        );

        fixture.runtime.block_on(async {
            let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await?;
            let mut client = tokio::net::TcpStream::connect(listener.local_addr()?).await?;
            let (server, _) = listener.accept().await?;

            let client = async {
                client.write_all(raw.as_bytes()).await?;
                let mut headers = Vec::new();

                while !headers.ends_with(b"\r\n\r\n") {
                    assert!(headers.len() < 8192);
                    headers.push(client.read_u8().await?);
                }

                status(std::str::from_utf8(&headers)?, 100);
                assert_eq!(fixture.observed.lock().unwrap().admitted.len(), admitted);
                client.write_all(b"0\r\n").await?;
                let mut byte = [0];

                assert!(
                    tokio::time::timeout(Duration::from_millis(50), client.read(&mut byte))
                        .await
                        .is_err()
                );

                assert_eq!(fixture.observed.lock().unwrap().admitted.len(), admitted);
                client.write_all(b"\r\n").await?;
                let mut response = String::new();
                client.read_to_string(&mut response).await?;
                status(&response, 200);

                assert!(response.contains(if operation == "copy" {
                    "<CopyObjectResult>"
                } else {
                    "<CopyPartResult>"
                }));

                Ok::<_, Box<dyn std::error::Error>>(())
            };

            let (client, server) = tokio::time::timeout(
                Duration::from_secs(5),
                futures_util::future::join(client, fixture.service.serve(server)),
            )
            .await?;

            client?;
            server?;

            assert_eq!(
                fixture.observed.lock().unwrap().admitted.len(),
                admitted + 1
            );

            Ok::<_, Box<dyn std::error::Error>>(())
        })?;
    }

    Ok(())
}
