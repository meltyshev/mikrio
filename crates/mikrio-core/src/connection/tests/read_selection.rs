//! Peer mutations between preliminary lookup and guarded native selection.

use super::objects::{put, read};
use super::*;
use crate::OperationFailure;
use crate::object::BucketName;

#[derive(Clone, Copy)]
enum ReadStage {
    Selected,
    Guarded,
}

fn pause_at(
    connection: &Connection,
    stage: ReadStage,
) -> (oneshot::Receiver<()>, oneshot::Sender<()>) {
    let (selected, observed) = oneshot::channel();
    let (release, released) = oneshot::channel();
    let hooks = &connection.owner.state.context.test_hooks;

    let checkpoint = match stage {
        ReadStage::Selected => &hooks.after_read_selection,
        ReadStage::Guarded => &hooks.after_read_guard,
    };

    assert!(
        checkpoint
            .lock()
            .unwrap()
            .replace((selected, released))
            .is_none()
    );

    (observed, release)
}

fn pause(connection: &Connection) -> (oneshot::Receiver<()>, oneshot::Sender<()>) {
    pause_at(connection, ReadStage::Selected)
}

#[test]
fn held_get_preserves_selected_bytes_without_retaining_a_wal_snapshot() -> TestResult {
    let directory = TestDirectory::new()?.retain_on_failure();
    let path = directory.path().join("store");

    runtime()?.block_on(async {
        let reader = Connection::open(path.clone(), None).await?;
        let peer = Connection::open(path.clone(), None).await?;
        reader.pause_maintenance();
        peer.pause_maintenance();

        reader
            .create_bucket(BucketName::parse("attachments")?)
            .await?;

        let original = vec![0x37; 96 * 1024];

        peer.put_object(put("selected"), io::Cursor::new(original.clone()))
            .await?;

        let (selected, mut body) = reader.get_object(read("selected")).await?;
        let mut received = body.read_chunk(1).await?.ok_or("missing first byte")?;

        // The observer uses the engine's bundled SQLite. Its deliberately held
        // snapshot is a positive control for the checkpoint progress assertion.
        let observer = rusqlite::Connection::open(path.join("metadata.sqlite3"))?;
        observer.execute_batch("BEGIN")?;

        let _: i64 = observer.query_row("SELECT count(*) FROM objects", [], |row| row.get(0))?;

        peer.put_object(put("selected"), b"replacement".as_slice())
            .await?;

        let checkpoint = || {
            observer.query_row("PRAGMA wal_checkpoint(PASSIVE)", [], |row| {
                Ok((
                    row.get::<_, i64>(0)?,
                    row.get::<_, i64>(1)?,
                    row.get::<_, i64>(2)?,
                ))
            })
        };

        // A second observer can checkpoint while the first holds a read
        // transaction; SQLite cannot checkpoint on that transaction itself.
        let checkpointer = rusqlite::Connection::open(path.join("metadata.sqlite3"))?;

        let (busy, pages, copied): (i64, i64, i64) =
            checkpointer.query_row("PRAGMA wal_checkpoint(PASSIVE)", [], |row| {
                Ok((row.get(0)?, row.get(1)?, row.get(2)?))
            })?;

        assert_eq!(busy, 0);

        assert!(
            pages > copied && copied >= 0,
            "held SQL snapshot must prevent full progress"
        );

        observer.execute_batch("ROLLBACK")?;
        checkpointer.close().map_err(|(_, error)| error)?;

        for round in 0..3 {
            let replacement = format!("replacement {round}");

            peer.put_object(put("selected"), io::Cursor::new(replacement.into_bytes()))
                .await?;

            let (busy, pages, copied) = checkpoint()?;
            assert_eq!(busy, 0);
            assert!(pages > 0);
            assert_eq!(copied, pages, "a selected GET must not pin WAL pages");

            received.extend(
                body.read_chunk(1)
                    .await?
                    .ok_or("selected body ended early")?,
            );

            let current = peer.head_object(read("selected")).await?;
            assert_ne!(current.generation, selected.generation);
            assert_ne!(current.blob_id, selected.blob_id);
        }

        received.extend(body.read_all(original.len()).await?);
        assert_eq!(received, original);
        body.close().await?;
        observer.close().map_err(|(_, error)| error)?;

        let (_, mut current) = peer.get_object(read("selected")).await?;
        assert_eq!(current.read_all(64).await?, b"replacement 2");
        current.close().await?;
        reader.close().await?;
        peer.close().await?;
        Ok::<_, Box<dyn std::error::Error>>(())
    })?;

    directory.finish()?;
    Ok(())
}

