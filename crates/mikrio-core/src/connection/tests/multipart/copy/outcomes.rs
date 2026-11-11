use super::*;
use crate::access::{AccessKeyId, HttpIdentity, PrincipalId, PrincipalName};
use crate::multipart::{ListUploadsRequest, UploadPageSize};
use crate::{BlobId, GuardManager};
use std::path::Path;
use std::sync::atomic::AtomicBool;

const CHILD: &str = "connection::tests::multipart::copy::outcomes::part_copy_process_child";
static FIRED: AtomicBool = AtomicBool::new(false);

#[allow(
    clippy::struct_field_names,
    reason = "distinct durable identities retain their subjects and ID suffixes"
)]
struct Prepared {
    upload_id: UploadId,
    source_blob_id: BlobId,
    principal_id: PrincipalId,
    access_key_id: AccessKeyId,
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
        return Err(io::Error::other("owned part-copy control closed"));
    }

    Ok(())
}

fn checkpoint(point: &str) -> io::Result<()> {
    let mode = std::env::var("mikrio_probe_mode").map_err(io::Error::other)?;

    let (kind, target) = mode
        .split_once(':')
        .ok_or_else(|| io::Error::other("missing part-copy fault mode"))?;

    if matches!(kind, "witness" | "unknown") {
        if point == "part.publication.commit.after" {
            return Err(io::Error::other("lost actual copied-part acknowledgment"));
        }

        if point == "publication.witness.before" {
            if kind == "unknown" {
                return Err(io::Error::other("copied-part witness unavailable"));
            }

            FIRED.store(true, Ordering::SeqCst);
            announce("CHECKPOINT")?;
            return release();
        }

        return Ok(());
    }

    if point != target || FIRED.swap(true, Ordering::SeqCst) {
        return Ok(());
    }

    if kind == "error" {
        return Err(io::Error::other("owned part-copy effect failure"));
    }

    announce("CHECKPOINT")?;
    release()
}

fn prepare(path: &Path) -> Result<Prepared, Box<dyn std::error::Error>> {
    runtime()?.block_on(async {
        let connection = Connection::open(path.to_path_buf(), None).await?;
        connection.pause_maintenance();
        connection.create_bucket(request().bucket).await?;
        let mut source = super::super::super::objects::put("source");
        source.tags = Some(ObjectTags::new([("team".into(), "source".into())])?);
        connection.put_object(source, b"abc".as_slice()).await?;

        connection
            .put_object(
                super::super::super::objects::put(request().key.as_str()),
                b"old-object".as_slice(),
            )
            .await?;

        let source = connection
            .head_object(super::super::super::objects::read("source"))
            .await?;

        let created = connection.create_multipart_upload(request()).await?;

        connection
            .upload_part(
                super::super::parts::part(created.upload_id.clone(), 1),
                b"before".as_slice(),
            )
            .await?;

        let principal = connection
            .create_principal(PrincipalName::new("part-copy-worker".into())?)
            .await?;

        let document = serde_json::json!({
            "Version": "2012-10-17",
            "Statement": [
                {
                    "Effect": "Allow",
                    "Action": "s3:GetObject",
                    "Resource": "arn:aws:s3:::attachments/source",
                    "Condition": {
                        "StringEquals": {"s3:ExistingObjectTag/team": "source"}
                    }
                },
                {
                    "Effect": "Allow",
                    "Action": "s3:PutObject",
                    "Resource": format!("arn:aws:s3:::attachments/{}", request().key.as_str())
                }
            ]
        });

        connection
            .put_principal_policy(principal.principal_id, document.to_string())
            .await?;

        let key = connection.create_access_key(principal.principal_id).await?;

        // Only the public key identity crosses the owned subprocess boundary.
        std::fs::write(
            path.with_extension("identity"),
            key.access_key.access_key_id.as_str(),
        )?;

        connection.close().await?;

        Ok(Prepared {
            upload_id: created.upload_id,
            source_blob_id: source.blob_id,
            principal_id: principal.principal_id,
            access_key_id: key.access_key.access_key_id,
        })
    })
}

