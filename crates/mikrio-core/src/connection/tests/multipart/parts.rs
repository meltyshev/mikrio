use super::*;
use crate::integrity::{Checksum, ExpectedDigests};
use crate::multipart::{PART_MAX_BYTES, PartNumber, PartRequest};
use crate::object::ByteLength;
use crate::{BlobId, OperationError};
use std::fmt::Write as _;
use std::pin::Pin;
use std::task::Context;
use tokio::io::{AsyncRead, ReadBuf};

mod checksums;
mod control;
mod effects;
mod retention;

pub(super) fn part(upload_id: UploadId, number: u32) -> PartRequest {
    PartRequest {
        bucket: request().bucket,
        key: request().key,
        upload_id,
        part_number: PartNumber::new(number).unwrap(),
        expected_owner: None,
        content_length: None,
        checksum_algorithm: None,
        expected: ExpectedDigests::default(),
    }
}

fn inspect(path: &std::path::Path, expected: &[u8]) -> TestResult {
    // All connections/owned subprocesses must settle before this offline check.
    use md5::{Digest as _, Md5};
    super::super::faults::publication_references(path)?;

    let database = rusqlite::Connection::open(path.join("metadata.sqlite3"))?;

    let mut statement = database.prepare(
        "SELECT p.blob_id, b.state, b.byte_length, b.md5_digest, b.etag, b.checksum_type, b.checksums_json, b.gc_pending FROM multipart_parts AS p LEFT JOIN blobs AS b ON p.blob_id = b.blob_id",
    )?;

    let mut rows = statement.query([])?;
    let row = rows.next()?.ok_or("missing committed part")?;

    let id: [u8; 16] = row
        .get::<_, Vec<u8>>(0)?
        .try_into()
        .map_err(|_| "invalid blob id")?;

    let blob_id = BlobId::from_bytes(id);

    let payload = std::fs::read(
        path.join("objects")
            .join(format!("{:02x}", blob_id.shard()))
            .join(blob_id.to_string()),
    )?;

    assert_eq!(payload, expected);
    assert_eq!(row.get::<_, String>(1)?, "ready");
    assert_eq!(row.get::<_, i64>(2)?, i64::try_from(expected.len())?);
    assert_eq!(row.get::<_, Vec<u8>>(3)?, Md5::digest(expected).as_slice());

    let mut etag = String::from("\"");

    for byte in Md5::digest(expected) {
        write!(etag, "{byte:02x}")?;
    }

    etag.push('"');
    assert_eq!(row.get::<_, String>(4)?, etag);
    assert_eq!(row.get::<_, String>(5)?, "FULL_OBJECT");

    let checksums: std::collections::BTreeMap<String, String> =
        serde_json::from_str(&row.get::<_, String>(6)?)?;

    assert_eq!(checksums.len(), 1);
    let (algorithm, encoded) = checksums.first_key_value().unwrap();
    let algorithm = ChecksumAlgorithm::parse(algorithm)?;

    let mut verifier = crate::integrity::PayloadVerifier::new(
        None,
        PART_MAX_BYTES,
        algorithm,
        ExpectedDigests::default(),
    )?;

    verifier.update(&payload)?;

    assert_eq!(
        encoded,
        &verifier
            .finish(ExpectedDigests::default())?
            .checksum()
            .encode()
    );

    assert_eq!(row.get::<_, i64>(7)?, 0);
    assert!(rows.next()?.is_none());
    drop(rows);
    drop(statement);

    assert_eq!(
        database.query_row("SELECT count(*) FROM objects", [], |row| {
            row.get::<_, i64>(0)
        })?,
        0
    );

    database.close().map_err(|(_, error)| error)?;
    Ok(())
}

struct SourceFailure;
impl AsyncRead for SourceFailure {
    fn poll_read(
        self: Pin<&mut Self>,
        _: &mut Context<'_>,
        _: &mut ReadBuf<'_>,
    ) -> Poll<io::Result<()>> {
        Poll::Ready(Err(io::Error::other("owned part source failure")))
    }
}

fn rejected(error: &OperationError, failure: OperationFailure) {
    assert_eq!(error.failure(), failure);
    assert_eq!(error.outcome(), Some(MutationOutcome::NotCommitted));
}

