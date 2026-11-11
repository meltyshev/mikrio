#![cfg(feature = "http")]

use std::path::PathBuf;
use std::process::{Child, Stdio};
use std::sync::Arc;
use std::time::Duration;

use mikrio::{
    CreateAccessKeyInput, CreateBucketInput, CreatePrincipalInput, Credentials,
    DeleteAccessKeyInput, ErrorCode, GetObjectInput, HeadBucketInput, ListenOptions, OpenOptions,
    PutPrincipalPolicyInput, Server,
};
use mikrio_core::{DurableDirectory, sync_file};
use tokio::io::{AsyncReadExt, AsyncWriteExt};

#[path = "../../mikrio-core/tests/support/python.rs"]
mod python;

#[path = "support/stores.rs"]
mod test_stores;

#[path = "../../mikrio-core/tests/support/directory.rs"]
mod test_directory;

use test_directory::TestDirectory;

#[path = "../../mikrio-core/tests/support/backup.rs"]
mod test_backup;
#[path = "../../mikrio-core/tests/support/process.rs"]
mod test_process;

#[path = "listener/buckets.rs"]
mod buckets;
#[path = "listener/configuration.rs"]
mod configuration;
#[path = "listener/disclosure.rs"]
mod disclosure;
#[path = "listener/namespace.rs"]
mod namespace;
#[path = "listener/reauthorization.rs"]
mod reauthorization;
#[path = "listener/recovery.rs"]
mod recovery;

type TestResult = Result<(), Box<dyn std::error::Error>>;

#[test]
fn concurrent_listens_publish_one_owner_and_dropping_it_releases_the_slot() -> TestResult {
    let directory = TestDirectory::new()?;

    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()?;

    test_stores::run(&runtime, async |stores| {
        let store = Arc::new(
            stores
                .open(OpenOptions::new(directory.path().join("store")))
                .await?,
        );

        let start = Arc::new(tokio::sync::Barrier::new(9));
        let mut attempts = tokio::task::JoinSet::new();

        for _ in 0..8 {
            let store = Arc::clone(&store);
            let start = Arc::clone(&start);

            attempts.spawn(async move {
                start.wait().await;
                store.listen(ListenOptions::default()).await
            });
        }

        start.wait().await;
        let mut winner = None;
        let mut rejected = 0;

        while let Some(result) = attempts.join_next().await {
            match result? {
                Ok(server) => assert!(winner.replace(server).is_none()),
                Err(error) => {
                    assert_eq!(error.code(), ErrorCode::ListenerAlreadyRunning);
                    rejected += 1;
                }
            }
        }

        assert_eq!(rejected, 7);
        let server = winner.ok_or("no listener was published")?;
        let old_endpoint = server.endpoint().to_owned();

        let address: std::net::SocketAddr =
            old_endpoint.strip_prefix("http://").unwrap().parse()?;

        drop(server);

        let replacement = tokio::time::timeout(Duration::from_secs(10), async {
            loop {
                match store
                    .listen(ListenOptions::default().port(address.port()))
                    .await
                {
                    Ok(server) => break Ok(server),
                    Err(error) if error.code() == ErrorCode::ListenerAlreadyRunning => {
                        tokio::task::yield_now().await;
                    }
                    Err(error) => break Err(error),
                }
            }
        })
        .await??;

        assert_eq!(replacement.endpoint(), old_endpoint);
        replacement.close().await?;

        store
            .create_bucket(CreateBucketInput::builder("direct-after-drop").build()?)
            .await?;

        store.close().await?;
        Ok::<_, Box<dyn std::error::Error>>(())
    })?;

    directory.finish()?;
    Ok(())
}

fn signed(method: &str, path: &str, key: &str, secret: &str, body: &str) -> Vec<u8> {
    signed_options(method, path, key, secret, body, "us-east-1", &[])
}

fn signed_options(
    method: &str,
    path: &str,
    key: &str,
    secret: &str,
    body: &str,
    region: &str,
    headers: &[(&str, &str)],
) -> Vec<u8> {
    signed_request(method, path, (key, secret), body, region, headers, "header")
}

struct RequestSigner {
    child: Child,
}