async fn object_bytes(connection: &Connection) -> Result<Vec<u8>, Box<dyn std::error::Error>> {
    let (_, mut body) = connection
        .get_object(super::super::super::objects::read(request().key.as_str()))
        .await?;

    let bytes = body.read_all(100).await?;
    body.close().await?;
    Ok(bytes)
}

fn output(path: &Path) -> Result<BlobId, Box<dyn std::error::Error>> {
    let database = rusqlite::Connection::open(path.join("metadata.sqlite3"))?;

    let blob: Vec<u8> = database.query_row(
        "SELECT blob_id FROM blobs WHERE state='reserved' AND write_kind='upload_part'
         UNION ALL SELECT blob_id FROM multipart_parts LIMIT 1",
        [],
        |row| row.get(0),
    )?;

    database.close().map_err(|(_, error)| error)?;

    Ok(BlobId::from_bytes(
        blob.try_into()
            .map_err(|_| "invalid copied-part identity")?,
    ))
}

fn protected(path: &Path, blob: BlobId) -> TestResult {
    assert!(
        GuardManager::open(path)?
            .try_acquire(GuardTarget::Shard(blob.shard()), GuardMode::Exclusive)?
            .is_none()
    );

    Ok(())
}

fn publication_effect(mode: &str) -> TestResult {
    let _serial = process::process_scenario()?;
    let directory = TestDirectory::new()?.retain_on_failure();

    let path = directory.path().join("store");
    let prepared = prepare(&path)?;
    let committed = mode.ends_with("part.publication.commit.after") || mode.starts_with("unknown:");
    let mut child = process::Probe::start(&path, mode, CHILD)?;

    if mode.starts_with("kill:") {
        assert_eq!(child.observation()?, "CHECKPOINT", "{mode}");
        child.kill_and_wait()?;
    } else {
        let expected = if mode.starts_with("unknown:") {
            "Unknown"
        } else if committed {
            "Committed"
        } else {
            "NotCommitted"
        };

        assert_eq!(child.observation()?, expected, "{mode}");
        child.finish()?;
    }

    super::super::super::faults::publication_references(&path)?;

    runtime()?.block_on(async {
        let connection = Connection::open(path, None).await?;
        assert_eq!(object_bytes(&connection).await?, b"old-object");

        assert_eq!(
            page(&connection, prepared.upload_id.clone())
                .await?
                .parts
                .len(),
            1
        );

        complete(&connection, prepared.upload_id).await?;

        assert_eq!(
            object_bytes(&connection).await?,
            if committed {
                b"abc".as_slice()
            } else {
                b"before".as_slice()
            },
            "{mode}"
        );

        connection.close().await?;
        Ok::<_, Box<dyn std::error::Error>>(())
    })?;

    directory.finish()?;
    Ok(())
}

fn selection_change(change: &str, point: &str, committed: bool) -> TestResult {
    let _serial = process::process_scenario()?;
    let directory = TestDirectory::new()?.retain_on_failure();

    let path = directory.path().join("store");
    let prepared = prepare(&path)?;
    let mut child = process::Probe::start(&path, &format!("{change}:{point}"), CHILD)?;
    assert_eq!(child.observation()?, "CHECKPOINT");

    if point != "copy.selected" {
        protected(&path, prepared.source_blob_id)?;
        protected(&path, output(&path)?)?;
    }

    runtime()?.block_on(async {
        let peer = Connection::open(path.clone(), None).await?;
        peer.pause_maintenance();

        match change {
            "source" => {
                peer.put_object(
                    super::super::super::objects::put("source"),
                    b"replacement".as_slice(),
                )
                .await?;
            }
            "part" => {
                peer.upload_part(
                    super::super::parts::part(prepared.upload_id.clone(), 1),
                    b"replacement".as_slice(),
                )
                .await?;
            }
            "abort" => {
                peer.abort_multipart_upload(upload(prepared.upload_id.clone()))
                    .await?;
            }
            "complete" => complete(&peer, prepared.upload_id.clone()).await?,
            "delete" => {
                peer.delete_object(super::super::super::objects::read("source"), None)
                    .await?;
            }
            "tags" => {
                peer.replace_object_tags(
                    super::super::super::objects::read("source"),
                    ObjectTags::new([("team".into(), "changed".into())])?,
                )
                .await?;
            }
            "revoke" => {
                peer.delete_access_key(prepared.principal_id, prepared.access_key_id)
                    .await?;
            }
            _ => return Err("unknown copied-part race".into()),
        }

        peer.close().await?;
        Ok::<_, Box<dyn std::error::Error>>(())
    })?;

    child.finish()?;

    assert_eq!(
        child.observation()?,
        if committed {
            "Committed"
        } else {
            "NotCommitted"
        }
    );

    runtime()?.block_on(async {
        let connection = Connection::open(path, None).await?;

        if committed {
            complete(&connection, prepared.upload_id).await?;
            assert_eq!(object_bytes(&connection).await?, b"abc");
        } else if matches!(change, "tags" | "revoke") {
            complete(&connection, prepared.upload_id).await?;
            assert_eq!(object_bytes(&connection).await?, b"before");
        } else {
            assert_eq!(
                page(&connection, prepared.upload_id)
                    .await
                    .unwrap_err()
                    .failure(),
                OperationFailure::NoSuchUpload
            );

            assert_eq!(
                object_bytes(&connection).await?,
                if change == "complete" {
                    b"before".as_slice()
                } else {
                    b"old-object".as_slice()
                }
            );
        }

        connection.close().await?;
        Ok::<_, Box<dyn std::error::Error>>(())
    })?;

    directory.finish()?;
    Ok(())
}