#[test]
fn failed_part_sources_and_header_or_terminal_integrity_preserve_acknowledged_bytes() -> TestResult
{
    let directory = TestDirectory::new()?.retain_on_failure();
    let path = directory.path().join("store");

    runtime()?.block_on(async {
        let connection = Connection::open(path.clone(), None).await?;
        connection.pause_maintenance();
        connection.create_bucket(request().bucket).await?;
        let upload = connection.create_multipart_upload(request()).await?;
        let original = part(upload.upload_id, 10_000);

        connection
            .upload_part(original.clone(), b"original".as_slice())
            .await?;

        rejected(
            &connection
                .upload_part(original.clone(), SourceFailure)
                .await
                .unwrap_err(),
            OperationFailure::BodyReadFailed,
        );

        for length in [1, 8] {
            let input = PartRequest {
                content_length: Some(ByteLength::new(length)?),
                ..original.clone()
            };

            rejected(
                &connection
                    .upload_part(input, b"short".as_slice())
                    .await
                    .unwrap_err(),
                OperationFailure::IncompleteBody,
            );
        }

        let input = PartRequest {
            content_length: Some(ByteLength::new(PART_MAX_BYTES + 1)?),
            ..original.clone()
        };

        rejected(
            &connection
                .upload_part(input, SourceFailure)
                .await
                .unwrap_err(),
            OperationFailure::EntityTooLarge,
        );

        let input = PartRequest {
            checksum_algorithm: Some(ChecksumAlgorithm::Sha256),
            ..original.clone()
        };

        rejected(
            &connection
                .upload_part(input, SourceFailure)
                .await
                .unwrap_err(),
            OperationFailure::BadDigest,
        );

        let input = PartRequest {
            expected: ExpectedDigests::parse(Some("1B2M2Y8AsgTpgAmY7PhCfg=="), None)?,
            ..original.clone()
        };

        rejected(
            &connection
                .upload_part(input, b"different".as_slice())
                .await
                .unwrap_err(),
            OperationFailure::BadDigest,
        );

        let bootstrap = crate::access::Bootstrap::generate()?;

        let identity = Arc::new(
            bootstrap
                .resolve(bootstrap.access_key_id())
                .unwrap()
                .verified(),
        );

        rejected(
            &connection
                .upload_part_http(
                    Arc::clone(&identity),
                    original.clone(),
                    b"different".as_slice(),
                    || Ok(ExpectedDigests::parse(None, Some(Checksum::Crc64Nvme([0; 8]))).unwrap()),
                )
                .await
                .unwrap_err(),
            OperationFailure::BadDigest,
        );

        rejected(
            &connection
                .upload_part_http(identity, original.clone(), b"different".as_slice(), || {
                    Err(io::Error::other("invalid terminal framing"))
                })
                .await
                .unwrap_err(),
            OperationFailure::IoError,
        );

        connection.close().await?;
        Ok::<_, Box<dyn std::error::Error>>(())
    })?;

    inspect(&path, b"original")?;
    directory.finish()?;
    Ok(())
}

struct Gated {
    entered: Option<oneshot::Sender<()>>,
    release: oneshot::Receiver<()>,
    ready: bool,
    bytes: &'static [u8],
}

impl AsyncRead for Gated {
    fn poll_read(
        mut self: Pin<&mut Self>,
        context: &mut Context<'_>,
        buffer: &mut ReadBuf<'_>,
    ) -> Poll<io::Result<()>> {
        if let Some(entered) = self.entered.take() {
            let _delivery = entered.send(());
        }

        if !self.ready {
            match Pin::new(&mut self.release).poll(context) {
                Poll::Pending => return Poll::Pending,
                Poll::Ready(Err(_)) => {
                    return Poll::Ready(Err(io::Error::other("owned release dropped")));
                }
                Poll::Ready(Ok(())) => self.ready = true,
            }
        }

        let length = self.bytes.len().min(buffer.remaining());
        buffer.put_slice(&self.bytes[..length]);
        self.bytes = &self.bytes[length..];
        Poll::Ready(Ok(()))
    }
}

