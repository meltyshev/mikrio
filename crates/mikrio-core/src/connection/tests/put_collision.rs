//! Colliding output identities cannot consume a body or replace another owner.

use super::objects::{put, read};
use super::*;
use crate::object::BucketName;
use crate::{BlobId, MutationOutcome, OperationFailure};
use std::pin::Pin;
use std::task::Context as TaskContext;
use tokio::io::{AsyncRead, ReadBuf};

struct ObservedSource(Arc<AtomicUsize>);

impl AsyncRead for ObservedSource {
    fn poll_read(
        self: Pin<&mut Self>,
        _context: &mut TaskContext<'_>,
        output: &mut ReadBuf<'_>,
    ) -> Poll<io::Result<()>> {
        if self.0.fetch_add(1, Ordering::SeqCst) == 0 {
            output.put_slice(b"x");
        }

        Poll::Ready(Ok(()))
    }
}

fn force(connection: &Connection, outputs: impl IntoIterator<Item = BlobId>) {
    let mut pending = connection
        .owner
        .state
        .context
        .test_hooks
        .put_outputs
        .lock()
        .unwrap();

    assert!(pending.is_empty());
    pending.extend(outputs);
}

fn all_selected(connection: &Connection) -> bool {
    connection
        .owner
        .state
        .context
        .test_hooks
        .put_outputs
        .lock()
        .unwrap()
        .is_empty()
}

fn reservation(
    sql: &rusqlite::Connection,
    blob: BlobId,
) -> rusqlite::Result<(String, Vec<u8>, i64, i64)> {
    sql.query_row(
        "SELECT state, target_key, gc_pending, gc_attempt_count FROM blobs WHERE blob_id = ?1",
        [blob.as_bytes().as_slice()],
        |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?)),
    )
}

fn collisions(database_collision: bool) -> TestResult {
    let directory = TestDirectory::new()?.retain_on_failure();

    runtime()?.block_on(async {
        let path = directory.path().join("store");
        let connection = Connection::open(path.clone(), None).await?;
        connection.pause_maintenance();

        connection
            .create_bucket(BucketName::parse("attachments")?)
            .await?;

        connection
            .put_object(put("selected"), b"acknowledged predecessor".as_slice())
            .await?;

        let predecessor = connection.head_object(read("selected")).await?;
        let collision = BlobId::from_bytes([0x72; 16]);
        let fresh = BlobId::from_bytes([0x73; 16]);
        assert_ne!(collision, predecessor.blob_id);
        assert_ne!(fresh, predecessor.blob_id);
        let sentinel = path.join("staging/72").join(collision.to_string());
        let sql = rusqlite::Connection::open(path.join("metadata.sqlite3"))?;

        if database_collision {
            sql.execute(
                "INSERT INTO blobs (blob_id, shard, state, write_kind, target_bucket_id,
                 target_key, created_at_ms, gc_pending, gc_attempt_count)
                 VALUES (?1, 114, 'reserved', 'put_object', zeroblob(16), x'73656e74696e656c', 0, 1, 0)",
                [collision.as_bytes().as_slice()],
            )?;

            assert!(!sentinel.exists());
        } else {
            let mut file = std::fs::OpenOptions::new()
                .write(true)
                .create_new(true)
                .open(&sentinel)?;

            file.write_all(b"another owner's retained input")?;
            file.sync_all()?;
        }

        let before: i64 = sql.query_row("SELECT COUNT(*) FROM blobs", [], |row| row.get(0))?;

        let preserved = if database_collision {
            Some(reservation(&sql, collision)?)
        } else {
            None
        };

        force(&connection, [collision; 4]);
        let reads = Arc::new(AtomicUsize::new(0));

        let error = connection
            .put_object(put("selected"), ObservedSource(Arc::clone(&reads)))
            .await
            .unwrap_err();

        assert_eq!(error.failure(), OperationFailure::IoError);
        assert_eq!(error.outcome(), Some(MutationOutcome::NotCommitted));
        assert_eq!(reads.load(Ordering::SeqCst), 0);
        assert!(all_selected(&connection));

        assert_eq!(
            connection.head_object(read("selected")).await?.blob_id,
            predecessor.blob_id
        );

        let (_, mut body) = connection.get_object(read("selected")).await?;
        assert_eq!(body.read_all(64).await?, b"acknowledged predecessor");
        let after: i64 = sql.query_row("SELECT COUNT(*) FROM blobs", [], |row| row.get(0))?;
        assert_eq!(after, before);

        // A collision followed by a new identity must make progress through the
        // real reservation, guarded write and publication paths.
        force(&connection, [collision, fresh]);

        connection
            .put_object(put("selected"), ObservedSource(Arc::clone(&reads)))
            .await?;

        assert!(all_selected(&connection));
        assert!(reads.load(Ordering::SeqCst) > 0);

        assert_eq!(
            connection.head_object(read("selected")).await?.blob_id,
            fresh
        );

        if let Some(preserved) = preserved {
            assert_eq!(reservation(&sql, collision)?, preserved);
            assert!(!sentinel.exists());
        } else {
            assert_eq!(std::fs::read(&sentinel)?, b"another owner's retained input");
        }

        drop(sql);
        connection.close().await?;

        let reopened = Connection::open(path, None).await?;
        let (_, mut body) = reopened.get_object(read("selected")).await?;
        assert_eq!(body.read_all(64).await?, b"x");
        reopened.close().await?;
        Ok::<_, Box<dyn std::error::Error>>(())
    })?;

    directory.finish()?;
    Ok(())
}

#[test]
fn filesystem_collision_preserves_bytes_and_retries_before_consuming_source() -> TestResult {
    collisions(false)
}

#[test]
fn reservation_collision_preserves_ledger_and_retries_before_consuming_source() -> TestResult {
    collisions(true)
}
