//! Admitted listener work and stored readers retain precise failure boundaries.

use super::*;
use crate::connection::objects::{acquire, write::Output};
use crate::integrity::{PutIntegrity, PutIntegrityInput};
use crate::{BlobId, OperationFailure};
use std::error::Error as _;
use std::pin::Pin;
use std::task::Context as TaskContext;
use tokio::io::{AsyncRead, ReadBuf};

#[test]
fn listener_closed_before_interruption_wait_never_polls_inner_work() -> TestResult {
    let directory = TestDirectory::new()?.retain_on_failure();

    runtime()?.block_on(async {
        let connection = Connection::open(directory.path().join("store"), None).await?;
        connection.pause_maintenance();
        let listener = connection.reserve_listener()?;
        let operation = listener.connection().admit()?;
        listener.begin_close();
        let polls = AtomicUsize::new(0);

        let work = std::future::poll_fn(|_| {
            polls.fetch_add(1, Ordering::SeqCst);
            Poll::Ready(())
        });

        let error = operation.interrupted(work).await.unwrap_err();
        assert_eq!(error.failure(), OperationFailure::OperationCanceled);
        assert_eq!(error.outcome(), None);
        assert_eq!(polls.load(Ordering::SeqCst), 0);
        assert!(!*operation.cancellation.borrow());
        assert!(!*operation.state.closing.borrow());
        drop(operation);
        listener.finish().await?;
        drop(listener);

        let direct = connection.admit()?;
        assert_eq!(direct.interrupted(std::future::ready(17)).await?, 17);
        drop(direct);
        connection.close().await?;
        Ok::<_, Box<dyn std::error::Error>>(())
    })?;

    directory.finish()?;
    Ok(())
}

#[test]
fn listener_close_wakes_pending_work_before_a_second_inner_poll() -> TestResult {
    let directory = TestDirectory::new()?.retain_on_failure();

    runtime()?.block_on(async {
        let connection = Connection::open(directory.path().join("store"), None).await?;
        connection.pause_maintenance();
        let listener = connection.reserve_listener()?;
        let operation = listener.connection().admit()?;
        let control = listener.control();
        let polls = AtomicUsize::new(0);

        let work = std::future::poll_fn(|_| {
            assert_eq!(polls.fetch_add(1, Ordering::SeqCst), 0);
            control.close();
            Poll::<()>::Pending
        });

        let error = tokio::time::timeout(
            std::time::Duration::from_secs(1),
            operation.interrupted(work),
        )
        .await?
        .unwrap_err();

        assert_eq!(error.failure(), OperationFailure::OperationCanceled);
        assert_eq!(error.outcome(), None);
        assert_eq!(polls.load(Ordering::SeqCst), 1);
        assert!(!*operation.cancellation.borrow());
        assert!(!*operation.state.closing.borrow());
        drop(operation);
        listener.finish().await?;
        drop(listener);

        let replacement = connection.reserve_listener()?;
        replacement.ready()?;
        replacement.finish().await?;
        drop(replacement);
        connection.close().await?;
        Ok::<_, Box<dyn std::error::Error>>(())
    })?;

    directory.finish()?;
    Ok(())
}

struct BareReadFailure;

impl AsyncRead for BareReadFailure {
    fn poll_read(
        self: Pin<&mut Self>,
        _context: &mut TaskContext<'_>,
        _buffer: &mut ReadBuf<'_>,
    ) -> Poll<io::Result<()>> {
        Poll::Ready(Err(io::Error::from(io::ErrorKind::UnexpectedEof)))
    }
}

#[test]
fn stored_read_error_without_inner_cause_does_not_publish_payload() -> TestResult {
    let directory = TestDirectory::new()?.retain_on_failure();

    runtime()?.block_on(async {
        let path = directory.path().join("store");
        let connection = Connection::open(path.clone(), None).await?;
        connection.pause_maintenance();
        let operation = Arc::new(connection.admit()?);
        let blob_id = BlobId::from_bytes([0x63; 16]);
        let guard = acquire(Arc::clone(&operation), path.clone(), blob_id).await?;

        let output = Output {
            operation: Arc::clone(&operation),
            path: path.clone(),
            blob_id,
            guard: Arc::clone(&guard),
        };

        assert!(output.available().await?);
        let verifier = PutIntegrity::new(PutIntegrityInput::default())?.verifier(None)?;

        let Err(error) = output.publish_stored(BareReadFailure, verifier).await else {
            return Err("failed stored reader unexpectedly published".into());
        };

        assert_eq!(error.failure(), OperationFailure::IoError);
        assert_eq!(error.outcome(), None);
        assert!(error.source().is_none());
        let name = format!("{:02x}/{}", blob_id.shard(), blob_id);
        assert!(!path.join("objects").join(&name).try_exists()?);

        assert_eq!(
            std::fs::metadata(path.join("staging").join(&name))?.len(),
            0
        );

        drop(guard);
        drop(operation);
        connection.close().await?;
        Ok::<_, Box<dyn std::error::Error>>(())
    })?;

    directory.finish()?;
    Ok(())
}

#[cfg(unix)]
#[test]
fn candidate_symlink_is_corrupt_and_does_not_change_its_target() -> TestResult {
    let directory = TestDirectory::new()?.retain_on_failure();

    runtime()?.block_on(async {
        let path = directory.path().join("store");
        let connection = Connection::open(path.clone(), None).await?;
        connection.pause_maintenance();
        let operation = Arc::new(connection.admit()?);
        let blob_id = BlobId::from_bytes([0x74; 16]);
        let guard = acquire(Arc::clone(&operation), path.clone(), blob_id).await?;
        let target = directory.path().join("untouched-target");
        std::fs::write(&target, b"independent target bytes")?;

        let candidate = path
            .join("staging")
            .join(format!("{:02x}", blob_id.shard()))
            .join(blob_id.to_string());

        std::os::unix::fs::symlink(&target, &candidate)?;

        let output = Output {
            operation: Arc::clone(&operation),
            path: path.clone(),
            blob_id,
            guard,
        };

        let error = output.available().await.unwrap_err();
        assert_eq!(error.failure(), OperationFailure::StoreCorrupt);
        assert_eq!(error.outcome(), None);

        assert!(
            error
                .source()
                .and_then(|cause| cause.downcast_ref::<io::Error>())
                .is_some()
        );

        assert_eq!(std::fs::read(&target)?, b"independent target bytes");
        assert_eq!(std::fs::read_link(&candidate)?, target);

        assert!(
            !path
                .join("objects")
                .join(format!("{:02x}", blob_id.shard()))
                .join(blob_id.to_string())
                .try_exists()?
        );

        std::fs::remove_file(&candidate)?;
        assert!(output.available().await?);
        drop(output);
        drop(operation);
        connection.close().await?;
        Ok::<_, Box<dyn std::error::Error>>(())
    })?;

    directory.finish()?;
    Ok(())
}
