use super::*;
use crate::multipart::{ListPartsRequest, PartPageSize, UploadRequest};

fn control(upload_id: UploadId) -> UploadRequest {
    UploadRequest {
        bucket: request().bucket,
        key: request().key,
        upload_id,
        expected_owner: None,
    }
}

fn page(upload_id: UploadId) -> ListPartsRequest {
    ListPartsRequest {
        upload: control(upload_id),
        part_number_marker: 0,
        max_parts: PartPageSize::default(),
    }
}

#[test]
fn abort_fences_an_admitted_stream_and_preserves_current_object_bytes_across_reopen() -> TestResult
{
    let directory = TestDirectory::new()?.retain_on_failure();
    let path = directory.path().join("store");

    let upload_id = runtime()?.block_on(async {
        let connection = Connection::open(path.clone(), None).await?;
        connection.pause_maintenance();
        let peer = Connection::open(path.clone(), None).await?;
        peer.pause_maintenance();
        connection.create_bucket(request().bucket).await?;

        connection
            .put_object(
                super::super::super::objects::put(request().key.as_str()),
                b"current object".as_slice(),
            )
            .await?;

        let upload = connection.create_multipart_upload(request()).await?;

        connection
            .upload_part(
                part(upload.upload_id.clone(), 7),
                b"original part".as_slice(),
            )
            .await?;

        let original = peer.list_parts(page(upload.upload_id.clone())).await?;
        assert_eq!(original.parts.len(), 1);
        assert_eq!(original.parts[0].size.get(), 13);
        let (entered, observed) = oneshot::channel();
        let (release, released) = oneshot::channel();
        let writer = connection.clone();
        let input = part(upload.upload_id.clone(), 7);

        let task = tokio::spawn(async move {
            writer
                .upload_part(
                    input,
                    Gated {
                        entered: Some(entered),
                        release: released,
                        ready: false,
                        bytes: b"losing replacement",
                    },
                )
                .await
        });

        observed.await?;

        assert_eq!(
            peer.list_parts(page(upload.upload_id.clone())).await?.parts,
            original.parts
        );

        tokio::time::timeout(
            Duration::from_secs(10),
            peer.abort_multipart_upload(control(upload.upload_id.clone())),
        )
        .await??;

        release.send(()).map_err(|()| "missing stalled writer")?;
        rejected(&task.await?.unwrap_err(), OperationFailure::NoSuchUpload);

        assert_eq!(
            peer.list_parts(page(upload.upload_id.clone()))
                .await
                .unwrap_err()
                .failure(),
            OperationFailure::NoSuchUpload
        );

        connection.close().await?;
        peer.close().await?;
        Ok::<_, Box<dyn std::error::Error>>(upload.upload_id)
    })?;

    runtime()?.block_on(async {
        let reopened = Connection::open(path.clone(), None).await?;

        assert_eq!(
            reopened
                .list_parts(page(upload_id.clone()))
                .await
                .unwrap_err()
                .failure(),
            OperationFailure::NoSuchUpload
        );

        rejected(
            &reopened
                .abort_multipart_upload(control(upload_id))
                .await
                .unwrap_err(),
            OperationFailure::NoSuchUpload,
        );

        let (_, mut body) = reopened
            .get_object(super::super::super::objects::read(request().key.as_str()))
            .await?;

        assert_eq!(
            body.read_chunk(1024).await?,
            Some(b"current object".to_vec())
        );

        assert_eq!(body.read_chunk(1024).await?, None);
        body.close().await?;
        reopened.close().await?;
        Ok::<_, Box<dyn std::error::Error>>(())
    })?;

    directory.finish()?;
    Ok(())
}

const CONTROL_CHILD: &str = "connection::tests::multipart::parts::control::abort_process_child";

