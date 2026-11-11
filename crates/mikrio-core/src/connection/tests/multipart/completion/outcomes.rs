use super::*;
use crate::multipart::{ListUploadsRequest, UploadPageSize};
use crate::{BlobId, GuardManager, GuardMode, GuardTarget};

use std::sync::atomic::AtomicBool;

mod effects;
mod lifecycle;
mod retirement;

const CHILD: &str = "connection::tests::multipart::completion::outcomes::completion_process_child";
static FIRED: AtomicBool = AtomicBool::new(false);

fn announce(value: &str) -> io::Result<()> {
    let mut output = io::stdout().lock();
    writeln!(output, "PROBE {value}")?;

    output.flush()
}

fn release() -> io::Result<()> {
    let mut line = String::new();
    io::stdin().lock().read_line(&mut line)?;

    Ok(())
}

fn checkpoint(point: &str) -> io::Result<()> {
    let mode = std::env::var("mikrio_probe_mode").map_err(io::Error::other)?;

    let (kind, target) = mode
        .split_once(':')
        .ok_or_else(|| io::Error::other("missing completion fault mode"))?;

    if matches!(kind, "witness" | "unknown") {
        if point == "multipart.complete.commit.after" {
            return Err(io::Error::other(
                "lost actual completion commit acknowledgment",
            ));
        }

        if point == "multipart.complete.witness.before" {
            if kind == "unknown" {
                return Err(io::Error::other("completion witness unavailable"));
            }

            announce("CHECKPOINT")?;

            return release();
        }

        return Ok(());
    }

    if point != target || FIRED.swap(true, Ordering::SeqCst) {
        return Ok(());
    }

    if kind == "error" {
        return Err(io::Error::other("owned completion effect failure"));
    }

    announce("CHECKPOINT")?;

    release()
}

fn prepare(path: &std::path::Path) -> Result<UploadId, Box<dyn std::error::Error>> {
    runtime()?.block_on(async {
        let connection = Connection::open(path.to_path_buf(), None).await?;
        connection.pause_maintenance();
        connection.create_bucket(request().bucket).await?;

        connection
            .put_object(
                super::super::super::objects::put(request().key.as_str()),
                b"before".as_slice(),
            )
            .await?;

        let mut initiation = request();

        initiation.tags = Some(crate::object::ObjectTags::new([(
            "status".into(),
            "initial".into(),
        )])?);

        let created = connection.create_multipart_upload(initiation).await?;

        connection
            .upload_part(
                super::super::parts::part(created.upload_id.clone(), 1),
                b"abc".as_slice(),
            )
            .await?;

        connection.close().await?;
        Ok(created.upload_id)
    })
}

fn count(path: &std::path::Path, query: &str) -> Result<i64, Box<dyn std::error::Error>> {
    let database = rusqlite::Connection::open(path.join("metadata.sqlite3"))?;
    let value = database.query_row(query, [], |row| row.get(0))?;
    database.close().map_err(|(_, error)| error)?;

    Ok(value)
}

fn completion_effect(mode: &str) -> TestResult {
    let _serial = process::process_scenario()?;
    let directory = TestDirectory::new()?.retain_on_failure();

    let path = directory.path().join("store");
    let upload_id = prepare(&path)?;
    let original = super::super::super::faults::object_state(&path, request().key.as_str())?;

    let committed =
        mode.ends_with("multipart.complete.commit.after") || mode.starts_with("unknown:");

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

    if !committed {
        assert_eq!(
            super::super::super::faults::object_state(&path, request().key.as_str())?,
            original,
            "{mode}"
        );
    }

    assert_eq!(
        count(&path, "SELECT count(*) FROM multipart_parts")?,
        i64::from(!committed),
        "{mode}"
    );

    assert_eq!(
        count(&path, "SELECT count(*) FROM blob_parts")?,
        i64::from(committed),
        "{mode}"
    );

    assert_eq!(
        count(
            &path,
            "SELECT count(*) FROM multipart_receipts WHERE outcome='completed'"
        )?,
        i64::from(committed),
        "{mode}"
    );

    runtime()?.block_on(async {
        let connection = Connection::open(path.clone(), None).await?;

        assert_eq!(
            bytes(&connection).await?,
            if committed {
                b"abc".as_slice()
            } else {
                b"before".as_slice()
            },
            "{mode}"
        );

        if !committed {
            tokio::time::timeout(Duration::from_secs(10), async {
                while count(
                    &path,
                    "SELECT count(*) FROM multipart_uploads WHERE state='completing'",
                )? != 0
                {
                    tokio::time::sleep(Duration::from_millis(20)).await;
                }

                Ok::<_, Box<dyn std::error::Error>>(())
            })
            .await??;

            let input = completion(&connection, upload_id.clone()).await?;
            assert_eq!(input.parts.parts().len(), 1);
            connection.complete_multipart_upload(input).await?;
            assert_eq!(bytes(&connection).await?, b"abc");
        }

        connection.close().await?;
        Ok::<_, Box<dyn std::error::Error>>(())
    })?;

    directory.finish()?;
    Ok(())
}

