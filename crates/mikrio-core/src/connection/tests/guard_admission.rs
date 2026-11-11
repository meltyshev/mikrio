//! An exhausted shard-admission budget cannot consume or publish a PUT.

use super::descriptors;
use super::objects::{put, read};
use super::*;
use crate::object::BucketName;
use crate::{MutationOutcome, OperationFailure};
use std::io::BufRead as _;
use std::pin::Pin;
use std::sync::atomic::AtomicUsize;
use std::task::Context as TaskContext;
use tokio::io::{AsyncRead, ReadBuf};

const CHILD: &str = "connection::tests::guard_admission::shard_admission_child";

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

#[test]
fn exhausted_shard_admission_preserves_predecessor_and_never_consumes_source() -> TestResult {
    let _serial = crate::test_process::process_scenario()?;
    let directory = TestDirectory::new()?.retain_on_failure();
    #[cfg(unix)]
    let parent_limits = descriptors::current()?;

    // Initialize the empty durable layout before the child's observation window.
    runtime()?.block_on(async {
        let connection = Connection::open(directory.path().join("store"), None).await?;
        connection.close().await
    })?;

    let mut child = crate::test_process::Probe::start(directory.path(), "admission", CHILD)?;
    let observation: serde_json::Value = serde_json::from_str(&child.observation()?)?;
    assert_eq!(observation["status"], "verified");

    #[cfg(unix)]
    {
        assert_eq!(
            observation["limits"]["soft_before"].as_u64(),
            Some(parent_limits.0)
        );

        assert_eq!(
            observation["limits"]["hard"].as_u64(),
            Some(parent_limits.1)
        );

        assert!(
            observation["limits"]["soft_after"]
                .as_u64()
                .ok_or("missing descriptor budget")?
                >= 512
        );
    }

    child.finish()?;
    #[cfg(unix)]
    assert_eq!(descriptors::current()?, parent_limits);
    directory.finish()?;
    Ok(())
}

#[test]
#[ignore = "owned all-shard admission subprocess; invoked by its parent scenario"]
fn shard_admission_child() -> TestResult {
    assert_eq!(std::env::var("mikrio_probe_mode")?, "admission");

    let path =
        PathBuf::from(std::env::var_os("mikrio_probe_directory").ok_or("missing owned directory")?)
            .join("store");

    let limits = descriptors::prepare()?;

    runtime()?.block_on(async {
        let connection = Connection::open(path.clone(), None).await?;
        connection.pause_maintenance();

        connection
            .create_bucket(BucketName::parse("attachments")?)
            .await?;

        connection
            .put_object(put("selected"), b"acknowledged predecessor".as_slice())
            .await?;

        let predecessor = connection.head_object(read("selected")).await?;
        let peer = Connection::open(path.clone(), None).await?;
        peer.pause_maintenance();

        // Hold the complete fixed shard set in order. Whichever ID the real
        // entropy source selects, its output cannot be admitted before release.
        let manager = GuardManager::open(&path)?;
        let mut held = Vec::new();

        for shard in 0..=u8::MAX {
            held.push(
                manager
                    .try_acquire(GuardTarget::Shard(shard), GuardMode::Exclusive)?
                    .ok_or("fresh paused store unexpectedly has a shard holder")?,
            );
        }

        let reads = Arc::new(AtomicUsize::new(0));
        let started = tokio::time::Instant::now();

        let error = tokio::time::timeout(
            Duration::from_secs(10),
            connection.put_object(put("selected"), ObservedSource(Arc::clone(&reads))),
        )
        .await?
        .unwrap_err();

        assert_eq!(error.failure(), OperationFailure::StoreBusy);
        assert_eq!(error.outcome(), Some(MutationOutcome::NotCommitted));
        assert!(started.elapsed() >= ADMISSION_TIMEOUT);
        assert_eq!(reads.load(Ordering::SeqCst), 0);
        assert_eq!(held.len(), 256);

        // The independent peer can still make metadata progress while the
        // shard guards remain held. No reservation was created by this PUT.
        let selected = peer.head_object(read("selected")).await?;
        assert_eq!(selected.blob_id, predecessor.blob_id);
        assert_eq!(selected.etag, predecessor.etag);

        let sql = rusqlite::Connection::open_with_flags(
            path.join("metadata.sqlite3"),
            rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY,
        )?;

        let blobs: i64 = sql.query_row("SELECT COUNT(*) FROM blobs", [], |row| row.get(0))?;
        assert_eq!(blobs, 1);
        drop(sql);
        drop(held);

        let (_, mut body) = peer.get_object(read("selected")).await?;
        assert_eq!(body.read_all(64).await?, b"acknowledged predecessor");
        connection.close().await?;

        peer.put_object(put("selected"), b"independent peer update".as_slice())
            .await?;

        peer.close().await?;

        let reopened = Connection::open(path, None).await?;
        let (_, mut body) = reopened.get_object(read("selected")).await?;
        assert_eq!(body.read_all(64).await?, b"independent peer update");
        reopened.close().await?;
        Ok::<_, Box<dyn std::error::Error>>(())
    })?;

    let observation = serde_json::json!({"status": "verified", "limits": limits});
    let mut output = std::io::stdout().lock();
    writeln!(output, "PROBE {observation}")?;
    output.flush()?;
    drop(output);
    let mut command = String::new();
    std::io::stdin().lock().read_line(&mut command)?;
    assert_eq!(command, "exit\n");
    Ok(())
}