#[test]
fn deletion_before_guarded_native_selection_returns_no_such_key_without_a_body() -> TestResult {
    let directory = TestDirectory::new()?.retain_on_failure();
    let path = directory.path().join("store");

    runtime()?.block_on(async {
        let reader = Connection::open(path.clone(), None).await?;
        let peer = Connection::open(path.clone(), None).await?;
        reader.pause_maintenance();

        reader
            .create_bucket(BucketName::parse("attachments")?)
            .await?;

        peer.put_object(put("selected"), b"original bytes".as_slice())
            .await?;

        let original = peer.head_object(read("selected")).await?;
        let (observed, release) = pause(&reader);
        let owner = reader.clone();
        let pending = tokio::spawn(async move { owner.get_object(read("selected")).await });
        tokio::time::timeout(Duration::from_secs(10), observed).await??;
        peer.delete_object(read("selected"), None).await?;
        release.send(()).map_err(|()| "read selection ended")?;

        let error = tokio::time::timeout(Duration::from_secs(10), pending)
            .await??
            .err()
            .ok_or("deleted native selection returned a body")?;

        assert_eq!(error.failure(), OperationFailure::NoSuchKey);
        assert_eq!(error.outcome(), None);

        peer.probe(|_| Ok(())).await?;

        let guard = GuardManager::open(&path)?
            .try_acquire(
                GuardTarget::Shard(original.blob_id.shard()),
                GuardMode::Exclusive,
            )?
            .ok_or("failed selection retained its shard")?;

        drop(guard);

        super::super::maintenance::batch(
            Arc::new(peer.admit()?),
            path.clone(),
            original.blob_id.shard(),
        )
        .await?;

        assert!(
            !path
                .join("objects")
                .join(format!("{:02x}", original.blob_id.shard()))
                .join(original.blob_id.to_string())
                .exists()
        );

        peer.put_object(put("selected"), b"replacement bytes".as_slice())
            .await?;

        let (_, mut body) = reader.get_object(read("selected")).await?;
        assert_eq!(body.read_all(64).await?, b"replacement bytes");
        body.close().await?;
        reader.close().await?;
        peer.close().await?;
        Ok::<_, Box<dyn std::error::Error>>(())
    })?;

    directory.finish()?;
    Ok(())
}

#[test]
fn replacement_at_every_native_selection_exhausts_retries_and_releases_guards() -> TestResult {
    let directory = TestDirectory::new()?.retain_on_failure();
    let path = directory.path().join("store");

    runtime()?.block_on(async {
        let reader = Connection::open(path.clone(), None).await?;
        let peer = Connection::open(path.clone(), None).await?;
        reader.pause_maintenance();

        reader
            .create_bucket(BucketName::parse("attachments")?)
            .await?;

        peer.put_object(put("selected"), b"unchanged bytes".as_slice())
            .await?;

        let mut previous = peer.head_object(read("selected")).await?;
        let mut shards = std::collections::BTreeSet::from([previous.blob_id.shard()]);
        let (mut observed, mut release) = pause(&reader);
        let owner = reader.clone();
        let pending = tokio::spawn(async move { owner.get_object(read("selected")).await });

        for _ in 0..4 {
            tokio::time::timeout(Duration::from_secs(10), observed).await??;

            peer.put_object(put("selected"), b"unchanged bytes".as_slice())
                .await?;

            let current = peer.head_object(read("selected")).await?;
            assert_eq!(current.etag, previous.etag);
            assert_ne!(current.blob_id, previous.blob_id);
            assert_ne!(current.generation, previous.generation);
            shards.insert(current.blob_id.shard());
            previous = current;
            let next = pause(&reader);

            release
                .send(())
                .map_err(|()| "read selection ended before retry")?;

            (observed, release) = next;
        }

        let error = tokio::time::timeout(Duration::from_secs(10), pending)
            .await??
            .err()
            .ok_or("continuously replaced selection returned a body")?;

        assert_eq!(error.failure(), OperationFailure::StoreBusy);
        assert_eq!(error.outcome(), None);

        assert_eq!(
            observed.try_recv(),
            Err(oneshot::error::TryRecvError::Empty)
        );

        let unused = reader
            .owner
            .state
            .context
            .test_hooks
            .after_read_selection
            .lock()
            .unwrap()
            .take();

        assert!(unused.is_some(), "retry budget consumed another selection");
        drop(unused);
        drop(release);
        peer.probe(|_| Ok(())).await?;

        for shard in shards {
            let guard = GuardManager::open(&path)?
                .try_acquire(GuardTarget::Shard(shard), GuardMode::Exclusive)?
                .ok_or("exhausted selection retained its shard")?;

            drop(guard);
        }

        let (selected, mut body) = reader.get_object(read("selected")).await?;
        assert_eq!(selected.blob_id, previous.blob_id);
        assert_eq!(selected.generation, previous.generation);
        assert_eq!(body.read_all(64).await?, b"unchanged bytes");
        body.close().await?;
        reader.close().await?;
        peer.close().await?;
        Ok::<_, Box<dyn std::error::Error>>(())
    })?;

    directory.finish()?;
    Ok(())
}

#[test]
fn replaced_root_cannot_supply_payload_bytes_to_a_paused_reader() -> TestResult {
    replaced_root(ReadStage::Selected)
}

#[test]
fn replaced_root_after_guard_acquisition_cannot_supply_payload_bytes() -> TestResult {
    replaced_root(ReadStage::Guarded)
}

