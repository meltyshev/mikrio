//! Real context lifetime and ledger refusals fence physical reclamation.

use super::*;
use crate::GuardManager;
use crate::connection::Connection;
use crate::integrity::{PutIntegrity, PutIntegrityInput};
use crate::object::{
    BucketName, ObjectKey, PutRequest, ReadConditions, ReadConditionsInput, ReadRequest,
    Representation, UserMetadata, WriteConditions,
};
use crate::test_directory::TestDirectory;
use std::io;

type TestResult = Result<(), Box<dyn std::error::Error>>;

fn runtime() -> io::Result<tokio::runtime::Runtime> {
    tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
}

fn put() -> PutRequest {
    PutRequest {
        bucket: BucketName::parse("attachments").unwrap(),
        key: ObjectKey::parse("selected").unwrap(),
        expected_owner: None,
        content_length: None,
        representation: Representation::default(),
        metadata: UserMetadata::default(),
        tags: None,
        integrity: PutIntegrity::new(PutIntegrityInput::default()).unwrap(),
        conditions: WriteConditions::new(None, None).unwrap(),
    }
}

fn read() -> ReadRequest {
    ReadRequest {
        bucket: BucketName::parse("attachments").unwrap(),
        key: ObjectKey::parse("selected").unwrap(),
        expected_owner: None,
        conditions: ReadConditions::new(ReadConditionsInput::default()).unwrap(),
        range: None,
        part_number: None,
        response: Representation::default(),
    }
}

#[test]
fn dropped_context_settles_scheduler_without_terminal_signal_or_store_mutation() -> TestResult {
    let directory = TestDirectory::new()?.retain_on_failure();

    runtime()?.block_on(async {
        let path = directory.path().join("store");
        let connection = Connection::open(path.clone(), None).await?;

        connection
            .owner
            .state
            .context
            .test_hooks
            .pause_maintenance
            .store(true, std::sync::atomic::Ordering::SeqCst);

        connection
            .create_bucket(BucketName::parse("attachments")?)
            .await?;

        connection
            .put_object(put(), b"acknowledged live bytes".as_slice())
            .await?;

        let facts = connection.head_object(read()).await?;
        let context = Arc::downgrade(&connection.owner.state.context);
        connection.close().await?;
        drop(connection);
        assert!(context.upgrade().is_none());
        let database = std::fs::read(path.join("metadata.sqlite3"))?;
        let marker = std::fs::read(path.join("store.json"))?;

        let payload = path
            .join("objects")
            .join(format!("{:02x}", facts.blob_id.shard()))
            .join(facts.blob_id.to_string());

        let bytes = std::fs::read(&payload)?;
        assert_eq!(bytes, b"acknowledged live bytes");
        let (terminal, receiver) = watch::channel(None);
        let wake = Arc::new(Notify::new());
        wake.notify_one();

        // The real scheduler cannot obtain a context. The sender stays open and
        // un-signaled, so neither terminal channel exit may satisfy this test.
        tokio::time::timeout(
            Duration::from_secs(2),
            schedule(context, path.clone(), receiver, wake),
        )
        .await?;

        assert!(terminal.borrow().is_none());
        assert!(terminal.is_closed());
        assert_eq!(std::fs::read(path.join("metadata.sqlite3"))?, database);
        assert_eq!(std::fs::read(path.join("store.json"))?, marker);
        assert_eq!(std::fs::read(&payload)?, bytes);
        let manager = GuardManager::open(&path)?;

        let guard = manager
            .try_acquire(
                GuardTarget::Shard(facts.blob_id.shard()),
                GuardMode::Exclusive,
            )?
            .ok_or("scheduler retained a shard guard")?;

        drop(guard);
        let reopened = Connection::open(path, None).await?;
        let (_, mut body) = reopened.get_object(read()).await?;
        assert_eq!(body.read_all(64).await?, b"acknowledged live bytes");
        reopened.close().await?;
        Ok::<_, Box<dyn std::error::Error>>(())
    })?;

    directory.finish()?;
    Ok(())
}

