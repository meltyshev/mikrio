#![allow(
    clippy::unwrap_used,
    reason = "test assertions use owned synthetic fixtures"
)]

use std::path::PathBuf;
use std::sync::Mutex;
use std::sync::atomic::{AtomicUsize, Ordering};

mod attributes;
pub(super) mod cleanup;
#[path = "../../mikrio-core/tests/support/directory.rs"]
mod directory;
mod forms;
mod integrity;
mod phases;
#[path = "../../mikrio-core/tests/support/python.rs"]
mod python;
mod sdk_fixture;
mod storage;
mod values;
pub(super) use directory::TestDirectory;

use futures_util::StreamExt;
use s3s::dto::{
    GetObjectInput, GetObjectOutput, PostObjectInput, PostObjectOutput, PutObjectInput,
    PutObjectOutput,
};
use s3s::{S3Request, S3Response, S3Result};
use tokio::io::{AsyncReadExt, AsyncWriteExt};

use super::*;

type TestResult = Result<(), Box<dyn std::error::Error>>;

// Fixture setup is not an open-admission saturation scenario. Bound concurrent
// directory initialization below the core's four preparation permits; individual
// transport scenarios retain their own deliberate request concurrency.
static FIXTURES: tokio::sync::Semaphore = tokio::sync::Semaphore::const_new(2);

const APP_KEY: &str = "mikrio_a00112233445566778899AABBCCDDEEFF";
const SECRET: &str = "synthetic-http-probe-secret";

#[derive(Default)]
struct ChecksumPlacement {
    header: bool,
    trailer: bool,
}

#[derive(Default)]
struct Observations {
    admitted: Vec<(String, String)>,
    finishes: usize,
    bytes: u64,
    boto_crc32: ChecksumPlacement,
    node_crc32: ChecksumPlacement,
    active_uploads: usize,
    peak_uploads: usize,
}

struct ActiveUpload(Arc<Mutex<Observations>>);

impl ActiveUpload {
    fn new(observed: &Arc<Mutex<Observations>>) -> Self {
        let mut state = observed.lock().unwrap();
        state.active_uploads += 1;
        state.peak_uploads = state.peak_uploads.max(state.active_uploads);
        Self(Arc::clone(observed))
    }
}

impl Drop for ActiveUpload {
    fn drop(&mut self) {
        // A failed assertion may poison observations; cleanup must not panic again.
        let mut observed = self
            .0
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);

        observed.active_uploads -= 1;
    }
}

#[derive(Clone)]
struct Sink(Arc<Mutex<Observations>>);

#[async_trait::async_trait]
impl s3s::S3 for Sink {
    async fn post_object(
        &self,
        request: S3Request<PostObjectInput>,
    ) -> S3Result<S3Response<PostObjectOutput>> {
        assert!(
            request
                .extensions
                .get::<Arc<mikrio_core::access::HttpIdentity>>()
                .is_some()
        );

        self.0
            .lock()
            .unwrap()
            .admitted
            .push((request.input.bucket.clone(), request.input.key.clone()));

        let facts = integrity::consume_post(request).await?;
        let mut observation = self.0.lock().unwrap();
        observation.bytes += facts.length();
        observation.finishes += 1;

        Ok(S3Response::new(PostObjectOutput {
            e_tag: Some(facts.etag().parse().unwrap()),
            ..PostObjectOutput::default()
        }))
    }

