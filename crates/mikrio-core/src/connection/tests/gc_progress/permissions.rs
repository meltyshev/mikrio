//! Real Unix unlink denial preserves bytes and advances durable retry state.

use super::*;
use std::error::Error as _;
use std::os::unix::fs::PermissionsExt as _;

struct PermissionRestore {
    path: PathBuf,
    permissions: std::fs::Permissions,
}

impl PermissionRestore {
    fn restrict(path: PathBuf) -> io::Result<Self> {
        let permissions = std::fs::metadata(&path)?.permissions();
        let restore = Self { path, permissions };
        std::fs::set_permissions(&restore.path, std::fs::Permissions::from_mode(0o500))?;
        Ok(restore)
    }

    fn restore(&self) -> io::Result<()> {
        std::fs::set_permissions(&self.path, self.permissions.clone())
    }
}

impl Drop for PermissionRestore {
    fn drop(&mut self) {
        if let Err(error) = self.restore() {
            process::trace(&format!("permission fixture cleanup failed: {error}"));
        }
    }
}

fn retry(path: &Path) -> rusqlite::Result<(String, i64, i64, String)> {
    let sql = rusqlite::Connection::open(path.join("metadata.sqlite3"))?;

    sql.query_row(
        "SELECT state, gc_attempt_count, gc_retry_at_ms, gc_error_code
         FROM blobs WHERE blob_id = ?1 AND gc_pending = 1",
        [blob(42).as_bytes().as_slice()],
        |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?)),
    )
}

#[test]
fn repeated_unlink_denial_preserves_retry_progress_and_recovers_after_permission_repair()
-> TestResult {
    let directory = TestDirectory::new()?.retain_on_failure();
    let path = directory.path().join("store");
    prepare(&path)?;
    let sentinel = path.join("objects/2a/operator-sentinel");
    std::fs::write(&sentinel, b"unowned evidence")?;

    runtime()?.block_on(async {
        // Protect the pending fixture through opening before disabling its own
        // scheduler. No other context runs maintenance during the manual cuts.
        let manager = GuardManager::open(&path)?;

        let protection = manager
            .try_acquire(GuardTarget::Shard(42), GuardMode::Shared)?
            .ok_or("fixture protection unavailable")?;

        let progress = manager
            .try_acquire(GuardTarget::Shard(44), GuardMode::Shared)?
            .ok_or("progress fixture protection unavailable")?;

        let connection = Connection::open(path.clone(), None).await?;
        pause_scheduler(&connection).await?;
        let permissions = PermissionRestore::restrict(path.join("objects/2a"))?;
        drop(protection);
        drop(progress);
        let before = crate::EpochMillis::from_system_time(std::time::SystemTime::now())?.get();

        let error = super::super::super::maintenance::batch(
            Arc::new(connection.admit()?),
            path.clone(),
            42,
        )
        .await
        .unwrap_err();

        assert_eq!(error.failure(), OperationFailure::IoError);

        assert_eq!(
            error
                .source()
                .and_then(|cause| cause.downcast_ref::<io::Error>())
                .map(io::Error::kind),
            Some(io::ErrorKind::PermissionDenied),
        );

        let first = retry(&path)?;

        assert_eq!(
            (first.0.as_str(), first.1, first.3.as_str()),
            ("deleting", 1, "CleanupFailed")
        );

        assert!(first.2 >= before + 1000);

        assert_eq!(
            std::fs::read(payload(&path, "objects", 42))?,
            b"contended bytes"
        );

        super::super::super::maintenance::batch(Arc::new(connection.admit()?), path.clone(), 44)
            .await?;

        assert_eq!(state(&path, 44)?, None);
        assert!(!payload(&path, "objects", 44).exists());

        tokio::time::timeout(Duration::from_secs(10), async {
            while crate::EpochMillis::from_system_time(std::time::SystemTime::now())?.get()
                < first.2
            {
                tokio::time::sleep(Duration::from_millis(10)).await;
            }

            Ok::<_, Box<dyn std::error::Error>>(())
        })
        .await??;

        let error = super::super::super::maintenance::batch(
            Arc::new(connection.admit()?),
            path.clone(),
            42,
        )
        .await
        .unwrap_err();

        assert_eq!(error.failure(), OperationFailure::IoError);

        assert_eq!(
            error
                .source()
                .and_then(|cause| cause.downcast_ref::<io::Error>())
                .map(io::Error::kind),
            Some(io::ErrorKind::PermissionDenied),
        );

        let second = retry(&path)?;

        assert_eq!(
            (second.0.as_str(), second.1, second.3.as_str()),
            ("deleting", 2, "CleanupFailed")
        );

        assert!(second.2 >= first.2 + 2000);

        assert_eq!(
            std::fs::read(payload(&path, "objects", 42))?,
            b"contended bytes"
        );

        assert_eq!(std::fs::read(&sentinel)?, b"unowned evidence");
        permissions.restore()?;

        connection
            .owner
            .state
            .context
            .test_hooks
            .pause_maintenance
            .store(false, Ordering::SeqCst);

        connection.owner.state.context.wake_maintenance.notify_one();

        tokio::time::timeout(Duration::from_secs(10), async {
            while state(&path, 42)?.is_some() {
                tokio::time::sleep(Duration::from_millis(10)).await;
            }

            Ok::<_, Box<dyn std::error::Error>>(())
        })
        .await??;

        assert!(!payload(&path, "objects", 42).exists());
        assert_eq!(std::fs::read(&sentinel)?, b"unowned evidence");
        drop(permissions);
        connection.close().await?;

        for _ in 0..2 {
            let reopened = Connection::open(path.clone(), None).await?;
            let (_, mut body) = reopened.get_object(read("protected")).await?;
            assert_eq!(body.read_all(64).await?, b"predecessor bytes");
            body.close().await?;
            reopened.close().await?;
            assert_eq!(state(&path, 42)?, None);
            assert_eq!(state(&path, 44)?, None);
            assert_eq!(std::fs::read(&sentinel)?, b"unowned evidence");
        }

        Ok::<_, Box<dyn std::error::Error>>(())
    })?;

    directory.finish()?;
    Ok(())
}