#[test]
fn live_blob_refusal_clears_stale_hint_without_unlink_and_survives_reopen() -> TestResult {
    let directory = TestDirectory::new()?.retain_on_failure();

    runtime()?.block_on(async {
        let path = directory.path().join("store");
        let connection = Connection::open(path.clone(), None).await?;

        connection
            .owner
            .state
            .context
            .test_hooks
            .pause_maintenance
            .store(true, std::sync::atomic::Ordering::SeqCst);

        connection
            .create_bucket(BucketName::parse("attachments")?)
            .await?;

        connection
            .put_object(put(), b"acknowledged live bytes".as_slice())
            .await?;

        let facts = connection.head_object(read()).await?;

        let payload = path
            .join("objects")
            .join(format!("{:02x}", facts.blob_id.shard()))
            .join(facts.blob_id.to_string());

        let sql = rusqlite::Connection::open(path.join("metadata.sqlite3"))?;

        assert_eq!(
            sql.execute(
                "UPDATE blobs SET gc_pending = 1 WHERE blob_id = ?1",
                [facts.blob_id.as_bytes().as_slice()]
            )?,
            1
        );

        let operation = Arc::new(connection.owner.state.admit()?);

        let guard = Arc::new(
            GuardManager::open(&path)?
                .try_acquire(
                    GuardTarget::Shard(facts.blob_id.shard()),
                    GuardMode::Exclusive,
                )?
                .ok_or("owned exclusive shard guard unavailable")?,
        );

        // The original collect() sends a real Mark command to the live worker.
        // The metadata transaction sees the actual object reference and refuses
        // deletion, despite the deliberately stale gc_pending hint.
        collect(
            Arc::clone(&operation),
            Arc::clone(&guard),
            path.clone(),
            facts.blob_id,
        )
        .await?;

        assert_eq!(std::fs::read(&payload)?, b"acknowledged live bytes");

        let row: (String, i64) = sql.query_row(
            "SELECT state, gc_pending FROM blobs WHERE blob_id = ?1",
            [facts.blob_id.as_bytes().as_slice()],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )?;

        assert_eq!(row, ("ready".to_owned(), 0));

        let referenced: i64 = sql.query_row(
            "SELECT count(*) FROM objects WHERE blob_id = ?1",
            [facts.blob_id.as_bytes().as_slice()],
            |row| row.get(0),
        )?;

        assert_eq!(referenced, 1);
        drop(guard);
        drop(operation);
        sql.close().map_err(|(_, error)| error)?;
        let (_, mut body) = connection.get_object(read()).await?;
        assert_eq!(body.read_all(64).await?, b"acknowledged live bytes");
        connection.close().await?;
        let reopened = Connection::open(path, None).await?;
        let (_, mut body) = reopened.get_object(read()).await?;
        assert_eq!(body.read_all(64).await?, b"acknowledged live bytes");
        reopened.close().await?;
        Ok::<_, Box<dyn std::error::Error>>(())
    })?;

    directory.finish()?;
    Ok(())
}

#[test]
fn referenced_deleting_blob_refuses_physical_collection_and_preserves_evidence() -> TestResult {
    let directory = TestDirectory::new()?.retain_on_failure();

    runtime()?.block_on(async {
        let path = directory.path().join("store");
        let connection = Connection::open(path.clone(), None).await?;
        connection.pause_maintenance();

        connection
            .create_bucket(BucketName::parse("attachments")?)
            .await?;

        connection
            .put_object(put(), b"acknowledged live bytes".as_slice())
            .await?;

        let facts = connection.head_object(read()).await?;

        let payload = path
            .join("objects")
            .join(format!("{:02x}", facts.blob_id.shard()))
            .join(facts.blob_id.to_string());

        let sql = rusqlite::Connection::open(path.join("metadata.sqlite3"))?;

        assert_eq!(
            sql.execute(
                "UPDATE blobs SET state = 'deleting', gc_pending = 1 WHERE blob_id = ?1",
                [facts.blob_id.as_bytes().as_slice()],
            )?,
            1
        );

        let operation = Arc::new(connection.owner.state.admit()?);

        let guard = Arc::new(
            GuardManager::open(&path)?
                .try_acquire(
                    GuardTarget::Shard(facts.blob_id.shard()),
                    GuardMode::Exclusive,
                )?
                .ok_or("owned exclusive shard guard unavailable")?,
        );

        for _ in 0..2 {
            let error = collect(
                Arc::clone(&operation),
                Arc::clone(&guard),
                path.clone(),
                facts.blob_id,
            )
            .await
            .unwrap_err();

            assert_eq!(error.failure(), OperationFailure::StoreCorrupt);
            assert_eq!(std::fs::read(&payload)?, b"acknowledged live bytes");

            let row: (String, i64, i64) = sql.query_row(
                "SELECT state, gc_pending, (SELECT count(*) FROM objects WHERE blob_id = ?1)
                 FROM blobs WHERE blob_id = ?1",
                [facts.blob_id.as_bytes().as_slice()],
                |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
            )?;

            assert_eq!(row, ("deleting".to_owned(), 1, 1));
        }

        drop(guard);
        drop(operation);
        sql.close().map_err(|(_, error)| error)?;
        connection.close().await?;
        assert_eq!(std::fs::read(&payload)?, b"acknowledged live bytes");
        Ok::<_, Box<dyn std::error::Error>>(())
    })?;

    directory.finish()?;
    Ok(())
}

