//! Process interruption at real collector effects; not a storage power-cut lane.

use std::fmt::Write as _;
use std::path::Path;
use std::sync::atomic::AtomicBool;

use md5::Digest as _;
use rusqlite::OptionalExtension as _;

use super::objects::{put, read};
use super::*;
use crate::object::BucketName;
use crate::test_process as process;
use crate::{BlobId, OperationFailure};

mod effects;

const CHILD: &str = "connection::tests::gc_faults::collector_fault_child";
static OCCURRENCES: AtomicUsize = AtomicUsize::new(0);
static FIRED: AtomicBool = AtomicBool::new(false);

#[derive(Clone, Copy)]
enum Layout {
    Single,
    Completed,
}

fn checkpoint(point: &str) -> io::Result<()> {
    process::trace(point);
    let mode = std::env::var("mikrio_probe_mode").map_err(io::Error::other)?;
    let mut fields = mode.split(':');

    let kind = fields
        .next()
        .ok_or_else(|| io::Error::other("missing mode"))?;

    let target = fields
        .next()
        .ok_or_else(|| io::Error::other("missing point"))?;

    let occurrence: usize = fields
        .next()
        .ok_or_else(|| io::Error::other("missing occurrence"))?
        .parse()
        .map_err(io::Error::other)?;

    if target != point || OCCURRENCES.fetch_add(1, Ordering::SeqCst) + 1 != occurrence {
        return Ok(());
    }

    if kind == "error" {
        FIRED.store(true, Ordering::SeqCst);
        return Err(io::Error::other("injected collector effect failure"));
    }

    announce("CHECKPOINT")?;
    FIRED.store(true, Ordering::SeqCst);
    process::read_exit()
}

fn announce(value: &str) -> io::Result<()> {
    let mut output = io::stdout().lock();
    writeln!(output, "PROBE {value}")?;
    output.flush()
}

fn blob_path(path: &Path, tree: &str, blob_id: BlobId) -> PathBuf {
    path.join(tree)
        .join(format!("{:02x}", blob_id.shard()))
        .join(blob_id.to_string())
}

fn ledger(path: &Path, blob_id: BlobId) -> rusqlite::Result<Option<String>> {
    let sql = rusqlite::Connection::open(path.join("metadata.sqlite3"))?;

    sql.query_row(
        "SELECT state FROM blobs WHERE blob_id = ?1",
        [blob_id.as_bytes().as_slice()],
        |row| row.get(0),
    )
    .optional()
}

fn verify_live_and_sentinels(path: &Path, orphan: BlobId) -> TestResult {
    let sql = rusqlite::Connection::open(path.join("metadata.sqlite3"))?;

    let live: Vec<u8> = sql.query_row(
        "SELECT o.blob_id FROM objects AS o JOIN blobs AS b ON b.blob_id = o.blob_id
         WHERE o.key = x'6c697665' AND b.state = 'ready'",
        [],
        |row| row.get(0),
    )?;

    let live = BlobId::from_bytes(live.try_into().map_err(|_| "invalid live identity")?);

    assert_eq!(
        std::fs::read(blob_path(path, "objects", live))?,
        b"live bytes"
    );

    for tree in ["staging", "objects"] {
        let sentinel = blob_path(path, tree, orphan).with_file_name("operator-sentinel");
        assert_eq!(std::fs::read(sentinel)?, b"unowned bytes");
    }

    Ok(())
}

async fn reclaim(path: &Path, orphan: BlobId) -> TestResult {
    let connection = Connection::open(path.to_path_buf(), None).await?;
    connection.pause_maintenance();

    tokio::time::timeout(Duration::from_secs(10), async {
        while ledger(path, orphan)?.is_some() {
            super::super::maintenance::batch(
                Arc::new(connection.admit()?),
                path.to_path_buf(),
                orphan.shard(),
            )
            .await?;

            tokio::time::sleep(Duration::from_millis(20)).await;
        }

        Ok::<_, Box<dyn std::error::Error>>(())
    })
    .await??;

    for tree in ["staging", "objects"] {
        assert!(!blob_path(path, tree, orphan).exists());
    }

    let (_, mut body) = connection.get_object(read("live")).await?;
    assert_eq!(body.read_all(100).await?, b"live bytes");
    verify_live_and_sentinels(path, orphan)?;
    connection.close().await?;
    Ok(())
}