#[test]
fn selected_source_survives_replacement() -> TestResult {
    selection_change("source", "part.copy.admitted", true)
}

#[test]
fn admitted_copy_replaces_competing_part() -> TestResult {
    selection_change("part", "payload.published", true)
}

#[test]
fn admitted_copy_loses_to_abort() -> TestResult {
    selection_change("abort", "stored.read.before", false)
}

#[test]
fn admitted_copy_loses_to_completion() -> TestResult {
    selection_change("complete", "payload.published", false)
}

#[test]
fn selected_source_survives_deletion() -> TestResult {
    selection_change("delete", "part.copy.admitted", true)
}

#[test]
fn admitted_copy_keeps_selected_source_tags() -> TestResult {
    selection_change("tags", "part.copy.admitted", true)
}

#[test]
fn selection_rechecks_source_tags() -> TestResult {
    selection_change("tags", "copy.selected", false)
}

#[test]
fn publication_rechecks_authority() -> TestResult {
    selection_change("revoke", "payload.published", false)
}

#[test]
fn copied_part_witness_and_original_timestamp_survive_replacement_and_abort() -> TestResult {
    let _serial = process::process_scenario()?;
    let directory = TestDirectory::new()?.retain_on_failure();
    let path = directory.path().join("store");
    let prepared = prepare(&path)?;
    let mut child = process::Probe::start(&path, "witness:publication.witness.before", CHILD)?;
    assert_eq!(child.observation()?, "CHECKPOINT");
    let output = output(&path)?;

    let modified = runtime()?.block_on(async {
        let peer = Connection::open(path.clone(), None).await?;
        peer.pause_maintenance();
        let selected = page(&peer, prepared.upload_id.clone()).await?;
        let modified = selected.parts[0].last_modified;

        peer.upload_part(
            super::super::parts::part(prepared.upload_id.clone(), 1),
            b"replacement".as_slice(),
        )
        .await?;

        peer.abort_multipart_upload(upload(prepared.upload_id))
            .await?;

        peer.close().await?;
        Ok::<_, Box<dyn std::error::Error>>(modified)
    })?;

    protected(&path, output)?;
    child.finish()?;
    assert_eq!(child.observation()?, "Committed");
    assert_eq!(child.observation()?, format!("MODIFIED {}", modified.get()));
    directory.finish()?;
    Ok(())
}

