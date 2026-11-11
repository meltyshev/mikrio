#[cfg(unix)]
use std::io::{Read, Write};
use std::net::TcpListener;
#[cfg(unix)]
use std::net::TcpStream;
#[cfg(unix)]
use std::time::{Duration, SystemTime, UNIX_EPOCH};

#[cfg(unix)]
use hmac::{Hmac, KeyInit, Mac};
#[cfg(unix)]
use sha2::{Digest, Sha256};

use super::*;

#[test]
#[cfg(unix)]
fn listener_accept_failure_reports_error_and_closes_without_a_signal() -> TestResult {
    let directory = TestDirectory::new()?;
    let root = directory.path();
    success(&run(root, &["init", "store", "--json"])?)?;

    let child = Process::spawn(support::descriptor_limited_command(
        root,
        &["serve", "store", "--json"],
    ))?;

    let ready = child.ready()?;
    let endpoint = field(&ready, "endpoint")?;
    let mut sockets = Vec::new();

    // Idle transports retain descriptors below the listener's 64-task limit.
    // Exhaust only the child's soft limit so accept fails while the host and
    // test observer retain enough descriptors to settle and inspect the result.
    for _ in 0..32 {
        match connect(endpoint) {
            Ok(socket) => sockets.push(socket),
            Err(_) => break,
        }
    }

    let output = child.finish()?;
    assert_eq!(output.status.code(), Some(1));
    assert_eq!(support::one_line(&output.stdout)?, ready);
    let failure = support::one_line(&output.stderr)?;
    assert_eq!(failure["event"], "error");
    assert_eq!(failure["command"], "serve");
    assert_eq!(failure["phase"], "operation");
    assert_eq!(failure["code"], "ShutdownFailed");
    assert_eq!(failure["message"], "ShutdownFailed: store shutdown failed");
    assert!(failure.get("mutationOutcome").is_none());
    drop(sockets);

    success(&run(root, &["principal", "list", "store", "--json"])?)?;
    directory.finish()?;
    Ok(())
}

#[test]
#[cfg(unix)]
fn human_readiness_uses_stderr_and_only_discloses_generated_credentials() -> TestResult {
    let directory = TestDirectory::new()?;

    for supplied in [false, true] {
        let mut invocation = command(directory.path(), &["serve", "store"]);

        if supplied {
            invocation
                .env("mikrio_access_key_id", "supplied-owner-id")
                .env("mikrio_secret_access_key", "supplied-owner-secret");
        }

        let child = Process::spawn(invocation)?;

        assert!(
            child
                .diagnostic()?
                .starts_with("Endpoint: http://127.0.0.1:")
        );

        assert_eq!(child.diagnostic()?, "Region: us-east-1\n");

        if !supplied {
            assert!(child.diagnostic()?.starts_with("Access key ID: mikrio_b"));
            assert!(child.diagnostic()?.starts_with("Secret access key: "));
        }

        child.signal("-INT")?;
        let output = child.finish()?;
        assert_eq!(output.status.code(), Some(0));
        assert_eq!(output.stdout, Vec::<u8>::new());
        let diagnostics = String::from_utf8(output.stderr)?;
        assert!(!diagnostics.contains("supplied-owner"));
        assert_eq!(diagnostics.lines().count(), if supplied { 2 } else { 4 });
    }

    directory.finish()?;
    Ok(())
}

#[test]
fn busy_port_failure_preserves_committed_bootstrap_without_readiness() -> TestResult {
    let directory = TestDirectory::new()?;
    let occupied = TcpListener::bind("127.0.0.1:0")?;
    let port = occupied.local_addr()?.port().to_string();

    let output = run(
        directory.path(),
        &[
            "serve",
            "store",
            "--bucket=first",
            "--bucket=second",
            "--port",
            &port,
            "--json",
        ],
    )?;

    error(&output, 1, "open", "IoError")?;

    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()?;

    test_stores::run(&runtime, async |stores| {
        let store = stores
            .open(mikrio::OpenOptions::new(directory.path().join("store")).create_if_missing(false))
            .await?;

        for bucket in ["first", "second"] {
            store
                .head_bucket(mikrio::HeadBucketInput::builder(bucket).build()?)
                .await?;
        }

        Ok(())
    })?;

    directory.finish()?;
    Ok(())
}