    async fn get_object(
        &self,
        request: S3Request<GetObjectInput>,
    ) -> S3Result<S3Response<GetObjectOutput>> {
        assert!(
            request
                .extensions
                .get::<Arc<mikrio_core::access::HttpIdentity>>()
                .is_some()
        );

        let key = request.input.key;

        self.0
            .lock()
            .unwrap()
            .admitted
            .push((request.input.bucket, key.clone()));

        let metadata = if key == "metadata-limit" {
            let metadata = mikrio_core::object::UserMetadata::new(
                (0..64).map(|index| (format!("key{index:02}"), String::new())),
            )
            .unwrap();

            let decoded =
                mikrio_core::object::UserMetadata::from_json(&metadata.to_json().unwrap()).unwrap();

            Some(
                decoded
                    .entries()
                    .iter()
                    .map(|(key, value)| (key.clone(), value.clone()))
                    .collect(),
            )
        } else {
            None
        };

        // This test-only response reports the exact key seen by the sink. It
        // lets an external proxy probe observe routing without a second parser
        // or an implied persistent GET implementation.
        Ok(S3Response::new(GetObjectOutput {
            content_length: Some(i64::try_from(key.len()).unwrap()),
            metadata,
            body: Some(s3s::Body::from(key).into()),
            ..GetObjectOutput::default()
        }))
    }

    async fn put_object(
        &self,
        request: S3Request<PutObjectInput>,
    ) -> S3Result<S3Response<PutObjectOutput>> {
        let _upload = ActiveUpload::new(&self.0);

        {
            let mut observed = self.0.lock().unwrap();

            match request.input.key.as_str() {
                "boto-default" => {
                    observed.boto_crc32.header = request.input.checksum_crc32.is_some();

                    observed.boto_crc32.trailer = request
                        .headers
                        .get("x-amz-trailer")
                        .is_some_and(|value| value == "x-amz-checksum-crc32");
                }
                "node-default" => {
                    observed.node_crc32.header = request.input.checksum_crc32.is_some();
                }
                "node-trailer" => {
                    observed.node_crc32.trailer = request
                        .headers
                        .get("x-amz-trailer")
                        .is_some_and(|value| value == "x-amz-checksum-crc32");
                }
                _ => {}
            }
        }

        self.0
            .lock()
            .unwrap()
            .admitted
            .push((request.input.bucket.clone(), request.input.key.clone()));

        let facts = integrity::consume(request).await?;
        let mut observation = self.0.lock().unwrap();
        observation.bytes += facts.length();
        observation.finishes += 1;

        let mut output = PutObjectOutput {
            e_tag: Some(facts.etag().parse().unwrap()),
            ..PutObjectOutput::default()
        };

        let checksum = Some(facts.checksum().encode());

        match facts.checksum().algorithm() {
            mikrio_core::integrity::ChecksumAlgorithm::Crc32 => output.checksum_crc32 = checksum,
            mikrio_core::integrity::ChecksumAlgorithm::Crc32c => output.checksum_crc32c = checksum,
            mikrio_core::integrity::ChecksumAlgorithm::Crc64Nvme => {
                output.checksum_crc64nvme = checksum;
            }
            mikrio_core::integrity::ChecksumAlgorithm::Sha1 => output.checksum_sha1 = checksum,
            mikrio_core::integrity::ChecksumAlgorithm::Sha256 => output.checksum_sha256 = checksum,
        }

        Ok(S3Response::new(output))
    }
}

struct Fixture<'a> {
    cleanup: &'a cleanup::Scope,
    service: Arc<Service>,
    bootstrap: Arc<Bootstrap>,
    observed: Arc<Mutex<Observations>>,
    database: rusqlite::Connection,
    connection: Connection,
    runtime: Arc<tokio::runtime::Runtime>,
    directory: &'a TestDirectory,
    _slot: tokio::sync::SemaphorePermit<'static>,
}