fn interruptions(kind: &str, points: &[(&str, usize)], layout: Layout) -> TestResult {
    let _serial = process::process_scenario()?;
    let directory = TestDirectory::new()?.retain_on_failure();

    runtime()?.block_on(async {
        for (index, (point, occurrence)) in points.iter().enumerate() {
            let path = directory.path().join(format!("store-{index}"));

            // Durable empty-layout preparation precedes the child watchdog.
            // The child still owns fixture objects and all collector effects.
            let prepared = Connection::open(path.clone(), None).await?;
            prepared.close().await?;

            let layout_name = match layout {
                Layout::Single => "single",
                Layout::Completed => "parts",
            };

            let mode = format!("{kind}:{point}:{occurrence}:{layout_name}");
            std::fs::write(directory.path().join("active-case.txt"), &mode)?;
            let mut child = process::Probe::start(&path, &mode, CHILD)?;

            let orphan = BlobId::parse(&child.observation_async().await.map_err(|error| {
                io::Error::other(format!("{mode}: awaiting prepared fixture: {error}"))
            })?)?;

            assert_eq!(
                child.observation_async().await.map_err(|error| {
                    io::Error::other(format!("{mode}: awaiting collector effect: {error}"))
                })?,
                if kind == "kill" {
                    "CHECKPOINT"
                } else {
                    "SETTLED"
                },
                "{mode}"
            );

            let guard = GuardManager::open(&path)?
                .try_acquire(GuardTarget::Shard(orphan.shard()), GuardMode::Exclusive)?;

            assert_eq!(guard.is_none(), kind == "kill", "{mode}");
            drop(guard);
            verify_live_and_sentinels(&path, orphan)?;

            if matches!(layout, Layout::Completed) {
                let sql = rusqlite::Connection::open(path.join("metadata.sqlite3"))?;

                let remaining: i64 =
                    sql.query_row("SELECT count(*) FROM blob_parts", [], |row| row.get(0))?;

                assert_eq!(
                    remaining,
                    if *point == "gc.finish.commit.after" {
                        1
                    } else {
                        257
                    }
                );
                assert_eq!(ledger(&path, orphan)?.as_deref(), Some("deleting"));
            }

            // A lost deletion-marking acknowledgment cannot authorize unlinking.
            // The next pass must establish authority in its own transaction.
            if point.starts_with("gc.mark.") || *point == "gc.selected" {
                assert!(blob_path(&path, "staging", orphan).exists(), "{mode}");
                assert!(blob_path(&path, "objects", orphan).exists(), "{mode}");

                assert_eq!(
                    ledger(&path, orphan)?.as_deref(),
                    Some(if *point == "gc.mark.commit.after" {
                        "deleting"
                    } else {
                        "ready"
                    }),
                    "{mode}"
                );
            }

            if kind == "kill" {
                child.kill_and_wait_async().await?;
            } else {
                child.finish_async().await?;
            }

            // Repeated normal opens and actual production batches converge;
            // no test rewrites retry state or deletes a ledger/file itself.
            reclaim(&path, orphan).await?;
            reclaim(&path, orphan).await?;
        }

        Ok::<_, Box<dyn std::error::Error>>(())
    })?;

    directory.finish()?;
    Ok(())
}

fn completed_layout(path: &Path, orphan: BlobId) -> TestResult {
    // A completed output awaiting ledger cleanup may have no files. Seed its
    // immutable layout without allocating a fake 1.25 GiB body.
    for tree in ["objects", "staging"] {
        crate::DurableDirectory::open(&path.join(tree).join(format!("{:02x}", orphan.shard())))?
            .remove_owned(&orphan.to_string())?;
    }

    let digest = md5::Md5::digest([0u8; 257 * 16]);
    let mut etag = String::from("\"");

    for byte in &digest {
        write!(etag, "{byte:02x}")?;
    }

    etag.push_str("-257\"");

    let mut sql = rusqlite::Connection::open(path.join("metadata.sqlite3"))?;
    let transaction = sql.transaction()?;

    transaction.execute(
        "UPDATE blobs SET state='deleting', write_kind='complete_multipart_upload',
        byte_length=?2, part_count=257, etag=?3, md5_digest=?4, checksum_type=NULL, checksums_json='{}'
        WHERE blob_id=?1",
        rusqlite::params![
            orphan.as_bytes().as_slice(),
            256_i64 * 5 * 1024 * 1024 + 1,
            etag,
            digest.as_slice()
        ],
    )?;

    for number in 1..=257 {
        transaction.execute(
            "INSERT INTO blob_parts (blob_id, part_number, byte_offset, byte_length, checksums_json, part_index)
            VALUES (?1, ?2, ?3, ?4, '{}', ?2)",
            rusqlite::params![
                orphan.as_bytes().as_slice(),
                number,
                (number - 1) * 5 * 1024 * 1024,
                if number == 257 { 1 } else { 5 * 1024 * 1024 }
            ],
        )?;
    }

    transaction.commit()?;
    Ok(())
}