#[cfg(unix)]
#[test]
fn listener_configuration_precedence_and_supplied_secret_redaction() -> TestResult {
    let directory = TestDirectory::new()?;
    let mut invocation = command(
        directory.path(),
        &[
            "serve",
            "store",
            "--host=127.0.0.1",
            "--port=0",
            "--region=eu-west-1",
            "--public-endpoint=https://storage.example.test",
            "--json",
        ],
    );

    invocation
        .env("mikrio_host", "INVALID")
        .env("mikrio_port", "INVALID")
        .env("mikrio_region", "INVALID")
        .env("mikrio_public_endpoint", "INVALID")
        .env("mikrio_access_key_id", "supplied-owner")
        .env("mikrio_secret_access_key", "sensitive-owner-secret")
        .env("AWS_ACCESS_KEY_ID", "unrelated")
        .env("AWS_SECRET_ACCESS_KEY", "aws-secret");

    let child = Process::spawn(invocation)?;
    let ready = child.ready()?;
    assert_eq!(ready.as_object().ok_or("ready object")?.len(), 4);
    assert_eq!(ready["region"], "eu-west-1");
    assert!(field(&ready, "endpoint")?.starts_with("http://127.0.0.1:"));
    assert!(ready.get("credentials").is_none());
    child.signal("-INT")?;
    let output = child.finish()?;
    assert_eq!(output.status.code(), Some(0));
    assert_eq!(output.stderr, Vec::<u8>::new());
    assert!(!String::from_utf8(output.stdout)?.contains("sensitive-owner-secret"));
    directory.finish()?;
    Ok(())
}

#[cfg(unix)]
#[test]
fn live_policy_key_rotation_and_restart_use_current_cross_process_authority() -> TestResult {
    let directory = TestDirectory::new()?;
    let root = directory.path();

    let initialized = success(&run(
        root,
        &["init", "store", "--bucket=uploads", "--json"],
    )?)?;

    let principal = success(&run(
        root,
        &["principal", "create", "store", "--name=app", "--json"],
    )?)?;

    let principal_id = field(&principal, "principalId")?;

    let first_key = success(&run(
        root,
        &[
            "access-key",
            "create",
            "store",
            "--principal-id",
            principal_id,
            "--json",
        ],
    )?)?;

    let first = key_pair(&first_key)?;
    let child = Process::spawn(command(root, &["serve", "store", "--json"]))?;
    let ready = child.ready()?;
    let endpoint = field(&ready, "endpoint")?;

    let owner = (
        field(&ready["credentials"], "accessKeyId")?,
        field(&ready["credentials"], "secretAccessKey")?,
    );

    assert_status(
        &request(
            endpoint,
            "PUT",
            "/uploads/allowed-value",
            owner,
            b"original",
        )?,
        200,
    );

    assert_status(
        &request(endpoint, "GET", "/uploads/allowed-value", first, b"")?,
        403,
    );

    fs::write(
        root.join("identity.json"),
        super::administration::IDENTITY_POLICY,
    )?;

    success(&run(
        root,
        &[
            "policy",
            "apply",
            "store",
            "--principal-id",
            principal_id,
            "--file=identity.json",
            "--json",
        ],
    )?)?;

    let allowed = request(endpoint, "GET", "/uploads/allowed-value", first, b"")?;
    assert_status(&allowed, 200);
    assert!(allowed.ends_with(b"original"));

    assert_status(
        &request(endpoint, "PUT", "/uploads/denied-value", first, b"denied")?,
        403,
    );

    success(&run(
        root,
        &[
            "principal",
            "disable",
            "store",
            "--principal-id",
            principal_id,
            "--json",
        ],
    )?)?;

    assert_status(
        &request(endpoint, "GET", "/uploads/allowed-value", first, b"")?,
        403,
    );

    success(&run(
        root,
        &[
            "principal",
            "enable",
            "store",
            "--principal-id",
            principal_id,
            "--json",
        ],
    )?)?;

    assert_status(
        &request(endpoint, "GET", "/uploads/allowed-value", first, b"")?,
        200,
    );

    let deny = r#"{"Version":"2012-10-17","Statement":{"Effect":"Deny","Principal":"*","Action":"s3:*","Resource":["arn:aws:s3:::uploads","arn:aws:s3:::uploads/*"]}}"#;
    fs::write(root.join("bucket.json"), deny)?;

    success(&run(
        root,
        &[
            "policy",
            "apply",
            "store",
            "--bucket=uploads",
            "--file=bucket.json",
            "--json",
        ],
    )?)?;

    assert_eq!(
        success(&run(
            root,
            &["init", "store", "--bucket=uploads", "--json"]
        )?)?,
        initialized
    );

    assert_status(
        &request(endpoint, "GET", "/uploads/allowed-value", owner, b"")?,
        403,
    );

    assert_status(
        &request(endpoint, "GET", "/uploads/allowed-value", first, b"")?,
        403,
    );

    assert_eq!(
        run(root, &["policy", "get", "store", "--bucket=uploads"])?.stdout,
        deny.as_bytes()
    );

    success(&run(
        root,
        &["policy", "delete", "store", "--bucket=uploads", "--json"],
    )?)?;

    assert_status(
        &request(endpoint, "GET", "/uploads/allowed-value", first, b"")?,
        200,
    );

    let second_key = success(&run(
        root,
        &[
            "access-key",
            "create",
            "store",
            "--principal-id",
            principal_id,
            "--json",
        ],
    )?)?;

    let second = key_pair(&second_key)?;

    assert_status(
        &request(endpoint, "GET", "/uploads/allowed-value", second, b"")?,
        200,
    );

    success(&run(
        root,
        &[
            "access-key",
            "revoke",
            "store",
            "--principal-id",
            principal_id,
            "--access-key-id",
            first.0,
            "--json",
        ],
    )?)?;

    assert_status(
        &request(endpoint, "GET", "/uploads/allowed-value", first, b"")?,
        403,
    );

    assert_status(
        &request(endpoint, "GET", "/uploads/allowed-value", second, b"")?,
        200,
    );

    child.signal("-TERM")?;
    let stopped = child.finish()?;
    assert_eq!(stopped.status.code(), Some(0));
    assert_eq!(stopped.stderr, Vec::<u8>::new());
    support::one_line(&stopped.stdout)?;

    let child = Process::spawn(command(
        root,
        &["serve", "store", "--bucket=uploads", "--json"],
    ))?;

    let restarted = child.ready()?;
    assert_ne!(ready["credentials"], restarted["credentials"]);
    let endpoint = field(&restarted, "endpoint")?;

    assert_status(
        &request(endpoint, "GET", "/uploads/allowed-value", second, b"")?,
        200,
    );

    success(&run(
        root,
        &[
            "access-key",
            "revoke",
            "store",
            "--principal-id",
            principal_id,
            "--access-key-id",
            second.0,
            "--json",
        ],
    )?)?;

    success(&run(
        root,
        &[
            "principal",
            "delete",
            "store",
            "--principal-id",
            principal_id,
            "--json",
        ],
    )?)?;

    assert_status(
        &request(endpoint, "GET", "/uploads/allowed-value", second, b"")?,
        403,
    );

    child.signal("-INT")?;
    assert_eq!(child.finish()?.status.code(), Some(0));
    directory.finish()?;
    Ok(())
}

