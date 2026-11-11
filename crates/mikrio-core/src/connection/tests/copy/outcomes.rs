use super::*;
use crate::access::{AccessKeyId, PrincipalId, PrincipalName};
use crate::object::StoredObject;
use crate::test_process as process;
use crate::{BlobId, GuardManager};
use std::io::BufRead as _;
use std::path::Path;
use std::sync::atomic::AtomicBool;

mod effects;

const CHILD: &str = "connection::tests::copy::outcomes::copy_process_child";
static FIRED: AtomicBool = AtomicBool::new(false);
static SELECTIONS: AtomicUsize = AtomicUsize::new(0);

struct Prepared {
    source: StoredObject,
    principal: PrincipalId,
    key: AccessKeyId,
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
        return Err(io::Error::other("owned copy control closed"));
    }

    Ok(())
}

fn checkpoint(point: &str) -> io::Result<()> {
    if point == "copy.selected" {
        SELECTIONS.fetch_add(1, Ordering::SeqCst);
    }

    let mode = std::env::var("mikrio_probe_mode").map_err(io::Error::other)?;

    let (kind, target) = mode
        .split_once(':')
        .ok_or_else(|| io::Error::other("missing copy fault mode"))?;

    if matches!(kind, "witness" | "unknown") {
        if point == "publication.commit.after" {
            return Err(io::Error::other("lost actual copy commit acknowledgment"));
        }

        if point == "publication.witness.before" {
            if kind == "unknown" {
                return Err(io::Error::other("copy witness unavailable"));
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
        return Err(io::Error::other("owned copy effect failure"));
    }

    announce("CHECKPOINT")?;
    release()
}

fn prepare(path: &Path) -> Result<Prepared, Box<dyn std::error::Error>> {
    runtime()?.block_on(async {
        let connection = Connection::open(path.to_path_buf(), None).await?;
        connection.pause_maintenance();
        connection.create_bucket(request().bucket).await?;
        let mut source = objects::put("source");
        source.tags = Some(ObjectTags::new([("team".into(), "source".into())])?);
        source.metadata = UserMetadata::new([("origin".into(), "source".into())])?;
        connection.put_object(source, b"abc".as_slice()).await?;

        connection
            .put_object(objects::put("destination"), b"before".as_slice())
            .await?;

        let source = connection.head_object(objects::read("source")).await?;

        let principal = connection
            .create_principal(PrincipalName::new("copy-worker".into())?)
            .await?;

        connection
            .put_principal_policy(
                principal.principal_id,
                super::authorization::policy(
                    true,
                    &["s3:PutObject", "s3:PutObjectTagging", "s3:GetObject"],
                    serde_json::json!({}),
                ),
            )
            .await?;

        let key = connection.create_access_key(principal.principal_id).await?;

        // Only the public key identity crosses the owned subprocess boundary.
        std::fs::write(
            path.with_extension("identity"),
            key.access_key.access_key_id.as_str(),
        )?;

        connection.close().await?;

        Ok(Prepared {
            source,
            principal: principal.principal_id,
            key: key.access_key.access_key_id,
        })
    })
}

fn output(path: &Path) -> Result<BlobId, Box<dyn std::error::Error>> {
    let database = rusqlite::Connection::open(path.join("metadata.sqlite3"))?;

    let blob: Vec<u8> = database.query_row(
        "SELECT blob_id FROM blobs WHERE write_kind='copy_object'",
        [],
        |row| row.get(0),
    )?;

    database.close().map_err(|(_, error)| error)?;

    Ok(BlobId::from_bytes(
        blob.try_into().map_err(|_| "invalid output identity")?,
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
    prepare(&path)?;
    let original = super::super::faults::object_state(&path, "destination")?;
    let committed = mode.ends_with("publication.commit.after") || mode.starts_with("unknown:");
    let mut child = process::Probe::start(&path, mode, CHILD)?;

    if mode.starts_with("kill:") {
        assert_eq!(child.observation()?, "CHECKPOINT", "{mode}");
        child.kill_and_wait()?;
    } else {
        assert_eq!(
            child.observation()?,
            if mode.starts_with("unknown:") {
                "Unknown"
            } else if committed {
                "Committed"
            } else {
                "NotCommitted"
            },
            "{mode}"
        );

        child.finish()?;
    }

    if !committed {
        assert_eq!(
            super::super::faults::object_state(&path, "destination")?,
            original,
            "{mode}"
        );
    }

    runtime()?.block_on(async {
        let connection = Connection::open(path, None).await?;
        assert_eq!(bytes(&connection, "source").await?, b"abc", "{mode}");

        assert_eq!(
            bytes(&connection, "destination").await?,
            if committed {
                b"abc".as_slice()
            } else {
                b"before".as_slice()
            },
            "{mode}"
        );

        let selected = connection.head_object(objects::read("destination")).await?;

        if committed {
            assert_eq!(
                selected.tags,
                ObjectTags::new([("team".into(), "source".into())])?
            );

            assert_eq!(
                selected.metadata,
                UserMetadata::new([("origin".into(), "source".into())])?
            );

            assert_eq!(selected.checksum_type, Some(ChecksumType::FullObject));
        }

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
    let mode = format!("{change}:{point}");
    let mut child = process::Probe::start(&path, &mode, CHILD)?;
    assert_eq!(child.observation()?, "CHECKPOINT");

    if point != "copy.selected" {
        protected(&path, prepared.source.blob_id)?;
        protected(&path, output(&path)?)?;
    }

    runtime()?.block_on(async {
        let peer = Connection::open(path.clone(), None).await?;
        peer.pause_maintenance();

        match change {
            "replace" => {
                peer.put_object(objects::put("source"), b"replacement".as_slice())
                    .await?;
            }
            "delete" => peer.delete_object(objects::read("source"), None).await?,
            "tags" => {
                peer.replace_object_tags(
                    objects::read("source"),
                    ObjectTags::new([("team".into(), "changed".into())])?,
                )
                .await?;
            }
            "condition" => {
                peer.put_object(objects::put("destination"), b"replacement".as_slice())
                    .await?;
            }
            "revoke" => {
                peer.delete_access_key(prepared.principal, prepared.key)
                    .await?;
            }
            "retire" => {
                peer.delete_object(objects::read("source"), None).await?;

                peer.delete_object(objects::read("destination"), None)
                    .await?;

                peer.delete_bucket(request().bucket, None).await?;
                peer.create_bucket(request().bucket).await?;

                peer.put_object(objects::put("destination"), b"replacement".as_slice())
                    .await?;
            }
            _ => return Err("unknown source/destination change".into()),
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

        let expected = if committed {
            b"abc".as_slice()
        } else if matches!(change, "condition" | "retire") {
            b"replacement".as_slice()
        } else {
            b"before".as_slice()
        };

        assert_eq!(bytes(&connection, "destination").await?, expected, "{mode}");

        if committed {
            let copied = connection.head_object(objects::read("destination")).await?;
            assert_eq!(copied.tags, prepared.source.tags);
            assert_eq!(copied.metadata, prepared.source.metadata);
        }

        connection.close().await?;
        Ok::<_, Box<dyn std::error::Error>>(())
    })?;

    directory.finish()?;
    Ok(())
}

#[test]
fn copy_reselects_same_etag_replacement_with_an_earlier_timestamp() -> TestResult {
    let _serial = process::process_scenario()?;
    let directory = TestDirectory::new()?.retain_on_failure();
    let path = directory.path().join("store");
    let prepared = prepare(&path)?;
    let sql = rusqlite::Connection::open(path.join("metadata.sqlite3"))?;

    // Explicit persisted-clock fixtures leave the process clock unchanged.
    assert_eq!(
        sql.execute(
            "UPDATE objects SET last_modified_at_ms = 1000, lifecycle_anchor_at_ms = 1000
         WHERE key = x'736f75726365'",
            [],
        )?,
        1
    );

    sql.close().map_err(|(_, error)| error)?;
    let mut child = process::Probe::start(&path, "clock:copy.selected", CHILD)?;
    assert_eq!(child.observation()?, "CHECKPOINT");

    runtime()?.block_on(async {
        let peer = Connection::open(path.clone(), None).await?;
        peer.pause_maintenance();

        assert_eq!(
            peer.head_object(objects::read("source"))
                .await?
                .last_modified,
            crate::EpochMillis::new(1000)?
        );

        let mut replacement = objects::put("source");
        replacement.tags = Some(prepared.source.tags.clone());
        replacement.metadata = UserMetadata::new([("origin".into(), "replacement".into())])?;
        peer.put_object(replacement, b"abc".as_slice()).await?;
        let current = peer.head_object(objects::read("source")).await?;
        assert_eq!(current.etag, prepared.source.etag);
        assert_ne!(current.blob_id, prepared.source.blob_id);
        assert_ne!(current.generation, prepared.source.generation);
        peer.close().await?;
        Ok::<_, Box<dyn std::error::Error>>(())
    })?;

    let sql = rusqlite::Connection::open(path.join("metadata.sqlite3"))?;

    assert_eq!(
        sql.execute(
            "UPDATE objects SET last_modified_at_ms = 0, lifecycle_anchor_at_ms = 0
         WHERE key = x'736f75726365'",
            [],
        )?,
        1
    );

    sql.close().map_err(|(_, error)| error)?;
    child.finish()?;
    assert_eq!(child.observation()?, "Committed");

    runtime()?.block_on(async {
        for _ in 0..2 {
            let connection = Connection::open(path.clone(), None).await?;
            let source = connection.head_object(objects::read("source")).await?;
            let copied = connection.head_object(objects::read("destination")).await?;
            assert_eq!(source.last_modified, crate::EpochMillis::new(0)?);
            assert_eq!(source.etag, prepared.source.etag);
            assert_eq!(copied.etag, source.etag);
            assert_ne!(copied.blob_id, source.blob_id);

            assert_eq!(
                copied.metadata,
                UserMetadata::new([("origin".into(), "replacement".into())])?
            );

            assert_eq!(bytes(&connection, "destination").await?, b"abc");
            connection.close().await?;
        }

        Ok::<_, Box<dyn std::error::Error>>(())
    })?;

    directory.finish()?;
    Ok(())
}

#[test]
fn copy_selected_source_survives_replacement() -> TestResult {
    selection_change("replace", "copy.admitted", true)
}

#[test]
fn copy_selected_source_survives_deletion() -> TestResult {
    selection_change("delete", "stored.read.before", true)
}

#[test]
fn copy_admitted_source_keeps_selected_tags() -> TestResult {
    selection_change("tags", "copy.admitted", true)
}

#[test]
fn copy_selection_rechecks_source_tags() -> TestResult {
    selection_change("tags", "copy.selected", false)
}

#[test]
fn copy_publication_rechecks_destination_conditions() -> TestResult {
    selection_change("condition", "payload.published", false)
}

#[test]
fn copy_publication_rechecks_authority() -> TestResult {
    selection_change("revoke", "payload.published", false)
}

#[test]
fn copy_publication_rejects_retired_bucket_identity() -> TestResult {
    selection_change("retire", "payload.published", false)
}

#[test]
fn copy_lost_acknowledgment_uses_its_guarded_blob_after_replacement_and_deletion() -> TestResult {
    let _serial = process::process_scenario()?;
    let directory = TestDirectory::new()?.retain_on_failure();
    let path = directory.path().join("store");
    prepare(&path)?;
    let mut child = process::Probe::start(&path, "witness:publication.witness.before", CHILD)?;
    assert_eq!(child.observation()?, "CHECKPOINT");
    let output = output(&path)?;

    runtime()?.block_on(async {
        let peer = Connection::open(path.clone(), None).await?;
        peer.pause_maintenance();
        assert_eq!(bytes(&peer, "destination").await?, b"abc");

        peer.put_object(objects::put("destination"), b"replacement".as_slice())
            .await?;

        peer.delete_object(objects::read("destination"), None)
            .await?;

        peer.close().await?;
        Ok::<_, Box<dyn std::error::Error>>(())
    })?;

    protected(&path, output)?;
    child.finish()?;
    assert_eq!(child.observation()?, "Committed");
    directory.finish()?;
    Ok(())
}

#[test]
fn copy_lost_acknowledgment_preserves_later_tags_and_original_payload_facts() -> TestResult {
    let _serial = process::process_scenario()?;
    let directory = TestDirectory::new()?.retain_on_failure();
    let path = directory.path().join("store");
    let prepared = prepare(&path)?;
    let mut child = process::Probe::start(&path, "witness:publication.witness.before", CHILD)?;
    assert_eq!(child.observation()?, "CHECKPOINT");
    let output = output(&path)?;

    runtime()?.block_on(async {
        let peer = Connection::open(path.clone(), None).await?;
        peer.pause_maintenance();
        let committed = peer.head_object(objects::read("destination")).await?;
        assert_eq!(committed.tags, prepared.source.tags);
        assert_eq!(committed.blob_id, output);
        assert_eq!(bytes(&peer, "destination").await?, b"abc");
        let tags = ObjectTags::new([("team".into(), "retagged".into())])?;

        peer.replace_object_tags(objects::read("destination"), tags.clone())
            .await?;

        let retagged = peer.head_object(objects::read("destination")).await?;
        assert_ne!(retagged.generation, committed.generation);
        assert_eq!(retagged.blob_id, committed.blob_id);
        assert_eq!(retagged.etag, committed.etag);
        assert_eq!(retagged.last_modified, committed.last_modified);
        assert_eq!(retagged.metadata, committed.metadata);
        assert_eq!(retagged.tags, tags);
        protected(&path, output)?;
        child.finish_async().await?;
        assert_eq!(child.observation_async().await?, "Committed");
        peer.close().await?;

        for _ in 0..2 {
            let reopened = Connection::open(path.clone(), None).await?;
            let selected = reopened.head_object(objects::read("destination")).await?;
            assert_eq!(selected.generation, retagged.generation);
            assert_eq!(selected.blob_id, committed.blob_id);
            assert_eq!(selected.etag, committed.etag);
            assert_eq!(selected.last_modified, committed.last_modified);
            assert_eq!(selected.metadata, committed.metadata);
            assert_eq!(selected.tags, tags);
            assert_eq!(bytes(&reopened, "destination").await?, b"abc");
            reopened.close().await?;
        }

        Ok::<_, Box<dyn std::error::Error>>(())
    })?;

    directory.finish()?;
    Ok(())
}

#[test]
#[ignore = "Owned subprocess entry for COPY publication, races and native ownership"]
fn copy_process_child() -> TestResult {
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

        let mut input = request();

        if mode.starts_with("condition:") {
            let previous = connection.head_object(objects::read("destination")).await?;
            input.conditions = WriteConditions::new(Some(previous.etag), None)?;
        }

        crate::protocol_fault::install(checkpoint)?;

        if mode.starts_with("close:") || mode.starts_with("cancel:") {
            return interrupt_during_read(connection, identity, input, &mode).await;
        }

        let result = connection.copy_object_http(identity, input).await;

        if mode.starts_with("clock:") {
            assert_eq!(
                SELECTIONS.load(Ordering::SeqCst),
                2,
                "stale COPY candidate was not retried"
            );
        }

        match result {
            Ok(_) => announce("Committed")?,
            Err(error) => announce(match error.outcome() {
                Some(MutationOutcome::NotCommitted) => "NotCommitted",
                Some(MutationOutcome::Unknown) => "Unknown",
                _ => return Err("invalid copy outcome".into()),
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
    identity: Arc<crate::access::HttpIdentity>,
    input: CopyRequest,
    mode: &str,
) -> TestResult {
    let writer = connection.clone();
    let task = tokio::spawn(async move { writer.copy_object_http(identity, input).await });

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
fn close_and_caller_cancellation_wait_for_native_copy_source_and_output_ownership() -> TestResult {
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
        protected(&path, prepared.source.blob_id)?;
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
            assert_eq!(bytes(&connection, "source").await?, b"abc");
            assert_eq!(bytes(&connection, "destination").await?, b"before");
            connection.close().await?;
            Ok::<_, Box<dyn std::error::Error>>(())
        })?;
    }

    directory.finish()?;
    Ok(())
}
