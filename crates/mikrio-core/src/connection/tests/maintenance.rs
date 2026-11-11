use super::*;
use crate::{EpochMillis, UploadId};
use std::path::Path;

fn receipts(path: &Path) -> rusqlite::Result<i64> {
    let connection = rusqlite::Connection::open(path.join("metadata.sqlite3"))?;

    connection.query_row("SELECT count(*) FROM multipart_receipts", [], |row| {
        row.get(0)
    })
}

fn pending(path: &Path, shard: u8) -> rusqlite::Result<i64> {
    let connection = rusqlite::Connection::open(path.join("metadata.sqlite3"))?;

    connection.query_row(
        "SELECT count(*) FROM blobs WHERE shard = ?1 AND gc_pending = 1",
        [i64::from(shard)],
        |row| row.get(0),
    )
}

#[test]
fn pending_shards_progress_past_a_reader_and_drain_without_empty_rotations() -> TestResult {
    let directory = TestDirectory::new()?;

    runtime()?.block_on(async {
        let path = directory.path().join("store");
        let connection = Connection::open(path.clone(), None).await?;
        connection.pause_maintenance();

        let guard = GuardManager::open(&path)?
            .try_acquire(GuardTarget::Shard(42), GuardMode::Shared)?
            .ok_or("owned reader guard is unavailable")?;

        // These valid reservations model dead writers before staging creation.
        // Use the engine's bundled SQLite; no second library shares this process.
        let mut sql = rusqlite::Connection::open(path.join("metadata.sqlite3"))?;
        let transaction = sql.transaction()?;

        for (shard, count) in [(42u8, 64u8), (43, 1)] {
            for suffix in 0..count {
                let mut blob = [0u8; 16];
                blob[0] = shard;
                blob[15] = suffix;

                transaction.execute(
                    "INSERT INTO blobs (blob_id, shard, state, write_kind,
                     target_bucket_id, target_key, created_at_ms, gc_pending, gc_attempt_count)
                     VALUES (?1, ?2, 'reserved', 'put_object', zeroblob(16), x'61', 0, 1, 0)",
                    rusqlite::params![blob.as_slice(), i64::from(shard)],
                )?;
            }
        }

        transaction.commit()?;
        sql.close().map_err(|(_, error)| error)?;

        connection
            .owner
            .state
            .context
            .test_hooks
            .pause_maintenance
            .store(false, Ordering::SeqCst);

        connection.owner.state.context.wake_maintenance.notify_one();

        tokio::time::timeout(Duration::from_secs(10), async {
            while pending(&path, 43)? != 0 {
                tokio::time::sleep(Duration::from_millis(20)).await;
            }

            Ok::<_, rusqlite::Error>(())
        })
        .await??;

        // Observed reclamation on another shard proves maintenance attempted work.
        assert_eq!(pending(&path, 42)?, 64);
        drop(guard);

        tokio::time::timeout(Duration::from_secs(10), async {
            while pending(&path, 42)? != 0 {
                tokio::time::sleep(Duration::from_millis(20)).await;
            }

            Ok::<_, rusqlite::Error>(())
        })
        .await??;

        connection.close().await?;
        Ok::<_, Box<dyn std::error::Error>>(())
    })?;

    directory.finish()?;
    Ok(())
}