#[cfg(unix)]
#[test]
fn signal_during_an_admitted_upload_preserves_the_previous_object() -> TestResult {
    let directory = TestDirectory::new()?;
    let root = directory.path();

    let child = Process::spawn(command(
        root,
        &["serve", "store", "--bucket=uploads", "--json"],
    ))?;

    let ready = child.ready()?;
    let endpoint = field(&ready, "endpoint")?;

    let owner = (
        field(&ready["credentials"], "accessKeyId")?,
        field(&ready["credentials"], "secretAccessKey")?,
    );

    assert_status(
        &request(endpoint, "PUT", "/uploads/value", owner, b"predecessor")?,
        200,
    );

    let body = vec![b'x'; 64 * 1024];
    let mut stream = connect(endpoint)?;
    let headers = headers(endpoint, "PUT", "/uploads/value", owner, &body, true)?;
    stream.write_all(headers.as_bytes())?;
    let mut accepted = [0; 25];
    stream.read_exact(&mut accepted)?;
    assert_eq!(&accepted, b"HTTP/1.1 100 Continue\r\n\r\n");
    stream.write_all(&body[..1])?;
    child.signal("-TERM")?;
    let output = child.finish()?;
    assert_eq!(output.status.code(), Some(0));
    drop(stream);

    let child = Process::spawn(command(root, &["serve", "store", "--json"]))?;
    let ready = child.ready()?;

    let owner = (
        field(&ready["credentials"], "accessKeyId")?,
        field(&ready["credentials"], "secretAccessKey")?,
    );

    let readback = request(
        field(&ready, "endpoint")?,
        "GET",
        "/uploads/value",
        owner,
        b"",
    )?;

    assert_status(&readback, 200);
    assert!(readback.ends_with(b"predecessor"));
    child.signal("-TERM")?;
    assert_eq!(child.finish()?.status.code(), Some(0));
    directory.finish()?;
    Ok(())
}