fn abort_effect(mode: &str, committed: bool) -> TestResult {
    let _serial = process::process_scenario()?;
    let directory = TestDirectory::new()?.retain_on_failure();

    let path = directory.path().join("store");
    prepare_part(&path)?;
    let upload_id = retained_ids_after_close(&path)?;
    let mut child = process::Probe::start(&path, mode, CONTROL_CHILD)?;

    if mode.starts_with("kill:") {
        assert_eq!(child.observation()?, "CHECKPOINT", "{mode}");
        child.kill_and_wait()?;
    } else {
        assert_eq!(
            child.observation()?,
            if committed { "Unknown" } else { "NotCommitted" },
            "{mode}"
        );

        child.finish()?;
    }

    // Maintenance is paused in the child; observe the terminal metadata before reopening.
    let database = rusqlite::Connection::open(path.join("metadata.sqlite3"))?;

    assert_eq!(
        database.query_row("SELECT count(*) FROM multipart_uploads", [], |row| {
            row.get::<_, i64>(0)
        })?,
        i64::from(!committed)
    );

    assert_eq!(
        database.query_row("SELECT count(*) FROM multipart_parts", [], |row| {
            row.get::<_, i64>(0)
        })?,
        i64::from(!committed)
    );

    assert_eq!(
        database.query_row(
            "SELECT count(*) FROM multipart_receipts WHERE outcome = 'aborted'",
            [],
            |row| row.get::<_, i64>(0)
        )?,
        i64::from(committed)
    );

    database.close().map_err(|(_, error)| error)?;

    if !committed {
        inspect(&path, b"original")?;
    }

    runtime()?.block_on(async {
        let reopened = Connection::open(path.clone(), None).await?;
        let result = reopened.list_parts(page(upload_id)).await;

        if committed {
            assert_eq!(
                result.unwrap_err().failure(),
                OperationFailure::NoSuchUpload
            );
        } else {
            assert_eq!(result?.parts[0].size.get(), 8);
        }

        reopened.close().await?;
        Ok::<_, Box<dyn std::error::Error>>(())
    })?;

    directory.finish()?;
    Ok(())
}

#[test]
fn kill_multipart_abort_begun_preserves_terminal_transition() -> TestResult {
    abort_effect("kill:multipart.abort.begun", false)
}

#[test]
fn kill_multipart_abort_commit_before_preserves_terminal_transition() -> TestResult {
    abort_effect("kill:multipart.abort.commit.before", false)
}

#[test]
fn kill_multipart_abort_commit_after_preserves_terminal_transition() -> TestResult {
    abort_effect("kill:multipart.abort.commit.after", true)
}

#[test]
fn kill_acknowledged_preserves_terminal_transition() -> TestResult {
    abort_effect("kill:acknowledged", true)
}

#[test]
fn error_multipart_abort_commit_before_preserves_terminal_transition() -> TestResult {
    abort_effect("error:multipart.abort.commit.before", false)
}

#[test]
fn error_multipart_abort_commit_after_preserves_terminal_transition() -> TestResult {
    abort_effect("error:multipart.abort.commit.after", true)
}

#[test]
#[ignore = "Owned subprocess entry for explicit abort transaction effects"]
fn abort_process_child() -> TestResult {
    let path = PathBuf::from(
        std::env::var_os("mikrio_probe_directory").ok_or("missing owned abort directory")?,
    );

    let upload_id = retained_ids_after_close(&path)?;
    let mode = std::env::var("mikrio_probe_mode")?;

    runtime()?.block_on(async {
        let connection = Connection::open(path, None).await?;
        connection.pause_maintenance();
        crate::protocol_fault::install(part_checkpoint)?;
        let result = connection.abort_multipart_upload(control(upload_id)).await;

        if mode == "kill:acknowledged" {
            result?;
            announce("CHECKPOINT")?;
            release()?;
        } else if mode.starts_with("error:") {
            assert!(FIRED.load(Ordering::SeqCst));
            let error = result.expect_err("injected abort failure must remain observable");

            announce(match error.outcome() {
                Some(MutationOutcome::NotCommitted) => "NotCommitted",
                Some(MutationOutcome::Unknown) => "Unknown",
                _ => return Err("unexpected abort outcome".into()),
            })?;

            release()?;
        } else {
            result?;
        }

        connection.close().await?;
        Ok::<_, Box<dyn std::error::Error>>(())
    })
}