fn replaced_root(stage: ReadStage) -> TestResult {
    let directory = TestDirectory::new()?.retain_on_failure();
    let path = directory.path().join("store");
    let moved = directory.path().join("original");
    let replacement = directory.path().join("replacement");

    runtime()?.block_on(async {
        let connection = Connection::open(path.clone(), None).await?;
        connection.pause_maintenance();

        connection
            .create_bucket(BucketName::parse("attachments")?)
            .await?;

        connection
            .put_object(put("selected"), b"original bytes".as_slice())
            .await?;

        let selected = connection.head_object(read("selected")).await?;
        let shard = format!("{:02x}", selected.blob_id.shard());

        let relative = std::path::Path::new("objects")
            .join(&shard)
            .join(selected.blob_id.to_string());

        let (observed, release) = pause_at(&connection, stage);
        let owner = connection.clone();
        let pending = tokio::spawn(async move { owner.get_object(read("selected")).await });
        tokio::time::timeout(Duration::from_secs(10), observed).await??;
        std::fs::rename(&path, &moved)?;
        std::fs::create_dir_all(path.join("locks"))?;
        std::fs::create_dir_all(path.join("objects").join(&shard))?;
        std::fs::File::create_new(path.join("locks").join(format!("{shard}.lock")))?;
        std::fs::write(path.join(&relative), b"impostor bytes")?;
        release.send(()).map_err(|()| "read selection ended")?;

        let result = tokio::time::timeout(Duration::from_secs(10), pending).await??;

        let (failure, bytes) = match result {
            Ok((_, mut body)) => {
                let bytes = body.read_all(64).await?;
                body.close().await?;
                (None, bytes)
            }
            Err(error) => (Some(error.failure()), Vec::new()),
        };

        // Restore the SQLite pathname before close, including on the failing
        // control path. Both independently owned directories remain available.
        std::fs::rename(&path, &replacement)?;
        std::fs::rename(&moved, &path)?;
        connection.close().await?;
        assert_eq!(failure, Some(OperationFailure::StoreCorrupt), "{bytes:?}");
        assert_eq!(bytes, Vec::<u8>::new());

        assert_eq!(
            std::fs::read(replacement.join(&relative))?,
            b"impostor bytes"
        );

        assert_eq!(std::fs::read(path.join(&relative))?, b"original bytes");

        let reopened = Connection::open(path.clone(), None).await?;
        let (_, mut body) = reopened.get_object(read("selected")).await?;
        assert_eq!(body.read_all(64).await?, b"original bytes");
        body.close().await?;
        reopened.close().await?;
        Ok::<_, Box<dyn std::error::Error>>(())
    })?;

    directory.finish()?;
    Ok(())
}

#[test]
fn replaced_root_refuses_publication_and_collection_without_touching_the_lookalike() -> TestResult {
    let directory = TestDirectory::new()?.retain_on_failure();
    let path = directory.path().join("store");
    let moved = directory.path().join("original");
    let replacement = directory.path().join("replacement");

    OpenedStore::open(&replacement, None)?
        .close()
        .map_err(|(_, error)| error)?;

    runtime()?.block_on(async {
        let connection = Connection::open(path.clone(), None).await?;
        connection.pause_maintenance();

        connection
            .create_bucket(BucketName::parse("attachments")?)
            .await?;

        connection
            .put_object(put("retired"), b"retired bytes".as_slice())
            .await?;

        let (retired, mut reader) = connection.get_object(read("retired")).await?;
        connection.delete_object(read("retired"), None).await?;
        reader.close().await?;
        connection.probe(|_| Ok(())).await?;

        let relative = std::path::Path::new("objects")
            .join(format!("{:02x}", retired.blob_id.shard()))
            .join(retired.blob_id.to_string());

        std::fs::write(replacement.join(&relative), b"unowned bytes")?;
        let before = crate::test_backup::manifest(&replacement)?;
        std::fs::rename(&path, &moved)?;
        std::fs::rename(&replacement, &path)?;

        let publication = connection
            .put_object(put("new"), b"unpublished bytes".as_slice())
            .await;

        let collection = super::super::maintenance::batch(
            Arc::new(connection.admit()?),
            path.clone(),
            retired.blob_id.shard(),
        )
        .await;

        let after = crate::test_backup::manifest(&path)?;
        std::fs::rename(&path, &replacement)?;
        std::fs::rename(&moved, &path)?;
        connection.close().await?;
        let publication = publication.unwrap_err();
        assert_eq!(publication.failure(), OperationFailure::StoreCorrupt);

        assert_eq!(
            publication.outcome(),
            Some(crate::MutationOutcome::NotCommitted)
        );

        assert_eq!(
            collection.unwrap_err().failure(),
            OperationFailure::StoreCorrupt
        );

        assert_eq!(before, after);
        assert_eq!(std::fs::read(path.join(&relative))?, b"retired bytes");

        assert_eq!(
            std::fs::read(replacement.join(&relative))?,
            b"unowned bytes"
        );

        Ok::<_, Box<dyn std::error::Error>>(())
    })?;

    directory.finish()?;
    Ok(())
}
