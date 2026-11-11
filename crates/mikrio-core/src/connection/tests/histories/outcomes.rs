//! A held read, lost publication acknowledgment, peer mutation and real GC.
//! The parent owns the history and fixture hashes outside the failing writer.

use std::path::Path;

use super::*;
use crate::BlobId;
use crate::test_process as process;

const WRITER: &str = "connection::tests::faults::publication_fault_child";

#[allow(clippy::unreachable, reason = "owned three-value fixture")]
fn bytes(value: u8) -> &'static [u8] {
    match value {
        1 => b"predecessor",
        2 => b"replacement bytes",
        3 => b"later bytes",
        _ => unreachable!("owned three-value fixture"),
    }
}

fn payload_path(path: &Path, blob: BlobId) -> PathBuf {
    path.join("objects")
        .join(format!("{:02x}", blob.shard()))
        .join(blob.to_string())
}

async fn select(
    connection: &Connection,
    actor: usize,
    value: u8,
    history: &History,
) -> Result<(Entry, StoredObject, ReadBody), OperationError> {
    let invoked = history.begin(actor, Action::Get);
    let (object, body) = connection.get_object(read("selected")).await?;

    assert_eq!(
        object.length.get(),
        u64::try_from(bytes(value).len()).unwrap()
    );

    assert_eq!(
        object.etag,
        format!("\"{}\"", hex(&Md5::digest(bytes(value))))
    );

    let entry = history.reply(
        actor,
        Action::Get,
        invoked,
        Response::Object(Object { value, tag: None }),
    );

    Ok((entry, object, body))
}

async fn verify_body(body: &mut ReadBody, value: u8, history: &History) -> TestResult {
    let actual = body.read_all(100).await?;
    assert_eq!(actual, bytes(value));
    assert_eq!(Sha256::digest(&actual), Sha256::digest(bytes(value)));
    body.close().await?;

    history.event(&serde_json::json!({
        "event": "body_eof", "value": value, "length": actual.len(),
        "sha256": hex(&Sha256::digest(&actual)),
    }));

    Ok(())
}