impl RequestSigner {
    fn output(&mut self) -> std::io::Result<Vec<u8>> {
        use std::io::Read as _;

        let mut output = Vec::new();

        self.child
            .stdout
            .take()
            .ok_or_else(|| std::io::Error::other("missing signer output pipe"))?
            .read_to_end(&mut output)?;

        if !self.child.wait()?.success() {
            return Err(std::io::Error::other("independent request signer failed"));
        }

        Ok(output)
    }

    fn stop(&mut self) -> std::io::Result<()> {
        if self.child.try_wait()?.is_some() {
            return Ok(());
        }

        self.child.kill()?;
        self.child.wait()?;
        Ok(())
    }
}

impl Drop for RequestSigner {
    #[allow(
        clippy::print_stderr,
        reason = "signer cleanup failures must remain visible beside the original failure"
    )]
    fn drop(&mut self) {
        if let Err(error) = self.stop() {
            eprintln!("request signer cleanup failed: {error}");
        }
    }
}

#[test]
fn request_signer_can_be_reaped_after_output_setup_fails() -> TestResult {
    let mut signer = RequestSigner {
        child: python::command()
            .args(["-c", "import sys; sys.stdin.buffer.read()"])
            .stdin(Stdio::piped())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()?,
    };

    assert_eq!(
        signer.output().unwrap_err().kind(),
        std::io::ErrorKind::Other
    );

    signer.stop()?;
    assert!(signer.child.try_wait()?.is_some());
    Ok(())
}

#[allow(clippy::expect_used, reason = "test request producer invariants")]
fn signed_request(
    method: &str,
    path: &str,
    credentials: (&str, &str),
    body: &str,
    region: &str,
    headers: &[(&str, &str)],
    mode: &str,
) -> Vec<u8> {
    let (key, secret) = credentials;
    let producer = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../tests/contract");
    let mut command = python::command();

    command
        .arg("-c")
        .arg(
            r#"
import sys
from urllib.parse import parse_qsl

sys.path.insert(0, sys.argv[1])
from http_sigv4 import request

values = []

while length := sys.stdin.buffer.readline():
    values.append(sys.stdin.buffer.read(int(length)).decode())

method, target, key, secret, body, region, mode, *headers = values
path, _, query = target.partition("?")

signed = request(
    method,
    path,
    mode,
    key,
    secret,
    region,
    "s3",
    body.encode(),
    dict(zip(headers[::2], headers[1::2])),
    dict(parse_qsl(query, keep_blank_values=True)),
)

sys.stdout.buffer.write(signed)
"#,
        )
        .arg(producer)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::null());

    let mut signer = RequestSigner {
        child: command.spawn().expect("independent request signer"),
    };

    {
        use std::io::Write as _;

        let mut input = signer.child.stdin.take().expect("owned signer pipe");

        // Keep generated credentials and policy text out of process arguments.
        for value in [method, path, key, secret, body, region, mode]
            .into_iter()
            .chain(headers.iter().flat_map(|(name, value)| [*name, *value]))
        {
            writeln!(input, "{}", value.len()).expect("signer field length");

            input
                .write_all(value.as_bytes())
                .expect("signer field bytes");
        }
    }

    signer.output().expect("independent request signer result")
}

async fn exchange(server: &Server, request: &[u8]) -> Result<String, Box<dyn std::error::Error>> {
    exchange_endpoint(server.endpoint(), request).await
}

#[allow(clippy::expect_used, reason = "test listener endpoint invariant")]
async fn exchange_endpoint(
    endpoint: &str,
    request: &[u8],
) -> Result<String, Box<dyn std::error::Error>> {
    let mut socket =
        tokio::net::TcpStream::connect(endpoint.strip_prefix("http://").expect("HTTP endpoint"))
            .await?;

    socket.write_all(request).await?;
    let mut response = String::new();

    tokio::time::timeout(
        Duration::from_secs(10),
        socket.read_to_string(&mut response),
    )
    .await??;

    Ok(response)
}

#[track_caller]
fn status(response: &str, expected: u16) {
    assert_eq!(
        response.split_whitespace().nth(1),
        Some(expected.to_string().as_str()),
        "{response}"
    );
}