#[test]
fn in_flight_part_replacement_orders_with_competing_writer_and_close_without_aborting_upload()
-> TestResult {
    let directory = TestDirectory::new()?.retain_on_failure();

    for cancel in [false, true] {
        let path = directory
            .path()
            .join(if cancel { "closed" } else { "competing" });

        runtime()?.block_on(async {
            let connection = Connection::open(path.clone(), None).await?;
            connection.pause_maintenance();
            let peer = Connection::open(path.clone(), None).await?;
            peer.pause_maintenance();
            connection.create_bucket(request().bucket).await?;
            let upload = connection.create_multipart_upload(request()).await?;
            let input = part(upload.upload_id, 1);

            connection
                .upload_part(input.clone(), b"original".as_slice())
                .await?;

            let (entered, observed) = oneshot::channel();
            let (release, released) = oneshot::channel();
            let writer = connection.clone();
            let request = input.clone();

            let task = tokio::spawn(async move {
                writer
                    .upload_part(
                        request,
                        Gated {
                            entered: Some(entered),
                            release: released,
                            ready: false,
                            bytes: b"delayed",
                        },
                    )
                    .await
            });

            observed.await?;
            peer.upload_part(input.clone(), b"peer".as_slice()).await?;

            if cancel {
                tokio::time::timeout(Duration::from_secs(10), connection.close()).await??;

                rejected(
                    &task.await?.unwrap_err(),
                    OperationFailure::OperationCanceled,
                );
            } else {
                release.send(()).map_err(|()| "missing waiting part")?;
                task.await??;
                connection.close().await?;
            }

            peer.close().await?;
            Ok::<_, Box<dyn std::error::Error>>(())
        })?;

        inspect(&path, if cancel { b"peer" } else { b"delayed" })?;
        let upload_id = retained_ids_after_close(&path)?;

        runtime()?.block_on(async {
            let reopened = Connection::open(path.clone(), None).await?;
            reopened.pause_maintenance();

            reopened
                .upload_part(part(upload_id, 1), b"after reopen".as_slice())
                .await?;

            reopened.close().await?;
            Ok::<_, Box<dyn std::error::Error>>(())
        })?;

        inspect(&path, b"after reopen")?;
    }

    directory.finish()?;
    Ok(())
}

fn retained_ids_after_close(
    path: &std::path::Path,
) -> Result<UploadId, Box<dyn std::error::Error>> {
    let ids = retained_ids(path)?;
    assert_eq!(ids.len(), 1);
    Ok(ids[0].clone())
}

fn checksum_streams(algorithm: ChecksumAlgorithm, encoded: &str, mode: ChecksumType) -> TestResult {
    assert!(algorithm.supports_multipart(mode));
    let directory = TestDirectory::new()?.retain_on_failure();

    let path = directory.path().join(format!("{algorithm:?}-{mode:?}"));

    let upload_id = runtime()?.block_on(async {
        let connection = Connection::open(path.clone(), None).await?;
        connection.pause_maintenance();
        connection.create_bucket(request().bucket).await?;
        let mut initiation = request();
        initiation.checksum = MultipartChecksum::new(algorithm, mode)?;
        let upload = connection.create_multipart_upload(initiation).await?;
        let mut input = part(upload.upload_id.clone(), 7);
        input.content_length = Some(ByteLength::new(9)?);
        input.expected = ExpectedDigests::parse(None, Some(Checksum::parse(algorithm, encoded)?))?;

        let facts = connection
            .upload_part(input, b"123456789".as_slice())
            .await?;

        assert_eq!(facts.checksum().encode(), encoded);
        assert_eq!(facts.etag(), "\"25f9e794323b453885f5181f1b624d0b\"");
        connection.close().await?;
        Ok::<_, Box<dyn std::error::Error>>(upload.upload_id)
    })?;

    inspect(&path, b"123456789")?;

    runtime()?.block_on(async {
        let connection = Connection::open(path.clone(), None).await?;
        connection.pause_maintenance();

        let facts = connection
            .upload_part(part(upload_id.clone(), 7), b"".as_slice())
            .await?;

        assert_eq!(facts.length(), 0);
        assert_eq!(facts.etag(), "\"d41d8cd98f00b204e9800998ecf8427e\"");
        connection.close().await?;
        Ok::<_, Box<dyn std::error::Error>>(())
    })?;

    inspect(&path, b"")?;

    runtime()?.block_on(async {
        let connection = Connection::open(path.clone(), None).await?;
        connection.pause_maintenance();

        let facts = connection
            .upload_part(part(upload_id, 7), MULTICHUNK)
            .await?;

        assert_eq!(facts.length(), u64::try_from(MULTICHUNK.len())?);
        connection.close().await?;
        Ok::<_, Box<dyn std::error::Error>>(())
    })?;

    inspect(&path, MULTICHUNK)?;

    directory.finish()?;
    Ok(())
}

