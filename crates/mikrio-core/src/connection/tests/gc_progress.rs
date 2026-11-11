//! Independent collectors and an admitted publisher share real native guards.

use super::objects::{put, read};
use super::*;
use crate::object::BucketName;
use crate::test_process as process;
use crate::{BlobId, OperationFailure};
use rusqlite::OptionalExtension as _;
use std::io::BufRead as _;
use std::path::Path;

#[cfg(unix)]
mod permissions;

const CHILD: &str = "connection::tests::gc_progress::collector_progress_child";
static MARKS: AtomicUsize = AtomicUsize::new(0);
static UNLINKS: AtomicUsize = AtomicUsize::new(0);
static PAUSED: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);

fn blob(shard: u8) -> BlobId {
    BlobId::from_bytes([shard; 16])
}

fn payload(path: &Path, tree: &str, shard: u8) -> PathBuf {
    path.join(tree)
        .join(format!("{shard:02x}"))
        .join(blob(shard).to_string())
}

fn state(path: &Path, shard: u8) -> rusqlite::Result<Option<String>> {
    let sql = rusqlite::Connection::open(path.join("metadata.sqlite3"))?;

    sql.query_row(
        "SELECT state FROM blobs WHERE blob_id = ?1",
        [blob(shard).as_bytes().as_slice()],
        |row| row.get(0),
    )
    .optional()
}

async fn publish(connection: &Connection, key: &str, shard: u8, bytes: &[u8]) -> TestResult {
    connection
        .owner
        .state
        .context
        .test_hooks
        .put_outputs
        .lock()
        .unwrap()
        .push_back(blob(shard));

    connection
        .put_object(put(key), io::Cursor::new(bytes.to_vec()))
        .await?;

    assert_eq!(
        connection.head_object(read(key)).await?.blob_id,
        blob(shard)
    );

    Ok(())
}

async fn pause_scheduler(connection: &Connection) -> TestResult {
    connection.pause_maintenance();

    tokio::time::timeout(Duration::from_secs(10), async {
        while connection.owner.state.activity.lock().unwrap().operations != 0 {
            tokio::task::yield_now().await;
        }
    })
    .await?;

    Ok(())
}

fn prepare(path: &Path) -> TestResult {
    runtime()?.block_on(async {
        let connection = Connection::open(path.to_path_buf(), None).await?;
        pause_scheduler(&connection).await?;

        connection
            .create_bucket(BucketName::parse("attachments")?)
            .await?;

        publish(&connection, "contended", 42, b"contended bytes").await?;
        publish(&connection, "progress", 44, b"unrelated bytes").await?;
        publish(&connection, "protected", 45, b"predecessor bytes").await?;
        connection.delete_object(read("contended"), None).await?;
        connection.delete_object(read("progress"), None).await?;
        connection.close().await?;
        Ok::<_, Box<dyn std::error::Error>>(())
    })
}

fn announce(value: &str) -> io::Result<()> {
    let mut output = io::stdout().lock();
    writeln!(output, "PROBE {value}")?;
    output.flush()
}

fn control(expected: &str) -> io::Result<()> {
    let mut line = String::new();
    io::stdin().lock().read_line(&mut line)?;

    if line != format!("{expected}\n") {
        return Err(io::Error::other("unexpected collector control command"));
    }

    Ok(())
}

fn checkpoint(point: &str) -> io::Result<()> {
    if point == "gc.mark.commit.before" {
        MARKS.fetch_add(1, Ordering::SeqCst);
    }

    if point == "fs.Unlink.Before" {
        UNLINKS.fetch_add(1, Ordering::SeqCst);
    }

    let mode = std::env::var("mikrio_probe_mode").map_err(io::Error::other)?;

    let target = mode
        .strip_prefix("owner:")
        .or_else(|| mode.strip_prefix("writer:"));

    // A child pauses once; later unlinks in the same batch keep running after
    // release. Counters observe effects in this process only.
    if target == Some(point) && !PAUSED.swap(true, Ordering::SeqCst) {
        announce("CHECKPOINT")?;
        control("exit")?;
    }

    Ok(())
}