impl<'a> Fixture<'a> {
    fn run(public: bool, scenario: impl FnOnce(&mut Fixture<'_>) -> TestResult) -> TestResult {
        cleanup::Scope::run(TestDirectory::new()?, |cleanup| {
            let mut fixture = Fixture::new(public, cleanup)?;
            scenario(&mut fixture)?;
            fixture.database.close().map_err(|(_, error)| error)?;
            Ok(())
        })
    }

    fn new(public: bool, cleanup: &'a cleanup::Scope) -> Result<Self, Box<dyn std::error::Error>> {
        let runtime = Arc::clone(&cleanup.runtime);
        let directory = &cleanup.directory;
        let slot = runtime.block_on(FIXTURES.acquire())?;

        let connection = cleanup
            .track(runtime.block_on(Connection::open(directory.path().join("store"), None))?);

        let database =
            rusqlite::Connection::open(connection.info().directory.join("metadata.sqlite3"))?;

        database.execute_batch(r#"
            PRAGMA foreign_keys=ON;
            INSERT INTO principals(principal_id,name,is_enabled,created_at_ms,policy_generation,policy_json)
            VALUES(zeroblob(16),'app',1,0,randomblob(16),'{"Version":"2012-10-17","Statement":{"Effect":"Allow","Action":["s3:GetObject","s3:PutObject"],"Resource":"*"}}');
            INSERT INTO buckets(bucket_id,name,created_at_ms) VALUES(randomblob(16),'photos',0);
        "#)?;

        database.execute(
            "INSERT INTO access_keys(access_key_id,principal_id,secret_access_key,created_at_ms) VALUES(?1,zeroblob(16),?2,0)",
            [APP_KEY, SECRET],
        )?;

        if public {
            database.execute("UPDATE buckets SET policy_generation=randomblob(16), policy_json=?1", [r#"{"Version":"2012-10-17","Statement":{"Effect":"Allow","Principal":"*","Action":"s3:GetObject","Resource":"arn:aws:s3:::photos/*"}}"#])?;
        }

        let observed = Arc::new(Mutex::new(Observations::default()));

        let bootstrap = Arc::new(Bootstrap::new(
            "probe-bootstrap".into(),
            mikrio_core::access::Secret::new(SECRET.into())?,
        )?);

        let service = Arc::new(Service::with_backend(
            connection.clone(),
            Some(Arc::clone(&bootstrap)),
            Sink(Arc::clone(&observed)),
        )?);

        Ok(Self {
            cleanup,
            service,
            bootstrap,
            observed,
            database,
            connection,
            runtime,
            directory,
            _slot: slot,
        })
    }

    fn exchange(&self, request: &[u8]) -> Result<String, Box<dyn std::error::Error>> {
        let (response, outcome) = self.exchange_outcome(request)?;
        outcome?;
        Ok(response)
    }

    fn exchange_outcome(
        &self,
        request: &[u8],
    ) -> Result<(String, std::io::Result<()>), Box<dyn std::error::Error>> {
        exchange(&self.runtime, &self.service, request)
    }
}

fn exchange(
    runtime: &tokio::runtime::Runtime,
    service: &Service,
    request: &[u8],
) -> Result<(String, std::io::Result<()>), Box<dyn std::error::Error>> {
    runtime.block_on(async {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await?;
        let address = listener.local_addr()?;

        let client = async {
            let mut socket = tokio::net::TcpStream::connect(address).await?;
            socket.write_all(request).await?;
            let mut response = Vec::new();
            socket.read_to_end(&mut response).await?;
            Ok::<_, std::io::Error>(response)
        };

        let server = async {
            let (socket, _) = listener.accept().await?;
            service.serve(socket).await
        };

        let (response, server) = tokio::time::timeout(
            Duration::from_secs(10),
            futures_util::future::join(client, server),
        )
        .await?;

        Ok((String::from_utf8(response?)?, server))
    })
}

fn response_request_id(response: &str) -> &str {
    let (headers, _) = response.split_once("\r\n\r\n").unwrap();

    let values: Vec<_> = headers
        .lines()
        .filter_map(|line| line.strip_prefix("x-amz-request-id: "))
        .collect();

    assert_eq!(values.len(), 1, "one response request identifier");
    let value = values[0];
    assert_eq!(value.len(), 32);
    assert!(value.bytes().all(|byte| byte.is_ascii_hexdigit()));
    value
}

fn signed(
    method: &str,
    path: &str,
    mode: &str,
    key: &str,
    secret: &str,
    region: &str,
    service: &str,
    body: &str,
) -> Vec<u8> {
    let script =
        PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../tests/contract/http_sigv4.py");

    let output = python::command()
        .arg(script)
        .args([method, path, mode, key, secret, region, service, body])
        .output()
        .unwrap();

    assert!(output.status.success(), "independent signer failed");
    output.stdout
}

fn get(path: &str, mode: &str) -> Vec<u8> {
    signed("GET", path, mode, APP_KEY, SECRET, "us-east-1", "s3", "")
}

fn unsigned() -> &'static [u8] {
    b"GET /photos/key HTTP/1.1\r\nhost: localhost:9000\r\nx-mikrio-authority: native\r\nconnection: close\r\n\r\n"
}

/// Identify a case in captured output, including failures from shared helpers.
#[track_caller]
#[allow(
    clippy::print_stderr,
    reason = "identify parameterized test cases in captured output"
)]
pub(crate) fn report_case(context: std::fmt::Arguments<'_>) {
    eprintln!("case at {}: {context}", std::panic::Location::caller());
}

#[track_caller]
fn status(response: &str, code: u16) {
    assert!(
        response.starts_with(&format!("HTTP/1.1 {code} ")),
        "response status mismatch: {}",
        response.lines().next().unwrap_or("empty")
    );
}

#[test]
fn signed_header_and_presign_preserve_exact_keys_and_current_revocation() -> TestResult {
    Fixture::run(false, |fixture| {
        for mode in ["header", "query"] {
            for (wire, logical) in [
                ("/photos/a%2Fb", "a/b"),
                ("/photos/a%252Fb", "a%2Fb"),
                ("/photos/a%2Bb", "a+b"),
                ("/photos/a/../b//", "a/../b//"),
                ("/photos/%C3%A9", "é"),
            ] {
                report_case(format_args!(
                    "mode={mode:?}, wire={wire:?}, logical={logical:?}"
                ));

                status(&fixture.exchange(&get(wire, mode))?, 200);

                assert_eq!(
                    fixture.observed.lock().unwrap().admitted.last(),
                    Some(&("photos".into(), logical.into()))
                );
            }
        }

        fixture.database.execute("DELETE FROM access_keys", [])?;
        status(&fixture.exchange(&get("/photos/key", "header"))?, 403);
        assert_eq!(fixture.observed.lock().unwrap().admitted.len(), 10);
        Ok(())
    })
}

#[test]
fn supplied_bad_auth_never_falls_back_to_public_or_native_access() -> TestResult {
    Fixture::run(true, |fixture| {
        status(&fixture.exchange(unsigned())?, 200);

        for mode in ["header", "query"] {
            report_case(format_args!("mode={mode:?}"));

            let bad = signed(
                "GET",
                "/photos/key",
                mode,
                APP_KEY,
                "wrong-secret",
                "us-east-1",
                "s3",
                "",
            );

            status(&fixture.exchange(&bad)?, 403);
        }

        let unknown = signed(
            "GET",
            "/photos/key",
            "header",
            "mikrio_aFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFF",
            SECRET,
            "us-east-1",
            "s3",
            "",
        );

        status(&fixture.exchange(&unknown)?, 403);
        let valid = String::from_utf8(get("/photos/key", "header"))?;

        for altered in [
            valid.replace("localhost:9000", "localhost:9001"),
            valid.replace("/photos/key", "/photos/other"),
            valid.replace("GET ", "HEAD "),
        ] {
            report_case(format_args!("altered={altered:?}"));

            status(&fixture.exchange(altered.as_bytes())?, 403);
        }

        let duplicate = String::from_utf8(get("/photos/key", "header"))?.replace(
            "connection: close",
            "authorization: invalid\r\nconnection: close",
        );

        status(&fixture.exchange(duplicate.as_bytes())?, 400);
        status(&fixture.exchange(&get("/photos/key", "expired"))?, 403);

        fixture
            .database
            .execute("UPDATE principals SET is_enabled=0", [])?;

        status(&fixture.exchange(&get("/photos/key", "header"))?, 403);
        assert_eq!(fixture.observed.lock().unwrap().admitted.len(), 1);
        Ok(())
    })
}

#[test]
fn bootstrap_is_listener_local_and_wrong_region_or_service_is_denied() -> TestResult {
    Fixture::run(false, |fixture| {
        status(&fixture.exchange(unsigned())?, 403);

        let valid = signed(
            "GET",
            "/photos/key",
            "header",
            "probe-bootstrap",
            SECRET,
            "us-east-1",
            "s3",
            "",
        );

        status(&fixture.exchange(&valid)?, 200);
        fixture.bootstrap.close();
        status(&fixture.exchange(&valid)?, 403);

        let wrong_region = signed(
            "GET",
            "/photos/key",
            "header",
            APP_KEY,
            SECRET,
            "eu-west-1",
            "s3",
            "",
        );

        status(&fixture.exchange(&wrong_region)?, 400);

        let wrong_service = signed(
            "GET",
            "/photos/key",
            "header",
            APP_KEY,
            SECRET,
            "us-east-1",
            "sts",
            "",
        );

        status(&fixture.exchange(&wrong_service)?, 501);
        assert_eq!(fixture.observed.lock().unwrap().admitted.len(), 1);
        Ok(())
    })
}

#[test]
fn signed_payload_must_finish_verification_before_sink_finish() -> TestResult {
    Fixture::run(false, |fixture| {
        let valid = signed(
            "PUT",
            "/photos/key",
            "header",
            APP_KEY,
            SECRET,
            "us-east-1",
            "s3",
            "body",
        );

        status(&fixture.exchange(&valid)?, 200);
        let mut tampered = valid.clone();
        *tampered.last_mut().unwrap() = b'!';
        status(&fixture.exchange(&tampered)?, 400);
        let observed = fixture.observed.lock().unwrap();
        assert_eq!(observed.finishes, 1);
        assert_eq!(observed.bytes, 4);
        drop(observed);
        Ok(())
    })
}

#[test]
fn canonical_path_signature_mismatch_cannot_retry_the_raw_path() -> TestResult {
    Fixture::run(false, |fixture| {
        status(&fixture.exchange(&get("/photos/a$b", "header"))?, 200);
        status(&fixture.exchange(&get("/photos/a$b", "query"))?, 200);
        status(&fixture.exchange(&get("/photos/a$b", "raw-path"))?, 403);

        status(
            &fixture.exchange(&get("/photos/a$b", "raw-path-query"))?,
            403,
        );

        assert_eq!(fixture.observed.lock().unwrap().admitted.len(), 2);
        Ok(())
    })
}

fn form(mode: &str) -> Vec<u8> {
    let script =
        PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../tests/contract/http_sigv4.py");

    let output = python::command()
        .arg(script)
        .args(["--post", mode])
        .output()
        .unwrap();

    assert!(output.status.success(), "independent form generator failed");
    output.stdout
}

#[test]
fn post_tail_and_extra_file_errors_prevent_sink_finish() -> TestResult {
    Fixture::run(false, |fixture| {
        status(&fixture.exchange(&form("valid"))?, 204);

        for mode in [
            "truncated",
            "epilogue",
            "extra-file",
            "duplicate",
            "bad-signature",
        ] {
            let response = fixture.exchange(&form(mode))?;

            assert!(
                response.starts_with("HTTP/1.1 4"),
                "mode {mode}: {}",
                response.lines().next().unwrap_or("empty")
            );

            assert_eq!(fixture.observed.lock().unwrap().finishes, 1, "mode {mode}");
        }

        assert_eq!(fixture.observed.lock().unwrap().bytes, 7);

        assert_eq!(
            fixture.observed.lock().unwrap().admitted.first(),
            Some(&("photos".into(), "album/a.txt".into()))
        );

        Ok(())
    })
}

#[test]
fn post_response_preferences_are_settled_before_finish() -> TestResult {
    Fixture::run(false, |fixture| {
        let response = fixture.exchange(&form("201"))?;
        status(&response, 201);
        assert!(response.contains("<Location>http://127.0.0.1:"));
        assert!(response.contains("/photos/album/a.txt</Location>"));
        assert!(response.contains("<ETag>&quot;321c3cf486ed509164edec1e1981fec8&quot;</ETag>"));
        assert!(response.contains("etag: \"321c3cf486ed509164edec1e1981fec8\""));

        Arc::get_mut(&mut fixture.service).unwrap().public_endpoint =
            Some(ResponseOrigin::parse("https://uploads.example.invalid/")?);

        let response = fixture.exchange(&form("201-encoded-key"))?;
        status(&response, 201);
        assert!(response.contains("<Location>https://uploads.example.invalid/photos/album/../%25%20%2B%3F%23%26%E9%9B%AA//a.txt</Location>"));
        assert!(response.contains("<Key>album/../% +?#&amp;雪//a.txt</Key>"));

        for mode in ["bad-status", "bad-redirect"] {
            report_case(format_args!("mode={mode:?}"));

            let response = fixture.exchange(&form(mode))?;
            status(&response, 204);
            assert!(response.ends_with("\r\n\r\n"));
        }

        status(&fixture.exchange(&form("bad-redirect-201"))?, 201);
        let response = fixture.exchange(&form("redirect"))?;
        status(&response, 303);
        assert!(response.contains("location: https://example.invalid/done?existing=1&bucket=photos&key=album%2Fa.txt&etag=%22321c3cf486ed509164edec1e1981fec8%22#done"));

        let before = fixture.observed.lock().unwrap().finishes;

        for mode in ["redirect-alias", "redirect-control", "redirect-truncated"] {
            report_case(format_args!("mode={mode:?}"));

            status(&fixture.exchange(&form(mode))?, 400);
            assert_eq!(fixture.observed.lock().unwrap().finishes, before, "{mode}");
        }

        Ok(())
    })
}

#[test]
fn post_ignored_tail_is_bounded_and_validated_before_finish() -> TestResult {
    Fixture::run(false, |fixture| {
        for mode in [
            "ignored-tail",
            "submit-tail",
            "envelope-at-limit",
            "range-at-limit",
        ] {
            report_case(format_args!("mode={mode:?}"));

            status(&fixture.exchange(&form(mode))?, 204);
        }

        for mode in [
            "envelope-over-limit",
            "tail-over-limit",
            "tail-security-field",
            "tail-truncated",
            "range-too-short",
            "range-too-long",
            "range-disjoint",
        ] {
            let response = fixture.exchange(&form(mode))?;
            assert!(response.starts_with("HTTP/1.1 4"), "mode {mode}");
            assert_eq!(fixture.observed.lock().unwrap().finishes, 4, "mode {mode}");
        }

        assert_eq!(fixture.observed.lock().unwrap().bytes, 28);
        Ok(())
    })
}

#[test]
fn post_checksums_cover_only_file_bytes_and_reject_corruption() -> TestResult {
    Fixture::run(false, |fixture| {
        status(&fixture.exchange(&form("checksums"))?, 204);

        for mode in ["bad-form-md5", "bad-form-crc32"] {
            report_case(format_args!("mode={mode:?}"));

            let response = fixture.exchange(&form(mode))?;
            status(&response, 400);
            assert!(response.contains("<Code>BadDigest</Code>"), "{mode}");
            assert_eq!(fixture.observed.lock().unwrap().finishes, 1);
        }

        assert_eq!(fixture.observed.lock().unwrap().bytes, 7);
        Ok(())
    })
}

#[derive(Default)]
struct TraceCounts {
    events: AtomicUsize,
    spans: AtomicUsize,
    records: AtomicUsize,
}

struct Observer(Arc<TraceCounts>);

impl tracing::Subscriber for Observer {
    fn enabled(&self, _: &tracing::Metadata<'_>) -> bool {
        true
    }