const MULTICHUNK: &[u8] = &[0x41; 3 * 64 * 1024 + 17];

const PART_CHILD: &str = "connection::tests::multipart::parts::part_process_child";
static FIRED: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);

#[test]
fn revocation_and_namespace_retirement_win_against_an_admitted_part_stream() -> TestResult {
    let directory = TestDirectory::new()?.retain_on_failure();

    for revoke in [false, true] {
        let path = directory
            .path()
            .join(if revoke { "revoked" } else { "retired" });

        runtime()?.block_on(async {
            let connection = Connection::open(path.clone(), None).await?;
            connection.pause_maintenance();
            let peer = Connection::open(path.clone(), None).await?;
            peer.pause_maintenance();
            connection.create_bucket(request().bucket).await?;
            let upload = connection.create_multipart_upload(request()).await?;
            let input = part(upload.upload_id, 1);

            connection
                .upload_part(input.clone(), b"original".as_slice())
                .await?;

            let principal = connection
                .create_principal(crate::access::PrincipalName::new("writer".into())?)
                .await?;

            connection.put_principal_policy(
                principal.principal_id,
                r#"{"Version":"2012-10-17","Statement":{"Effect":"Allow","Action":"s3:PutObject","Resource":"*"}}"#.into(),
            )
            .await?;

            let key = connection.create_access_key(principal.principal_id).await?;

            let identity = Arc::new(
                connection
                    .resolve_application_key(key.access_key.access_key_id.as_str().into())
                    .await?
                    .verified(),
            );

            let (entered, observed) = oneshot::channel();
            let (release, released) = oneshot::channel();
            let writer = connection.clone();

            let task = tokio::spawn(async move {
                writer
                    .upload_part_http(
                        identity,
                        input,
                        Gated {
                            entered: Some(entered),
                            release: released,
                            ready: false,
                            bytes: b"losing replacement",
                        },
                        || Ok(ExpectedDigests::default()),
                    )
                    .await
            });

            observed.await?;

            if revoke {
                peer.delete_access_key(principal.principal_id, key.access_key.access_key_id)
                    .await?;
            } else {
                peer.delete_bucket(request().bucket, None).await?;
                peer.create_bucket(request().bucket).await?;
            }

            release.send(()).map_err(|()| "missing part writer")?;

            rejected(
                &task.await?.unwrap_err(),
                if revoke {
                    OperationFailure::AccessDenied
                } else {
                    OperationFailure::NoSuchUpload
                },
            );

            connection.close().await?;
            peer.close().await?;
            Ok::<_, Box<dyn std::error::Error>>(())
        })?;

        // Maintenance is deliberately paused: the old namespace still owns its
        // acknowledged part; the losing stream cannot alter or resurrect it.
        inspect(&path, b"original")?;
    }

    directory.finish()?;
    Ok(())
}

fn announce(value: &str) -> io::Result<()> {
    let mut output = io::stdout().lock();
    writeln!(output, "PROBE {value}")?;
    output.flush()
}

fn release() -> io::Result<()> {
    let mut line = String::new();
    io::stdin().lock().read_line(&mut line)?;

    if line != "exit\n" {
        return Err(io::Error::other("owned part control pipe closed"));
    }

    Ok(())
}