fn output(path: &std::path::Path) -> Result<BlobId, Box<dyn std::error::Error>> {
    let database = rusqlite::Connection::open(path.join("metadata.sqlite3"))?;

    let blob: Vec<u8> = database.query_row(
        "SELECT completion_blob_id FROM multipart_uploads WHERE completion_blob_id IS NOT NULL
         UNION ALL SELECT completion_blob_id FROM multipart_receipts
         WHERE completion_blob_id IS NOT NULL LIMIT 1",
        [],
        |row| row.get(0),
    )?;

    database.close().map_err(|(_, error)| error)?;

    Ok(BlobId::from_bytes(
        blob.try_into().map_err(|_| "invalid output identity")?,
    ))
}

#[test]
fn abort_wins_against_guarded_assembly_and_lost_commit_receipt_survives_later_delete() -> TestResult
{
    let _serial = process::process_scenario()?;
    let directory = TestDirectory::new()?.retain_on_failure();

    for (index, mode) in ["pause:stored.read.before", "witness:replace"]
        .into_iter()
        .enumerate()
    {
        let path = directory.path().join(format!("store-{index}"));
        let upload_id = prepare(&path)?;
        let mut child = process::Probe::start(&path, mode, CHILD)?;
        assert_eq!(child.observation()?, "CHECKPOINT");
        let output = output(&path)?;

        assert!(
            GuardManager::open(&path)?
                .try_acquire(GuardTarget::Shard(output.shard()), GuardMode::Exclusive)?
                .is_none()
        );

        runtime()?.block_on(async {
            let connection = Connection::open(path.clone(), None).await?;
            connection.pause_maintenance();

            if index == 0 {
                connection.abort_multipart_upload(upload(upload_id)).await?;
                assert_eq!(bytes(&connection).await?, b"before");
            } else {
                connection
                    .put_object(
                        super::super::super::objects::put(request().key.as_str()),
                        b"replacement".as_slice(),
                    )
                    .await?;

                connection
                    .delete_object(
                        super::super::super::objects::read(request().key.as_str()),
                        None,
                    )
                    .await?;

                assert_eq!(
                    count(
                        &path,
                        "SELECT count(*) FROM multipart_receipts WHERE outcome='completed'"
                    )?,
                    1
                );
            }

            connection.close().await?;
            Ok::<_, Box<dyn std::error::Error>>(())
        })?;

        assert!(
            GuardManager::open(&path)?
                .try_acquire(GuardTarget::Shard(output.shard()), GuardMode::Exclusive)?
                .is_none()
        );

        child.finish()?;

        assert_eq!(
            child.observation()?,
            if index == 0 {
                "NotCommitted"
            } else {
                "Committed"
            }
        );

        assert_eq!(
            count(&path, "SELECT count(*) FROM objects")?,
            i64::from(index == 0)
        );
    }

    directory.finish()?;

    Ok(())
}