#[cfg(unix)]
fn key_pair(value: &Value) -> TestResult<(&str, &str)> {
    Ok((
        field(&value["accessKey"], "accessKeyId")?,
        field(value, "secretAccessKey")?,
    ))
}

#[cfg(unix)]
fn assert_status(response: &[u8], status: u16) {
    assert!(
        response.starts_with(format!("HTTP/1.1 {status} ").as_bytes()),
        "unexpected HTTP status: {}",
        String::from_utf8_lossy(
            response
                .split(|byte| *byte == b'\r')
                .next()
                .unwrap_or_default()
        )
    );
}

#[cfg(unix)]
fn connect(endpoint: &str) -> TestResult<TcpStream> {
    let stream = TcpStream::connect(
        endpoint
            .strip_prefix("http://")
            .ok_or("HTTP endpoint required")?,
    )?;

    stream.set_read_timeout(Some(Duration::from_secs(10)))?;
    stream.set_write_timeout(Some(Duration::from_secs(10)))?;
    Ok(stream)
}

#[cfg(unix)]
fn request(
    endpoint: &str,
    method: &str,
    path: &str,
    credentials: (&str, &str),
    body: &[u8],
) -> TestResult<Vec<u8>> {
    let mut stream = connect(endpoint)?;
    stream.write_all(headers(endpoint, method, path, credentials, body, false)?.as_bytes())?;
    stream.write_all(body)?;
    let mut response = Vec::new();
    stream.take(1024 * 1024).read_to_end(&mut response)?;
    Ok(response)
}

#[cfg(unix)]
fn headers(
    endpoint: &str,
    method: &str,
    path: &str,
    credentials: (&str, &str),
    body: &[u8],
    expect_continue: bool,
) -> TestResult<String> {
    let host = endpoint
        .strip_prefix("http://")
        .ok_or("HTTP endpoint required")?;

    let now = time::OffsetDateTime::from_unix_timestamp(i64::try_from(
        SystemTime::now().duration_since(UNIX_EPOCH)?.as_secs(),
    )?)?;

    let date = format!(
        "{:04}{:02}{:02}",
        now.year(),
        u8::from(now.month()),
        now.day()
    );

    let instant = format!(
        "{date}T{:02}{:02}{:02}Z",
        now.hour(),
        now.minute(),
        now.second()
    );

    let payload_hash = hex(&Sha256::digest(body));

    let canonical = format!(
        "{method}\n{path}\n\nhost:{host}\nx-amz-content-sha256:{payload_hash}\nx-amz-date:{instant}\n\nhost;x-amz-content-sha256;x-amz-date\n{payload_hash}"
    );

    let scope = format!("{date}/us-east-1/s3/aws4_request");

    let to_sign = format!(
        "AWS4-HMAC-SHA256\n{instant}\n{scope}\n{}",
        hex(&Sha256::digest(canonical.as_bytes()))
    );

    let key = mac(format!("AWS4{}", credentials.1).as_bytes(), date.as_bytes())?;
    let key = mac(&key, b"us-east-1")?;
    let key = mac(&key, b"s3")?;
    let key = mac(&key, b"aws4_request")?;
    let signature = hex(&mac(&key, to_sign.as_bytes())?);

    let expect = if expect_continue {
        "Expect: 100-continue\r\n"
    } else {
        ""
    };

    Ok(format!(
        "{method} {path} HTTP/1.1\r\nHost: {host}\r\nx-amz-content-sha256: {payload_hash}\r\nx-amz-date: {instant}\r\nAuthorization: AWS4-HMAC-SHA256 Credential={}/{scope}, SignedHeaders=host;x-amz-content-sha256;x-amz-date, Signature={signature}\r\nContent-Length: {}\r\nConnection: close\r\n{expect}\r\n",
        credentials.0,
        body.len()
    ))
}

#[cfg(unix)]
fn mac(key: &[u8], message: &[u8]) -> TestResult<Vec<u8>> {
    let mut mac = Hmac::<Sha256>::new_from_slice(key)?;
    mac.update(message);
    Ok(mac.finalize().into_bytes().to_vec())
}

#[cfg(unix)]
fn hex(bytes: &[u8]) -> String {
    const DIGITS: &[u8; 16] = b"0123456789abcdef";
    let mut text = String::with_capacity(bytes.len() * 2);

    for byte in bytes {
        text.push(char::from(DIGITS[usize::from(byte >> 4)]));
        text.push(char::from(DIGITS[usize::from(byte & 15)]));
    }

    text
}