#[test]
fn provision_two_listeners_revoke_read_relisten_and_preserve_direct_access() -> TestResult {
    let directory = TestDirectory::new()?;

    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()?;

    test_stores::run(&runtime, async |stores| {
        let path = directory.path().join("store");
        let store = stores.open(OpenOptions::new(&path)).await?;
        let peer = stores.open(OpenOptions::new(&path)).await?;

        store
            .create_bucket(CreateBucketInput::builder("photos").build()?)
            .await?;

        let principal = store
            .create_principal(CreatePrincipalInput::builder("app").build()?)
            .await?;

        let policy = r#"{
            "Version": "2012-10-17",
            "Statement": {
                "Effect": "Allow",
                "Action": ["s3:GetObject", "s3:PutObject"],
                "Resource": "arn:aws:s3:::photos/allowed/*"
            }
        }"#;

        store
            .put_principal_policy(
                PutPrincipalPolicyInput::builder(principal.principal_id(), policy).build()?,
            )
            .await?;

        let key = store
            .create_access_key(CreateAccessKeyInput::builder(principal.principal_id()).build()?)
            .await?;

        let first = store.listen(ListenOptions::default()).await?;
        let second = peer.listen(ListenOptions::default()).await?;
        assert_ne!(first.endpoint(), second.endpoint());

        assert_ne!(
            first.credentials().access_key_id(),
            second.credentials().access_key_id()
        );

        let write = signed(
            "PUT",
            "/photos/allowed/object",
            key.access_key().access_key_id(),
            key.secret_access_key(),
            "durable listener bytes",
        );

        status(&exchange(&first, &write).await?, 200);

        status(
            &exchange(
                &second,
                &signed(
                    "PUT",
                    "/photos/denied",
                    key.access_key().access_key_id(),
                    key.secret_access_key(),
                    "denied",
                ),
            )
            .await?,
            403,
        );

        let read = signed(
            "GET",
            "/photos/allowed/object",
            key.access_key().access_key_id(),
            key.secret_access_key(),
            "",
        );

        let response = exchange(&second, &read).await?;
        status(&response, 200);

        assert_eq!(
            response.split_once("\r\n\r\n").unwrap().1,
            "durable listener bytes"
        );

        let bootstrap = signed(
            "GET",
            "/photos/allowed/object",
            first.credentials().access_key_id(),
            first.credentials().secret_access_key(),
            "",
        );

        status(&exchange(&first, &bootstrap).await?, 200);
        status(&exchange(&second, &bootstrap).await?, 403);

        peer.delete_access_key(
            DeleteAccessKeyInput::builder(
                principal.principal_id(),
                key.access_key().access_key_id(),
            )
            .build()?,
        )
        .await?;

        status(&exchange(&first, &read).await?, 403);
        status(&exchange(&second, &read).await?, 403);

        first.close().await?;
        first.close().await?;

        let mut direct = store
            .get_object(GetObjectInput::builder("photos", "allowed/object").build()?)
            .await?;

        assert_eq!(
            direct.body_mut().read_all(100).await?,
            b"durable listener bytes"
        );

        let next = store.listen(ListenOptions::default()).await?;

        assert_ne!(
            first.credentials().access_key_id(),
            next.credentials().access_key_id()
        );

        status(&exchange(&next, &bootstrap).await?, 403);
        store.close().await?;
        next.close().await?;

        status(
            &exchange(
                &second,
                &signed(
                    "GET",
                    "/photos/allowed/object",
                    second.credentials().access_key_id(),
                    second.credentials().secret_access_key(),
                    "",
                ),
            )
            .await?,
            200,
        );

        peer.close().await?;
        second.close().await?;

        let reopened = stores.open(OpenOptions::new(path)).await?;
        let server = reopened.listen(ListenOptions::default()).await?;
        status(&exchange(&server, &read).await?, 403);
        reopened.close().await?;
        server.close().await?;
        Ok::<_, Box<dyn std::error::Error>>(())
    })?;

    directory.finish()?;
    Ok(())
}