#[test]
#[ignore = "Owned subprocess entry for copied-part publication and upload races"]
fn part_copy_process_child() -> TestResult {
    let path =
        PathBuf::from(std::env::var_os("mikrio_probe_directory").ok_or("missing owned directory")?);

    let mode = std::env::var("mikrio_probe_mode")?;

    runtime()?.block_on(async {
        let connection = Connection::open(path.clone(), None).await?;
        connection.pause_maintenance();

        let identity = Arc::new(
            connection
                .resolve_application_key(std::fs::read_to_string(path.with_extension("identity"))?)
                .await?
                .verified(),
        );

        let uploads = connection
            .list_multipart_uploads(ListUploadsRequest {
                bucket: request().bucket,
                expected_owner: None,
                prefix: None,
                delimiter: None,
                key_marker: None,
                upload_id_marker: None,
                max_uploads: UploadPageSize::default(),
            })
            .await?;

        let upload_id = uploads
            .uploads
            .first()
            .ok_or("missing prepared upload")?
            .upload_id
            .clone();

        crate::protocol_fault::install(checkpoint)?;

        if mode.starts_with("close:") || mode.starts_with("cancel:") {
            return interrupt_during_read(connection, identity, input(upload_id, 1), &mode).await;
        }

        let result = connection
            .upload_part_copy_http(identity, input(upload_id, 1))
            .await;

        match result {
            Ok(copied) => {
                announce("Committed")?;
                announce(&format!("MODIFIED {}", copied.last_modified.get()))?;
            }
            Err(error) => announce(match error.outcome() {
                Some(MutationOutcome::NotCommitted) => "NotCommitted",
                Some(MutationOutcome::Unknown) => "Unknown",
                _ => return Err("invalid copied-part outcome".into()),
            })?,
        }

        if mode.starts_with("error:") || mode.starts_with("unknown:") {
            release()?;
        }

        connection.close().await?;
        Ok::<_, Box<dyn std::error::Error>>(())
    })
}

async fn interrupt_during_read(
    connection: Connection,
    identity: Arc<HttpIdentity>,
    request: CopyPartRequest,
    mode: &str,
) -> TestResult {
    let writer = connection.clone();
    let task = tokio::spawn(async move { writer.upload_part_copy_http(identity, request).await });

    tokio::time::timeout(Duration::from_secs(10), async {
        while !FIRED.load(Ordering::SeqCst) {
            tokio::time::sleep(Duration::from_millis(1)).await;
        }
    })
    .await?;

    let task = if mode.starts_with("cancel:") {
        task.abort();
        assert!(task.await.unwrap_err().is_cancelled());
        None
    } else {
        Some(task)
    };

    let mut closing = Box::pin(connection.close());

    std::future::poll_fn(|context| {
        assert!(closing.as_mut().poll(context).is_pending());
        Poll::Ready(())
    })
    .await;

    announce("CLOSE_FENCED")?;
    closing.await?;

    let Some(task) = task else {
        announce("Canceled")?;
        return Ok(());
    };

    let error = task.await?.unwrap_err();
    assert_eq!(error.failure(), OperationFailure::OperationCanceled);
    assert_eq!(error.outcome(), Some(MutationOutcome::NotCommitted));
    announce("NotCommitted")?;
    Ok(())
}

#[test]
fn copied_part_close_and_cancellation_retain_native_input_and_output_guards() -> TestResult {
    let _serial = process::process_scenario()?;
    let directory = TestDirectory::new()?.retain_on_failure();

    for (index, mode) in ["close:stored.read.before", "cancel:stored.read.before"]
        .into_iter()
        .enumerate()
    {
        let path = directory.path().join(format!("store-{index}"));
        let prepared = prepare(&path)?;
        let mut child = process::Probe::start(&path, mode, CHILD)?;
        assert_eq!(child.observation()?, "CHECKPOINT");
        assert_eq!(child.observation()?, "CLOSE_FENCED");
        protected(&path, prepared.source_blob_id)?;
        protected(&path, output(&path)?)?;
        child.finish()?;

        assert_eq!(
            child.observation()?,
            if index == 0 {
                "NotCommitted"
            } else {
                "Canceled"
            }
        );

        runtime()?.block_on(async {
            let connection = Connection::open(path, None).await?;
            assert_eq!(object_bytes(&connection).await?, b"old-object");
            complete(&connection, prepared.upload_id).await?;
            assert_eq!(object_bytes(&connection).await?, b"before");
            connection.close().await?;
            Ok::<_, Box<dyn std::error::Error>>(())
        })?;
    }

    directory.finish()?;
    Ok(())
}

mod effects;