fn competition(point: &str) -> TestResult {
    let _serial = process::process_scenario()?;
    let directory = TestDirectory::new()?.retain_on_failure();
    let path = directory.path().join("store");
    prepare(&path)?;
    std::fs::hard_link(payload(&path, "objects", 42), payload(&path, "staging", 42))?;
    let manager = GuardManager::open(&path)?;
    let mut preparation = Vec::new();

    // Prevent startup maintenance in any child from changing the fixture before
    // every child has acknowledged its own paused scheduler.
    for shard in [42, 44] {
        preparation.push(
            manager
                .try_acquire(GuardTarget::Shard(shard), GuardMode::Shared)?
                .ok_or("fixture shard unavailable")?,
        );
    }

    let mut writer = process::Probe::start(&path, "writer:payload.published", CHILD)?;
    assert_eq!(writer.observation()?, "READY");
    let mut owner = process::Probe::start(&path, &format!("owner:{point}"), CHILD)?;
    assert_eq!(owner.observation()?, "READY");
    let mut contender = process::Probe::start(&path, "contender", CHILD)?;
    assert_eq!(contender.observation()?, "READY");
    writer.command("run")?;
    assert_eq!(writer.observation()?, "CHECKPOINT");
    assert_eq!(state(&path, 43)?.as_deref(), Some("reserved"));

    assert_eq!(
        std::fs::read(payload(&path, "objects", 43))?,
        b"replacement bytes"
    );

    drop(preparation);
    owner.command("run")?;
    assert_eq!(owner.observation()?, "CHECKPOINT");
    assert_eq!(state(&path, 42)?.as_deref(), Some("deleting"));
    contender.command("run")?;
    assert_eq!(contender.observation()?, "SKIPPED");
    assert_eq!(contender.observation()?, "PROGRESSED");

    assert_eq!(state(&path, 42)?.as_deref(), Some("deleting"));
    assert_eq!(state(&path, 43)?.as_deref(), Some("reserved"));
    assert_eq!(state(&path, 44)?, None);

    for tree in ["staging", "objects"] {
        assert_eq!(std::fs::read(payload(&path, tree, 42))?, b"contended bytes");
    }

    assert_eq!(
        std::fs::read(payload(&path, "objects", 43))?,
        b"replacement bytes"
    );

    assert_eq!(
        std::fs::read(payload(&path, "objects", 45))?,
        b"predecessor bytes"
    );

    assert!(!payload(&path, "objects", 44).exists());

    for shard in [42, 43] {
        assert!(
            manager
                .try_acquire(GuardTarget::Shard(shard), GuardMode::Exclusive)?
                .is_none()
        );
    }

    contender.finish()?;
    owner.finish()?;
    assert_eq!(state(&path, 42)?, None);

    for tree in ["staging", "objects"] {
        assert!(!payload(&path, tree, 42).exists());
    }

    writer.finish()?;

    runtime()?.block_on(async {
        for _ in 0..2 {
            let reopened = Connection::open(path.clone(), None).await?;
            let (selected, mut body) = reopened.get_object(read("protected")).await?;
            assert_eq!(selected.blob_id, blob(43));
            assert_eq!(body.read_all(64).await?, b"replacement bytes");
            body.close().await?;

            for key in ["contended", "progress"] {
                assert_eq!(
                    reopened.head_object(read(key)).await.unwrap_err().failure(),
                    OperationFailure::NoSuchKey
                );
            }

            reopened.close().await?;
        }

        Ok::<_, Box<dyn std::error::Error>>(())
    })?;

    directory.finish()?;
    Ok(())
}

#[test]
fn competing_collector_skips_committed_deletion_and_guarded_publication() -> TestResult {
    competition("gc.mark.commit.after")
}

#[test]
fn competing_collector_skips_native_unlink_and_guarded_publication() -> TestResult {
    competition("fs.Unlink.Before")
}

#[test]
#[ignore = "Owned publisher and collector processes driven by exclusion barriers"]
fn collector_progress_child() -> TestResult {
    let path = PathBuf::from(std::env::var_os("mikrio_probe_directory").ok_or("missing store")?);
    let mode = std::env::var("mikrio_probe_mode")?;

    runtime()?.block_on(async {
        let connection = Connection::open(path.clone(), None).await?;
        pause_scheduler(&connection).await?;
        announce("READY")?;
        control("run")?;
        crate::protocol_fault::install(checkpoint)?;

        if mode.starts_with("writer:") {
            publish(&connection, "protected", 43, b"replacement bytes").await?;
        } else if mode.starts_with("owner:") {
            super::super::maintenance::batch(Arc::new(connection.admit()?), path.clone(), 42)
                .await?;
        } else if mode == "contender" {
            for shard in [42, 43] {
                super::super::maintenance::batch(
                    Arc::new(connection.admit()?),
                    path.clone(),
                    shard,
                )
                .await?;
            }

            assert_eq!(MARKS.load(Ordering::SeqCst), 0);
            assert_eq!(UNLINKS.load(Ordering::SeqCst), 0);
            announce("SKIPPED")?;

            super::super::maintenance::batch(Arc::new(connection.admit()?), path.clone(), 44)
                .await?;

            assert_eq!(MARKS.load(Ordering::SeqCst), 1);
            assert_eq!(UNLINKS.load(Ordering::SeqCst), 2);
            assert_eq!(state(&path, 44)?, None);
            announce("PROGRESSED")?;
            control("exit")?;
        } else {
            return Err("unknown collector mode".into());
        }

        connection.close().await?;
        Ok::<_, Box<dyn std::error::Error>>(())
    })
}