#[test]
fn listener_validation_failed_bind_single_slot_and_snapshots_preserve_store() -> TestResult {
    let directory = TestDirectory::new()?;

    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()?;

    test_stores::run(&runtime, async |stores| {
        let store = stores
            .open(OpenOptions::new(directory.path().join("store")))
            .await?;

        for options in [
            ListenOptions::default().host("localhost"),
            ListenOptions::default().host(""),
            ListenOptions::default().public_endpoint("https://host/path"),
            ListenOptions::default().credentials(Credentials::new(
                format!("mikrio_a{}", "AB".repeat(16)),
                "secret",
            )?),
        ] {
            assert_eq!(
                store.listen(options).await.err().unwrap().code(),
                ErrorCode::InvalidArgument
            );
        }

        let occupied = tokio::net::TcpListener::bind("127.0.0.1:0").await?;

        assert_eq!(
            store
                .listen(ListenOptions::default().port(occupied.local_addr()?.port()))
                .await
                .err()
                .unwrap()
                .code(),
            ErrorCode::IoError
        );

        let server = store
            .listen(
                ListenOptions::default()
                    .host("0.0.0.0")
                    .public_endpoint("HTTPS://EXAMPLE.INVALID:443/"),
            )
            .await?;

        assert!(server.endpoint().starts_with("http://127.0.0.1:"));

        assert_eq!(
            store
                .listen(ListenOptions::default())
                .await
                .err()
                .unwrap()
                .code(),
            ErrorCode::ListenerAlreadyRunning
        );

        let credentials = server.credentials().clone();
        assert!(!format!("{credentials:?}").contains(credentials.secret_access_key()));
        assert_eq!(credentials.access_key_id().len(), 40);
        assert_eq!(credentials.secret_access_key().len(), 64);
        let endpoint = server.endpoint().to_owned();
        store.close().await?;
        server.close().await?;
        assert_eq!(server.endpoint(), endpoint);

        assert_eq!(
            server.credentials().access_key_id(),
            credentials.access_key_id()
        );

        assert_eq!(
            tokio::net::TcpStream::connect(endpoint.strip_prefix("http://").unwrap())
                .await
                .unwrap_err()
                .kind(),
            std::io::ErrorKind::ConnectionRefused
        );

        Ok::<_, Box<dyn std::error::Error>>(())
    })?;

    directory.finish()?;
    Ok(())
}

#[test]
fn missing_runtime_drivers_release_startup_ownership_and_preserve_direct_use() -> TestResult {
    let directory = TestDirectory::new()?.retain_on_failure();

    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()?;

    test_stores::run_sync(&runtime, |stores| {
        let store =
            runtime.block_on(stores.open(OpenOptions::new(directory.path().join("store"))))?;

        for drivers in ["none", "io-only", "time-only"] {
            let mut builder = tokio::runtime::Builder::new_current_thread();

            match drivers {
                "io-only" => {
                    builder.enable_io();
                }
                "time-only" => {
                    builder.enable_time();
                }
                _ => {}
            }

            let incomplete = builder.build()?;

            let error = incomplete
                .block_on(store.listen(ListenOptions::default()))
                .err()
                .ok_or("listener unexpectedly accepted missing runtime drivers")?;

            assert_eq!(error.code(), ErrorCode::InvalidArgument, "{drivers}");
            drop(incomplete);

            runtime.block_on(async {
                let bucket = format!("after-{drivers}");

                store
                    .create_bucket(CreateBucketInput::builder(&bucket).build()?)
                    .await?;

                let server = store.listen(ListenOptions::default()).await?;
                server.close().await?;

                store
                    .head_bucket(HeadBucketInput::builder(bucket).build()?)
                    .await?;

                Ok::<_, Box<dyn std::error::Error>>(())
            })?;
        }

        runtime.block_on(store.close())?;
        Ok(())
    })?;

    directory.finish()?;
    Ok(())
}

#[test]
fn ipv4_and_ipv6_bind_addresses_report_reachable_loopback_endpoints() -> TestResult {
    let directory = TestDirectory::new()?.retain_on_failure();

    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()?;

    test_stores::run(&runtime, async |stores| {
        let store = stores
            .open(OpenOptions::new(directory.path().join("store")))
            .await?;

        store
            .create_bucket(CreateBucketInput::builder("photos").build()?)
            .await?;

        for (host, expected_prefix) in [
            ("127.0.0.1", "http://127.0.0.1:"),
            ("0.0.0.0", "http://127.0.0.1:"),
            ("::1", "http://[::1]:"),
            ("::", "http://[::1]:"),
        ] {
            let server = store.listen(ListenOptions::default().host(host)).await?;
            assert!(server.endpoint().starts_with(expected_prefix), "{host}");

            let response = exchange(
                &server,
                b"HEAD /photos HTTP/1.1\r\nHost: localhost\r\nConnection: close\r\n\r\n",
            )
            .await?;

            status(&response, 403);
            server.close().await?;
        }

        store.close().await?;
        Ok::<_, Box<dyn std::error::Error>>(())
    })?;

    directory.finish()?;
    Ok(())
}
