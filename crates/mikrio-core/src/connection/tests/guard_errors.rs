//! Failed native lock attempts never authorize payload access or source polling.

use std::error::Error as _;
use std::io::BufRead as _;
use std::pin::Pin;
use std::sync::OnceLock;
use std::sync::atomic::AtomicBool;
use std::task::Context as TaskContext;

use tokio::io::{AsyncRead, ReadBuf};

use super::objects::{put, read};
use super::*;
use crate::object::BucketName;
use crate::test_process as process;
use crate::{MutationOutcome, OperationError, OperationFailure};

const CHILD: &str = "connection::tests::guard_errors::lock_failure_child";
static FAILURE: OnceLock<io::ErrorKind> = OnceLock::new();
static ENABLED: AtomicBool = AtomicBool::new(true);
static LOCKS: AtomicUsize = AtomicUsize::new(0);
static OPENS: AtomicUsize = AtomicUsize::new(0);
static POLLS: AtomicUsize = AtomicUsize::new(0);

fn checkpoint(point: &str) -> io::Result<()> {
    if point == "read.open.before" {
        OPENS.fetch_add(1, Ordering::SeqCst);
    }

    if point == "guard.lock.before" && ENABLED.load(Ordering::SeqCst) {
        LOCKS.fetch_add(1, Ordering::SeqCst);

        return Err(io::Error::new(
            *FAILURE
                .get()
                .ok_or_else(|| io::Error::other("missing lock fault"))?,
            "owned lock acquisition failure",
        ));
    }

    Ok(())
}

struct ObservedSource;

impl AsyncRead for ObservedSource {
    fn poll_read(
        self: Pin<&mut Self>,
        _: &mut TaskContext<'_>,
        _: &mut ReadBuf<'_>,
    ) -> Poll<io::Result<()>> {
        POLLS.fetch_add(1, Ordering::SeqCst);
        Poll::Ready(Err(io::Error::other("source must remain unconsumed")))
    }
}

fn assert_lock_error(error: &OperationError, kind: io::ErrorKind) {
    assert_eq!(error.failure(), OperationFailure::IoError);

    let cause = error
        .source()
        .and_then(|cause| cause.downcast_ref::<io::Error>())
        .expect("native lock error retained");

    assert_eq!(cause.kind(), kind);
}

fn refusal(mode: &str) -> TestResult {
    let _serial = process::process_scenario()?;
    let directory = TestDirectory::new()?.retain_on_failure();
    let path = directory.path().join("store");

    runtime()?.block_on(async {
        let connection = Connection::open(path.clone(), None).await?;

        connection
            .create_bucket(BucketName::parse("attachments")?)
            .await?;

        connection
            .put_object(put("selected"), b"unchanged bytes".as_slice())
            .await?;

        connection.close().await?;
        Ok::<_, Box<dyn std::error::Error>>(())
    })?;

    let mut child = process::Probe::start(&path, mode, CHILD)?;
    assert_eq!(child.observation()?, "VERIFIED");
    child.finish()?;

    runtime()?.block_on(async {
        let connection = Connection::open(path.clone(), None).await?;
        let (_, mut body) = connection.get_object(read("selected")).await?;
        assert_eq!(body.read_all(64).await?, b"unchanged bytes");
        body.close().await?;
        connection.close().await?;
        Ok::<_, Box<dyn std::error::Error>>(())
    })?;

    directory.finish()?;
    Ok(())
}

#[test]
fn unsupported_lock_refuses_payload_access_and_preserves_the_source() -> TestResult {
    refusal("unsupported")
}

#[test]
fn lock_io_error_refuses_payload_access_and_preserves_the_source() -> TestResult {
    refusal("io")
}

#[test]
#[ignore = "Owned native lock-failure process with payload and source observations"]
fn lock_failure_child() -> TestResult {
    let path = PathBuf::from(std::env::var_os("mikrio_probe_directory").ok_or("missing store")?);

    let kind = match std::env::var("mikrio_probe_mode")?.as_str() {
        "unsupported" => io::ErrorKind::Unsupported,
        "io" => io::ErrorKind::Other,
        _ => return Err("unknown lock-failure mode".into()),
    };

    FAILURE
        .set(kind)
        .map_err(|_| "lock fault already installed")?;

    runtime()?.block_on(async {
        let connection = Connection::open(path.clone(), None).await?;
        connection.pause_maintenance();

        tokio::time::timeout(Duration::from_secs(10), async {
            while connection.owner.state.activity.lock().unwrap().operations != 0 {
                tokio::task::yield_now().await;
            }
        })
        .await?;

        crate::protocol_fault::install(checkpoint)?;

        let error = connection
            .get_object(read("selected"))
            .await
            .err()
            .ok_or("failed lock returned a reader")?;

        assert_lock_error(&error, kind);
        assert_eq!(error.outcome(), None);
        assert_eq!(LOCKS.load(Ordering::SeqCst), 1);
        assert_eq!(OPENS.load(Ordering::SeqCst), 0);

        let error = connection
            .put_object(put("selected"), ObservedSource)
            .await
            .unwrap_err();

        assert_lock_error(&error, kind);
        assert_eq!(error.outcome(), Some(MutationOutcome::NotCommitted));
        assert_eq!(LOCKS.load(Ordering::SeqCst), 2);
        assert_eq!(OPENS.load(Ordering::SeqCst), 0);
        assert_eq!(POLLS.load(Ordering::SeqCst), 0);
        ENABLED.store(false, Ordering::SeqCst);

        let (_, mut body) = connection.get_object(read("selected")).await?;
        assert_eq!(body.read_all(64).await?, b"unchanged bytes");
        body.close().await?;
        assert_eq!(OPENS.load(Ordering::SeqCst), 1);
        connection.close().await?;
        Ok::<_, Box<dyn std::error::Error>>(())
    })?;

    let mut output = io::stdout().lock();
    writeln!(output, "PROBE VERIFIED")?;
    output.flush()?;
    drop(output);
    let mut command = String::new();
    io::stdin().lock().read_line(&mut command)?;
    assert_eq!(command, "exit\n");
    Ok(())
}