#[allow(
    clippy::print_stdout,
    reason = "qualification retains the owned external history artifact"
)]
fn combined(replace: bool) -> TestResult {
    let _serial = process::process_scenario()?;

    let artifacts = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../artifacts/histories");

    std::fs::create_dir_all(&artifacts)?;
    let directory = TestDirectory::new_in(&artifacts)?.retain_on_failure();
    let path = directory.path().join("store");

    let history = History {
        clock: AtomicUsize::new(0),
        log: Mutex::new(File::create_new(directory.path().join("history.jsonl"))?),
    };

    history.event(&serde_json::json!({
        "event": "fixture", "replace": replace,
        "scope": "real peer process; committed publication with unavailable witness; held reader, peer mutation, close and explicit production GC batches; not power-loss evidence",
        "payloads": (1..=3).map(|value| serde_json::json!({
            "value": value, "length": bytes(value).len(),
            "sha256": hex(&Sha256::digest(bytes(value))),
        })).collect::<Vec<_>>(),
    }));

    runtime()?.block_on(async {
        let owner = Connection::open(path.clone(), None).await?;
        let peer = Connection::open(path.clone(), None).await?;
        owner.pause_maintenance();
        peer.pause_maintenance();

        owner
            .create_bucket(BucketName::parse("attachments")?)
            .await?;

        owner.put_object(put("selected"), bytes(1)).await?;
        let (held, predecessor, mut body) = select(&owner, 0, 1, &history).await?;
        let mut entries = vec![held];
        let mutation = Action::Put(2, Condition::Any);
        let invoked = history.begin(1, mutation);

        let mut writer = process::Probe::start(&path, "unavailable:publication.witness.before", WRITER)?;

        assert_eq!(writer.observation_async().await?, "CHECKPOINT");

        history.event(&serde_json::json!({
            "event": "checkpoint", "operation_invoked": invoked,
            "point": "publication.witness.before", "prior_effect": "publication.commit.after acknowledgment lost",
        }));

        let (observed, candidate, mut candidate_body) = select(&peer, 2, 2, &history).await?;

        entries.push(observed);
        verify_body(&mut candidate_body, 2, &history).await?;

        let action = if replace {
            Action::Put(3, Condition::Any)
        } else {
            Action::Delete(None)
        };

        let later = history.begin(2, action);

        if replace {
            peer.put_object(put("selected"), bytes(3)).await?;
        } else {
            peer.delete_object(read("selected"), None).await?;
        }

        entries.push(history.reply(2, action, later, Response::Success));
        let guards = GuardManager::open(&path)?;

        for blob in [predecessor.blob_id, candidate.blob_id] {
            super::super::super::maintenance::batch(
                Arc::new(peer.admit()?),
                path.clone(),
                blob.shard(),
            )
            .await?;

            assert!(
                guards
                    .try_acquire(GuardTarget::Shard(blob.shard()), GuardMode::Exclusive)?
                    .is_none()
            );

            assert!(payload_path(&path, blob).exists());

            history.event(&serde_json::json!({
                "event": "gc_blocked_by_live_owner", "blob_id": blob.to_string(),
            }));
        }

        verify_body(&mut body, 1, &history).await?;
        let closing = history.begin(0, Action::Close);
        owner.close().await?;
        entries.push(history.reply(0, Action::Close, closing, Response::Success));
        writer.finish_async().await?;
        assert_eq!(writer.observation_async().await?, "Unknown");
        entries.push(history.reply(1, mutation, invoked, Response::Unknown));

        if replace {
            let (entry, _, mut current) = select(&peer, 2, 3, &history).await?;
            entries.push(entry);
            verify_body(&mut current, 3, &history).await?;
        } else {
            let invoked = history.begin(2, Action::Get);
            let error = peer.get_object(read("selected")).await.unwrap_err();
            assert_eq!(error.failure(), OperationFailure::NoSuchKey);
            assert_eq!(error.outcome(), None);
            entries.push(history.reply(2, Action::Get, invoked, Response::MissingKey));
        }

        let initial = Model {
            bucket: true,
            object: Some(Object {
                value: 1,
                tag: None,
            }),
            ..Model::default()
        };

        let model = reconcile(initial, &entries).ok_or("impossible combined history")?;
        assert!(model.closed[0]);

        assert_eq!(
            model.object,
            replace.then_some(Object {
                value: 3,
                tag: None
            })
        );

        // A separately recorded stale observation is a sensitive oracle control.
        // Do not alter the actual store, observed history or original results.
        let mut invalid = entries.clone();

        invalid.last_mut().unwrap().response = Response::Object(Object {
            value: 1,
            tag: None,
        });

        assert!(reconcile(initial, &invalid).is_none());

        history.event(&serde_json::json!({
            "event": "negative_control", "kind": "stale read after acknowledged peer mutation",
            "history": invalid, "rejected": true,
        }));

        tokio::time::timeout(Duration::from_secs(10), async {
            loop {
                for blob in [predecessor.blob_id, candidate.blob_id] {
                    super::super::super::maintenance::batch(
                        Arc::new(peer.admit()?),
                        path.clone(),
                        blob.shard(),
                    )
                    .await?;
                }

                if [predecessor.blob_id, candidate.blob_id]
                    .iter()
                    .all(|blob| !payload_path(&path, *blob).exists())
                {
                    break;
                }

                tokio::time::sleep(Duration::from_millis(20)).await;
            }

            Ok::<_, OperationError>(())
        })
        .await??;

        peer.close().await?;

        for pass in 0..2 {
            let reopened = Connection::open(path.clone(), None).await?;

            if replace {
                let (_, _, mut body) = select(&reopened, 2, 3, &history).await?;
                verify_body(&mut body, 3, &history).await?;
            } else {
                let error = reopened.get_object(read("selected")).await.unwrap_err();
                assert_eq!(error.failure(), OperationFailure::NoSuchKey);
            }

            reopened.close().await?;

            let sql = rusqlite::Connection::open_with_flags(
                path.join("metadata.sqlite3"),
                rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY,
            )?;

            let (blobs, pending, references): (i64, i64, i64) = sql.query_row(
                "SELECT (SELECT count(*) FROM blobs),
                        (SELECT count(*) FROM blobs WHERE gc_pending = 1),
                        (SELECT count(*) FROM objects AS o JOIN blobs AS b
                         ON b.blob_id = o.blob_id WHERE b.state = 'ready')",
                [],
                |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
            )?;

            assert_eq!(
                (blobs, pending, references),
                (i64::from(replace), 0, i64::from(replace))
            );

            history.event(&serde_json::json!({
                "event": "recovery_fixed_point", "pass": pass,
                "blobs": blobs, "pending": pending, "ready_references": references,
            }));
        }

        Ok::<_, Box<dyn std::error::Error>>(())
    })?;

    history.event(&serde_json::json!({"event": "passed"}));
    drop(history);
    std::fs::remove_dir_all(&path)?;
    println!("HISTORY_ARTIFACT {}", directory.preserve().display());
    Ok(())
}

#[test]
fn unknown_publication_with_held_read_replacement_gc_and_close_matches_model() -> TestResult {
    combined(true)
}

#[test]
fn unknown_publication_with_held_read_deletion_gc_and_close_matches_model() -> TestResult {
    combined(false)
}
