//! Actual metadata-thread unwind before/after durable reservation/publication.
//! The parent process owns the predecessor oracle and the failing child's exit.

use std::io::BufRead as _;
use std::sync::atomic::AtomicBool;

use super::objects::{put, read};
use super::*;
use crate::object::BucketName;
use crate::test_process as process;
use crate::{MutationOutcome, OperationFailure};

const CHILD: &str = "connection::tests::worker_unwind::metadata_unwind_child";
static FIRED: AtomicBool = AtomicBool::new(false);

fn announce(value: &str) -> io::Result<()> {
    let mut output = io::stdout().lock();
    writeln!(output, "PROBE {value}")?;
    output.flush()
}

#[allow(
    clippy::panic,
    reason = "owned child unwinds the actual metadata worker"
)]
fn checkpoint(point: &str) -> io::Result<()> {
    let target = std::env::var("mikrio_probe_mode").map_err(io::Error::other)?;

    if target != point || FIRED.swap(true, Ordering::SeqCst) {
        return Ok(());
    }

    assert_eq!(std::thread::current().name(), Some("mikrio-metadata"));
    announce("CHECKPOINT")?;
    let mut line = String::new();
    io::stdin().lock().read_line(&mut line)?;

    if line != "exit\n" {
        return Err(io::Error::other("owned unwind control pipe closed"));
    }

    panic!("controlled metadata worker unwind at {point}");
}

fn expected(point: &str) -> &'static [u8] {
    if point == "publication.commit.after" {
        b"replacement bytes"
    } else {
        b"predecessor"
    }
}

fn exercise(point: &str) -> TestResult {
    let _serial = process::process_scenario()?;
    let directory = TestDirectory::new()?.retain_on_failure();

    runtime()?.block_on(async {
        let path = directory.path().join("store");
        let peer = Connection::open(path.clone(), None).await?;

        peer.create_bucket(BucketName::parse("attachments")?)
            .await?;

        peer.put_object(put("selected"), b"predecessor".as_slice())
            .await?;

        let mut child = process::Probe::start(&path, point, CHILD)?;
        assert_eq!(child.observation_async().await?, "CHECKPOINT");
        let (_, mut body) = peer.get_object(read("selected")).await?;
        assert_eq!(body.read_all(100).await?, expected(point));
        body.close().await?;
        child.finish_async().await?;
        assert_eq!(child.observation_async().await?, "CONTAINED");

        // A failed context cannot invalidate another process's connection.
        let (_, mut body) = peer.get_object(read("selected")).await?;
        assert_eq!(body.read_all(100).await?, expected(point));
        body.close().await?;
        let (_, mut body) = peer.get_object(read("after-unwind")).await?;
        assert_eq!(body.read_all(100).await?, b"fresh context writes");
        body.close().await?;
        peer.close().await?;

        for _ in 0..2 {
            let reopened = Connection::open(path.clone(), None).await?;
            let (_, mut body) = reopened.get_object(read("selected")).await?;
            assert_eq!(body.read_all(100).await?, expected(point));
            body.close().await?;
            reopened.close().await?;
        }

        Ok::<_, Box<dyn std::error::Error>>(())
    })?;

    directory.finish()?;
    Ok(())
}

#[test]
fn reservation_before_commit_worker_unwind_is_contained() -> TestResult {
    exercise("reservation.commit.before")
}

#[test]
fn reservation_after_commit_worker_unwind_is_contained() -> TestResult {
    exercise("reservation.commit.after")
}

#[test]
fn publication_before_commit_worker_unwind_preserves_unknown() -> TestResult {
    exercise("publication.commit.before")
}

#[test]
fn publication_after_commit_worker_unwind_preserves_unknown() -> TestResult {
    exercise("publication.commit.after")
}

#[test]
#[ignore = "Owned subprocess entry for actual metadata-thread unwind"]
fn metadata_unwind_child() -> TestResult {
    let path =
        PathBuf::from(std::env::var_os("mikrio_probe_directory").ok_or("missing owned directory")?);

    let point = std::env::var("mikrio_probe_mode")?;

    runtime()?.block_on(async {
        let connection = Connection::open(path.clone(), None).await?;
        let context = Arc::clone(&connection.owner.state.context);
        crate::protocol_fault::install(checkpoint)?;

        let error = connection
            .put_object(put("selected"), b"replacement bytes".as_slice())
            .await
            .unwrap_err();

        assert!(FIRED.load(Ordering::SeqCst));
        assert_eq!(error.failure(), OperationFailure::IoError);

        assert_eq!(
            error.outcome(),
            Some(if point.starts_with("reservation.") {
                MutationOutcome::NotCommitted
            } else {
                MutationOutcome::Unknown
            })
        );

        let terminal = context.outcome().await.unwrap_err();

        let ConnectionError::ShutdownFailed(cause) = terminal else {
            return Err("worker did not publish its contained shutdown failure".into());
        };

        assert_eq!(cause.to_string(), "metadata worker panicked");

        for _ in 0..2 {
            assert!(matches!(
                connection.close().await,
                Err(ConnectionError::ShutdownFailed(_))
            ));
        }

        let rejected = connection
            .put_object(put("must-not-publish"), b"rejected".as_slice())
            .await
            .unwrap_err();

        assert_eq!(rejected.outcome(), Some(MutationOutcome::NotCommitted));
        let worker = context.worker.lock().unwrap().take().unwrap();

        tokio::time::timeout(Duration::from_secs(10), async {
            while !worker.is_finished() {
                tokio::task::yield_now().await;
            }
        })
        .await?;

        // Removing the production catch boundary makes the actual thread join
        // report a panic instead of this normal, explicitly failed shutdown.
        assert!(worker.join().is_ok());

        let reopened = Connection::open(path, None).await?;
        let (_, mut body) = reopened.get_object(read("selected")).await?;
        assert_eq!(body.read_all(100).await?, expected(&point));
        body.close().await?;

        assert_eq!(
            reopened
                .head_object(read("must-not-publish"))
                .await
                .unwrap_err()
                .failure(),
            OperationFailure::NoSuchKey
        );

        reopened
            .put_object(put("after-unwind"), b"fresh context writes".as_slice())
            .await?;

        reopened.close().await?;
        announce("CONTAINED")?;
        Ok::<_, Box<dyn std::error::Error>>(())
    })
}