fn part_checkpoint(point: &str) -> io::Result<()> {
    let mode = std::env::var("mikrio_probe_mode").map_err(io::Error::other)?;

    let (kind, target) = mode
        .split_once(':')
        .ok_or_else(|| io::Error::other("missing part fault mode"))?;

    if kind == "unknown" {
        if point == "part.publication.commit.after" {
            FIRED.store(true, Ordering::SeqCst);
            return Err(io::Error::other("lost actual part commit acknowledgment"));
        }

        if point == "publication.witness.before" {
            return Err(io::Error::other("part witness unavailable"));
        }

        return Ok(());
    }

    if point != target || FIRED.swap(true, Ordering::SeqCst) {
        return Ok(());
    }

    if kind == "error" {
        return Err(io::Error::other("owned part effect failure"));
    }

    announce("CHECKPOINT")?;
    release()
}

fn prepare_part(path: &std::path::Path) -> TestResult {
    runtime()?.block_on(async {
        let connection = Connection::open(path.to_path_buf(), None).await?;
        connection.pause_maintenance();
        connection.create_bucket(request().bucket).await?;
        let upload = connection.create_multipart_upload(request()).await?;

        connection
            .upload_part(part(upload.upload_id, 1), b"original".as_slice())
            .await?;

        connection.close().await?;
        Ok::<_, Box<dyn std::error::Error>>(())
    })
}

fn part_death(point: &str) -> TestResult {
    let _serial = process::process_scenario()?;
    let directory = TestDirectory::new()?.retain_on_failure();

    let path = directory.path().join("store");
    prepare_part(&path)?;
    let mut child = process::Probe::start(&path, &format!("kill:{point}"), PART_CHILD)?;
    assert_eq!(child.observation()?, "CHECKPOINT", "{point}");
    child.kill_and_wait()?;

    let expected: &[u8] = if matches!(point, "part.publication.commit.after" | "acknowledged") {
        b"replacement"
    } else {
        b"original"
    };

    inspect(&path, expected)?;

    runtime()?.block_on(async {
        let reopened = Connection::open(path.clone(), None).await?;
        reopened.get_owner_info().await?;
        reopened.close().await?;
        Ok::<_, Box<dyn std::error::Error>>(())
    })?;

    inspect(&path, expected)?;

    directory.finish()?;
    Ok(())
}

fn part_error(mode: &str) -> TestResult {
    let _serial = process::process_scenario()?;
    let directory = TestDirectory::new()?.retain_on_failure();

    let path = directory.path().join("store");
    prepare_part(&path)?;
    let mut child = process::Probe::start(&path, mode, PART_CHILD)?;
    let observation = child.observation()?;

    let expected = if mode.starts_with("unknown:") {
        "Unknown"
    } else if mode == "error:part.publication.commit.after" {
        "Committed"
    } else {
        "NotCommitted"
    };

    assert_eq!(observation, expected, "{mode}");
    child.finish()?;

    inspect(
        &path,
        if expected == "NotCommitted" {
            b"original"
        } else {
            b"replacement"
        },
    )?;

    directory.finish()?;
    Ok(())
}

#[test]
#[ignore = "Owned subprocess entry for multipart part publication effects"]
fn part_process_child() -> TestResult {
    let path = PathBuf::from(
        std::env::var_os("mikrio_probe_directory").ok_or("missing owned part directory")?,
    );

    let upload_id = retained_ids_after_close(&path)?;
    let mode = std::env::var("mikrio_probe_mode")?;

    runtime()?.block_on(async {
        let connection = Connection::open(path, None).await?;
        connection.pause_maintenance();
        crate::protocol_fault::install(part_checkpoint)?;

        let result = connection
            .upload_part(part(upload_id, 1), b"replacement".as_slice())
            .await;

        if mode == "kill:acknowledged" {
            result?;
            announce("CHECKPOINT")?;
            release()?;
        } else if mode.starts_with("error:") || mode.starts_with("unknown:") {
            assert!(FIRED.load(Ordering::SeqCst));

            match result {
                Ok(_) => announce("Committed")?,
                Err(error) => announce(match error.outcome() {
                    Some(MutationOutcome::NotCommitted) => "NotCommitted",
                    Some(MutationOutcome::Unknown) => "Unknown",
                    _ => return Err("unexpected part error outcome".into()),
                })?,
            }

            release()?;
        } else {
            result?;
        }

        connection.close().await?;
        Ok::<_, Box<dyn std::error::Error>>(())
    })
}