#[test]
fn canceled_collector_and_close_retain_exclusion_until_native_unlink_settles() -> TestResult {
    let _serial = process::process_scenario()?;
    let directory = TestDirectory::new()?.retain_on_failure();
    let path = directory.path().join("store");

    OpenedStore::open(&path, None)?
        .close()
        .map_err(|(_, error)| error)?;

    let mut child = process::Probe::start(&path, "cancel:fs.Unlink.Before:1", CHILD)?;
    let orphan = BlobId::parse(&child.observation()?)?;
    assert_eq!(child.observation()?, "CHECKPOINT");
    assert_eq!(child.observation()?, "CLOSE_WAITING");
    let manager = GuardManager::open(&path)?;

    assert!(
        manager
            .try_acquire(GuardTarget::Shard(orphan.shard()), GuardMode::Exclusive)?
            .is_none()
    );

    assert!(
        manager
            .try_acquire(GuardTarget::Store, GuardMode::Exclusive)?
            .is_none()
    );

    assert!(blob_path(&path, "staging", orphan).exists());
    assert!(blob_path(&path, "objects", orphan).exists());

    child.finish()?;

    assert!(
        manager
            .try_acquire(GuardTarget::Shard(orphan.shard()), GuardMode::Exclusive)?
            .is_some()
    );

    assert!(
        manager
            .try_acquire(GuardTarget::Store, GuardMode::Exclusive)?
            .is_some()
    );

    runtime()?.block_on(reclaim(&path, orphan))?;
    directory.finish()?;
    Ok(())
}

async fn cancel_batch(connection: &Connection, path: PathBuf, orphan: BlobId) -> TestResult {
    let batch = tokio::spawn(super::super::maintenance::batch(
        Arc::new(connection.admit()?),
        path,
        orphan.shard(),
    ));

    tokio::time::timeout(Duration::from_secs(10), async {
        while !FIRED.load(Ordering::SeqCst) {
            tokio::task::yield_now().await;
        }
    })
    .await?;

    batch.abort();
    assert!(batch.await.unwrap_err().is_cancelled());
    let mut closing = Box::pin(connection.close());

    std::future::poll_fn(|context| {
        assert!(closing.as_mut().poll(context).is_pending());
        Poll::Ready(())
    })
    .await;

    assert!(matches!(
        ensure_open(&connection.owner.state),
        Err(ConnectionError::StoreClosing)
    ));

    announce("CLOSE_WAITING")?;
    tokio::time::timeout(Duration::from_secs(10), closing).await??;
    Ok(())
}

#[test]
#[ignore = "Owned subprocess entry for actual collector effects"]
fn collector_fault_child() -> TestResult {
    let path =
        PathBuf::from(std::env::var_os("mikrio_probe_directory").ok_or("missing owned directory")?);

    runtime()?.block_on(async {
        process::trace("collector fixture: opening");
        let connection = Connection::open(path.clone(), None).await?;
        process::trace("collector fixture: opened");
        connection.pause_maintenance();

        connection
            .create_bucket(BucketName::parse("attachments")?)
            .await?;

        process::trace("collector fixture: bucket created");

        connection
            .put_object(put("live"), b"live bytes".as_slice())
            .await?;

        process::trace("collector fixture: live object published");

        connection
            .put_object(put("orphan"), b"retired bytes".as_slice())
            .await?;

        process::trace("collector fixture: orphan candidate published");
        let orphan = connection.head_object(read("orphan")).await?.blob_id;
        connection.delete_object(read("orphan"), None).await?;
        process::trace("collector fixture: object retired");

        // A lost source-directory synchronization can leave both owned names
        // after publication/restart. Preserve that valid retry shape explicitly.
        std::fs::hard_link(
            blob_path(&path, "objects", orphan),
            blob_path(&path, "staging", orphan),
        )?;

        for tree in ["staging", "objects"] {
            std::fs::write(
                blob_path(&path, tree, orphan).with_file_name("operator-sentinel"),
                b"unowned bytes",
            )?;
        }

        if std::env::var("mikrio_probe_mode")?.ends_with(":parts") {
            completed_layout(&path, orphan)?;
        }

        announce(&orphan.to_string())?;
        process::trace("collector fixture: ready");
        crate::protocol_fault::install(checkpoint)?;

        if std::env::var("mikrio_probe_mode")?.starts_with("cancel:") {
            return cancel_batch(&connection, path, orphan).await;
        }

        let result =
            super::super::maintenance::batch(Arc::new(connection.admit()?), path, orphan.shard())
                .await;

        assert!(FIRED.load(Ordering::SeqCst));

        if std::env::var("mikrio_probe_mode")?.starts_with("error:") {
            let error = result.unwrap_err();
            assert_eq!(error.failure(), OperationFailure::IoError);
            let mode = std::env::var("mikrio_probe_mode")?;

            let expected = if mode.contains("commit.after") {
                Some(crate::MutationOutcome::Unknown)
            } else if mode.starts_with("error:g") {
                Some(crate::MutationOutcome::NotCommitted)
            } else {
                None
            };

            assert_eq!(error.outcome(), expected);

            announce("SETTLED")?;
            process::wait_for_exit().await?;
        } else {
            result?;
        }

        connection.close().await?;
        Ok::<_, Box<dyn std::error::Error>>(())
    })
}