    fn new_span(&self, _: &tracing::span::Attributes<'_>) -> tracing::span::Id {
        self.0.spans.fetch_add(1, Ordering::Relaxed);
        tracing::span::Id::from_u64(1)
    }

    fn record(&self, _: &tracing::span::Id, _: &tracing::span::Record<'_>) {
        self.0.records.fetch_add(1, Ordering::Relaxed);
    }

    fn record_follows_from(&self, _: &tracing::span::Id, _: &tracing::span::Id) {}

    fn event(&self, _: &tracing::Event<'_>) {
        self.0.events.fetch_add(1, Ordering::Relaxed);
    }

    fn enter(&self, _: &tracing::span::Id) {}

    fn exit(&self, _: &tracing::span::Id) {}
}

#[test]
fn upstream_protocol_events_do_not_reach_or_replace_the_host_subscriber() -> TestResult {
    let counts = Arc::new(TraceCounts::default());

    tracing::subscriber::with_default(Observer(Arc::clone(&counts)), || -> TestResult {
        let span = tracing::info_span!("host observer", value = tracing::field::Empty);
        span.record("value", "host field before request");
        tracing::error!("host observer before request");
        assert_eq!(counts.events.load(Ordering::Relaxed), 1);
        assert_eq!(counts.spans.load(Ordering::Relaxed), 1);
        assert_eq!(counts.records.load(Ordering::Relaxed), 1);

        Fixture::run(false, |fixture| {
            status(&fixture.exchange(&form("valid"))?, 204);
            status(&fixture.exchange(&form("bad-signature"))?, 403);

            for mode in ["header", "query"] {
                for (key, secret, expected) in [
                    (APP_KEY, SECRET, 200),
                    ("probe-bootstrap", SECRET, 200),
                    (APP_KEY, "wrong-secret", 403),
                    ("probe-bootstrap", "wrong-secret", 403),
                ] {
                    report_case(format_args!(
                        "mode={mode:?}, key={key:?}, expected={expected:?}"
                    ));

                    let request = signed(
                        "GET",
                        "/photos/key",
                        mode,
                        key,
                        secret,
                        "us-east-1",
                        "s3",
                        "",
                    );

                    status(&fixture.exchange(&request)?, expected);
                }
            }

            fixture.bootstrap.close();

            fixture
                .database
                .execute("UPDATE principals SET is_enabled=0", [])?;

            for mode in ["header", "query"] {
                for key in [APP_KEY, "probe-bootstrap", "unknown-key"] {
                    report_case(format_args!("mode={mode:?}, key={key:?}"));

                    let request = signed(
                        "GET",
                        "/photos/key",
                        mode,
                        key,
                        SECRET,
                        "us-east-1",
                        "s3",
                        "",
                    );

                    status(&fixture.exchange(&request)?, 403);
                }
            }

            assert_eq!(fixture.observed.lock().unwrap().admitted.len(), 5);
            Ok(())
        })?;

        assert_eq!(counts.events.load(Ordering::Relaxed), 1);
        assert_eq!(counts.spans.load(Ordering::Relaxed), 1);
        assert_eq!(counts.records.load(Ordering::Relaxed), 1);

        span.record("value", "host field after request");
        tracing::error!("host observer after request");
        assert_eq!(counts.events.load(Ordering::Relaxed), 2);
        assert_eq!(counts.records.load(Ordering::Relaxed), 2);
        Ok(())
    })
}