#[test]
fn completion_lost_acknowledgment_preserves_retagging_and_fences_receipt_pruning() -> TestResult {
    use super::super::super::objects;
    use crate::connection::maintenance;

    let _serial = process::process_scenario()?;
    let directory = TestDirectory::new()?.retain_on_failure();
    let path = directory.path().join("store");
    prepare(&path)?;
    let mut child = process::Probe::start(&path, "witness:retag", CHILD)?;
    assert_eq!(child.observation()?, "CHECKPOINT");

    runtime()?.block_on(async {
        let peer = Connection::open(path.clone(), None).await?;
        peer.pause_maintenance();
        let read = objects::read(request().key.as_str());
        let committed = peer.head_object(read.clone()).await?;

        assert_eq!(
            committed.tags,
            crate::object::ObjectTags::new([("status".into(), "initial".into())])?
        );

        assert_eq!(bytes(&peer).await?, b"abc");
        assert_eq!(committed.part_count, Some(1));
        let tags = crate::object::ObjectTags::new([("status".into(), "retagged".into())])?;
        peer.replace_object_tags(read.clone(), tags.clone()).await?;
        let retagged = peer.head_object(read.clone()).await?;
        assert_ne!(retagged.generation, committed.generation);
        assert_eq!(retagged.blob_id, committed.blob_id);
        assert_eq!(retagged.etag, committed.etag);
        assert_eq!(retagged.last_modified, committed.last_modified);
        assert_eq!(retagged.part_count, committed.part_count);
        assert_eq!(retagged.checksums, committed.checksums);
        assert_eq!(retagged.tags, tags);
        assert_eq!(count(&path, "SELECT count(*) FROM multipart_receipts")?, 1);

        // Execute the actual receipt scan and output-guard acquisition while
        // the completing process is paused before its authoritative witness.
        maintenance::prune_receipts(Arc::new(peer.admit()?), path.clone()).await?;
        assert_eq!(count(&path, "SELECT count(*) FROM multipart_receipts")?, 1);
        child.finish_async().await?;
        assert_eq!(child.observation_async().await?, "Committed");
        maintenance::prune_receipts(Arc::new(peer.admit()?), path.clone()).await?;
        assert_eq!(count(&path, "SELECT count(*) FROM multipart_receipts")?, 0);
        peer.close().await?;

        for _ in 0..2 {
            let reopened = Connection::open(path.clone(), None).await?;
            let selected = reopened.head_object(read.clone()).await?;
            assert_eq!(selected.generation, retagged.generation);
            assert_eq!(selected.blob_id, committed.blob_id);
            assert_eq!(selected.etag, committed.etag);
            assert_eq!(selected.last_modified, committed.last_modified);
            assert_eq!(selected.part_count, committed.part_count);
            assert_eq!(selected.checksums, committed.checksums);
            assert_eq!(selected.tags, tags);
            assert_eq!(bytes(&reopened).await?, b"abc");
            reopened.close().await?;
        }

        Ok::<_, Box<dyn std::error::Error>>(())
    })?;

    directory.finish()?;
    Ok(())
}

#[test]
#[ignore = "Owned subprocess entry for multipart completion and receipt outcomes"]
fn completion_process_child() -> TestResult {
    let path =
        PathBuf::from(std::env::var_os("mikrio_probe_directory").ok_or("missing owned directory")?);

    runtime()?.block_on(async {
        let connection = Connection::open(path, None).await?;
        connection.pause_maintenance();

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

        let input = completion(&connection, upload_id).await?;
        crate::protocol_fault::install(checkpoint)?;
        let mode = std::env::var("mikrio_probe_mode")?;

        if mode.starts_with("close:") || mode.starts_with("cancel:") {
            return interrupt_during_read(connection, input, &mode).await;
        }

        let result = connection.complete_multipart_upload(input).await;

        match result {
            Ok(result) => {
                assert_eq!(result.etag, "\"af5da9f45af7a300e3aded972f8ff687-1\"");
                announce("Committed")?;
            }

            Err(error) => announce(match error.outcome() {
                Some(MutationOutcome::NotCommitted) => "NotCommitted",
                Some(MutationOutcome::Unknown) => "Unknown",
                _ => return Err("invalid completion outcome".into()),
            })?,
        }

        release()?;
        connection.close().await?;
        Ok::<_, Box<dyn std::error::Error>>(())
    })
}

async fn interrupt_during_read(
    connection: Connection,
    input: CompleteRequest,
    mode: &str,
) -> TestResult {
    let writer = connection.clone();
    let task = tokio::spawn(async move { writer.complete_multipart_upload(input).await });

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
fn close_and_caller_cancellation_wait_for_actual_native_assembly_ownership() -> TestResult {
    let _serial = process::process_scenario()?;
    let directory = TestDirectory::new()?.retain_on_failure();

    for (index, mode) in ["close:stored.read.before", "cancel:stored.read.before"]
        .into_iter()
        .enumerate()
    {
        let path = directory.path().join(format!("store-{index}"));
        prepare(&path)?;
        let mut child = process::Probe::start(&path, mode, CHILD)?;
        assert_eq!(child.observation()?, "CHECKPOINT");
        assert_eq!(child.observation()?, "CLOSE_FENCED");
        let output = output(&path)?;

        assert!(
            GuardManager::open(&path)?
                .try_acquire(GuardTarget::Shard(output.shard()), GuardMode::Exclusive)?
                .is_none()
        );

        child.finish()?;

        assert_eq!(
            child.observation()?,
            if index == 0 {
                "NotCommitted"
            } else {
                "Canceled"
            }
        );

        assert_eq!(count(&path, "SELECT count(*) FROM multipart_parts")?, 1);
        assert_eq!(count(&path, "SELECT count(*) FROM multipart_receipts")?, 0);

        runtime()?.block_on(async {
            let connection = Connection::open(path.clone(), None).await?;
            assert_eq!(bytes(&connection).await?, b"before");
            connection.close().await?;
            Ok::<_, Box<dyn std::error::Error>>(())
        })?;
    }

    directory.finish()?;
    Ok(())
}