#[test]
fn corrupt_gc_candidate_preserves_its_files_and_does_not_block_the_same_shard() -> TestResult {
    let directory = TestDirectory::new()?.retain_on_failure();

    runtime()?.block_on(async {
        let path = directory.path().join("store");
        let connection = Connection::open(path.clone(), None).await?;

        connection
            .owner
            .state
            .context
            .test_hooks
            .pause_maintenance
            .store(true, std::sync::atomic::Ordering::SeqCst);

        let sql = rusqlite::Connection::open(path.join("metadata.sqlite3"))?;
        let corrupt = BlobId::from_bytes([8; 16]);
        let eligible = BlobId::from_bytes([7; 16]);

        // Both rows are in shard 7's page, but the first identity belongs to 8.
        // It must never authorize file work under either shard's guard.
        for blob in [corrupt, eligible] {
            sql.execute(
                "INSERT INTO blobs (blob_id, shard, state, write_kind, target_bucket_id,
                 target_key, created_at_ms, gc_pending, gc_attempt_count)
                 VALUES (?1, 7, 'reserved', 'put_object', zeroblob(16), x'61', 0, 1, 0)",
                [blob.as_bytes().as_slice()],
            )?;
        }

        let misplaced = path.join("staging/07").join(corrupt.to_string());
        let protected = path.join("staging/08").join(corrupt.to_string());
        let abandoned = path.join("staging/07").join(eligible.to_string());
        std::fs::write(&misplaced, b"preserve misplaced evidence")?;
        std::fs::write(&protected, b"preserve other shard")?;
        std::fs::write(&abandoned, b"abandoned reservation")?;
        let operation = Arc::new(connection.owner.state.admit()?);

        let error = batch(Arc::clone(&operation), path.clone(), 7)
            .await
            .unwrap_err();

        assert_eq!(error.failure(), OperationFailure::StoreCorrupt);

        assert!(
            !abandoned.exists(),
            "valid work behind corruption must progress"
        );

        assert_eq!(std::fs::read(&misplaced)?, b"preserve misplaced evidence");
        assert_eq!(std::fs::read(&protected)?, b"preserve other shard");

        let remaining: Vec<u8> =
            sql.query_row("SELECT blob_id FROM blobs", [], |row| row.get(0))?;

        assert_eq!(remaining, corrupt.as_bytes());

        let cursor: i64 = sql.query_row(
            "SELECT cursor_row_id FROM maintenance_sweeps WHERE task = 'gc_blobs' AND shard = 7",
            [],
            |row| row.get(0),
        )?;

        assert_eq!(cursor, 2);

        // A later sweep retries the retained corruption rather than hiding it.
        let error = batch(Arc::clone(&operation), path.clone(), 7)
            .await
            .unwrap_err();

        assert_eq!(error.failure(), OperationFailure::StoreCorrupt);
        assert_eq!(std::fs::read(&misplaced)?, b"preserve misplaced evidence");
        assert_eq!(std::fs::read(&protected)?, b"preserve other shard");
        drop(operation);
        sql.close().map_err(|(_, error)| error)?;
        connection.close().await?;
        Ok::<_, Box<dyn std::error::Error>>(())
    })?;

    directory.finish()?;
    Ok(())
}