#[test]
fn idle_cleanup_reports_bounded_retained_bytes_and_actual_guard_contention() -> TestResult {
    let directory = TestDirectory::new()?;

    runtime()?.block_on(async {
        let path = directory.path().join("secret-store-path");
        let connection = Connection::open(path.clone(), None).await?;
        connection.pause_maintenance();
        let receiver = connection.diagnostic_receiver();

        let guard = GuardManager::open(&path)?
            .try_acquire(GuardTarget::Shard(42), GuardMode::Shared)?
            .ok_or("owned reader guard is unavailable")?;

        let mut sql = rusqlite::Connection::open(path.join("metadata.sqlite3"))?;
        let transaction = sql.transaction()?;

        for suffix in 0..17u8 {
            let mut blob = [0u8; 16];
            blob[0] = 42;
            blob[15] = suffix;

            transaction.execute(
                "INSERT INTO blobs (blob_id, shard, state, write_kind,
                 target_bucket_id, target_key, created_at_ms, gc_pending, gc_attempt_count)
                 VALUES (?1, 42, 'reserved', 'put_object', zeroblob(16), x'61', 0, 1, 0)",
                [blob.as_slice()],
            )?;

            // Seed valid interrupted reservations with independently known
            // physical bytes. The observer must not rely on NULL ledger lengths.
            let blob = crate::BlobId::from_bytes(blob);

            std::fs::write(
                path.join("staging/2a").join(blob.to_string()),
                b"private-body-canary",
            )?;
        }

        transaction.commit()?;
        sql.close().map_err(|(_, error)| error)?;

        connection
            .owner
            .state
            .context
            .test_hooks
            .pause_maintenance
            .store(false, Ordering::SeqCst);

        connection.owner.state.context.wake_maintenance.notify_one();

        let record = tokio::time::timeout(Duration::from_secs(15), async {
            loop {
                if let Some((record, _)) = receiver.take()
                    && record.cleanup().is_some()
                {
                    break record;
                }

                tokio::time::sleep(Duration::from_millis(10)).await;
            }
        })
        .await?;

        let sample = record.cleanup().ok_or("missing cleanup observation")?;
        assert_eq!(sample.sampled_blobs, 16);
        assert_eq!(sample.observed_file_bytes, 16 * 19);
        assert!(sample.oldest_sample_blob_age_ms.is_some_and(|age| age > 0));
        assert_eq!(sample.unobserved_blobs, 0);
        assert!(sample.more_pending);
        assert!(sample.blocked_shard_attempts > 0);
        assert!(!format!("{record:?}").contains("secret-store-path"));
        assert!(!format!("{record:?}").contains("private-body-canary"));
        assert_eq!(pending(&path, 42)?, 17);
        connection.close().await?;
        assert!(receiver.is_closed());
        drop(guard);
        Ok::<_, Box<dyn std::error::Error>>(())
    })?;

    directory.finish()?;
    Ok(())
}

#[test]
fn receipt_scheduler_skips_protected_output_and_prunes_after_guard_release() -> TestResult {
    let directory = TestDirectory::new()?;

    runtime()?.block_on(async {
        let path = directory.path().join("store");
        let connection = Connection::open(path.clone(), None).await?;
        let peer = Connection::open(path.clone(), None).await?;
        connection.pause_maintenance();

        let guard = GuardManager::open(&path)?
            .try_acquire(GuardTarget::Shard(8), GuardMode::Shared)?
            .unwrap();

        let protected = UploadId::new(EpochMillis::new(0)?, [8; 16]).to_string();
        let unclaimed = UploadId::new(EpochMillis::new(0)?, [9; 16]).to_string();
        let sql = rusqlite::Connection::open(path.join("metadata.sqlite3"))?;

        sql.execute(
            "INSERT INTO multipart_receipts (upload_id, bucket_id, key, outcome, completion_claim_id,
             completion_blob_id, completion_parts_digest, finished_at_ms)
             VALUES (?1, zeroblob(16), x'61', 'aborted', zeroblob(16), ?2, zeroblob(32), 0)",
            rusqlite::params![protected, [8u8; 16].as_slice()],
        )?;

        sql.execute(
            "INSERT INTO multipart_receipts (upload_id, bucket_id, key, outcome, finished_at_ms)
             VALUES (?1, zeroblob(16), x'62', 'aborted', 0)",
            [&unclaimed],
        )?;

        sql.close().map_err(|(_, error)| error)?;

        connection
            .owner
            .state
            .context
            .test_hooks
            .pause_maintenance
            .store(false, Ordering::SeqCst);

        connection.owner.state.context.wake_maintenance.notify_one();

        tokio::time::timeout(Duration::from_secs(10), async {
            while receipts(&path)? != 1 {
                tokio::time::sleep(Duration::from_millis(20)).await;
            }

            Ok::<_, rusqlite::Error>(())
        })
        .await??;

        let sql = rusqlite::Connection::open(path.join("metadata.sqlite3"))?;

        assert_eq!(
            sql.query_row("SELECT upload_id FROM multipart_receipts", [], |row| {
                row.get::<_, String>(0)
            })?,
            protected
        );

        sql.close().map_err(|(_, error)| error)?;
        connection.close().await?;
        assert_eq!(receipts(&path)?, 1);
        drop(guard);

        tokio::time::timeout(Duration::from_secs(10), async {
            while receipts(&path)? != 0 {
                tokio::time::sleep(Duration::from_millis(20)).await;
            }

            Ok::<_, rusqlite::Error>(())
        })
        .await??;

        peer.close().await?;
        Ok::<_, Box<dyn std::error::Error>>(())
    })?;

    directory.finish()?;
    Ok(())
}
