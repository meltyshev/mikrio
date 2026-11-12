//! Independent-process persistent flows and real native read ownership barriers.

use std::pin::Pin;
use std::sync::atomic::AtomicBool;
use std::task::Context as TaskContext;

use tokio::io::{AsyncRead, ReadBuf};

use super::objects::{put, read};
use super::*;
use crate::object::BucketName;
use crate::test_process as process;
use crate::{BlobId, OperationFailure};

mod control;

const CHILD: &str = "connection::tests::processes::object_process_child";
static READ_BLOCKED: AtomicBool = AtomicBool::new(false);

#[test]
fn canceled_persistent_read_and_close_retain_protection_until_native_io_settles() -> TestResult {
    canceled_read(false)
}

#[test]
fn canceled_part_read_and_close_retain_protection_until_native_io_settles() -> TestResult {
    canceled_read(true)
}

fn canceled_read(part: bool) -> TestResult {
    let _serial = process::process_scenario()?;

    for point in ["read.native.before", "read.native.after"] {
        let directory = TestDirectory::new()?.retain_on_failure();

        runtime()?.block_on(async {
            let path = directory.path().join("store");
            let connection = Connection::open(path.clone(), None).await?;
            connection.pause_maintenance();

            connection
                .create_bucket(BucketName::parse("attachments")?)
                .await?;

            if part {
                super::read_authorization::seed_parts(
                    &connection,
                    put("selected"),
                    b"original process bytes",
                )
                .await?;
            } else {
                connection
                    .put_object(put("selected"), b"original process bytes".as_slice())
                    .await?;
            }

            let mode = if part {
                format!("part.{point}")
            } else {
                point.to_owned()
            };

            let mut child = process::Probe::start(&path, &mode, CHILD)?;
            let selected = BlobId::parse(&child.observation_async().await?)?;
            assert_eq!(child.observation_async().await?, "NATIVE_READ_BLOCKED");
            assert_eq!(child.observation_async().await?, "CLOSE_WAITING");

            connection.delete_object(read("selected"), None).await?;

            super::super::maintenance::batch(
                Arc::new(connection.admit()?),
                path.clone(),
                selected.shard(),
            )
            .await?;

            let selected_payload_path = path
                .join("objects")
                .join(format!("{:02x}", selected.shard()))
                .join(selected.to_string());

            let retained = std::fs::read(&selected_payload_path)?;
            let offset = if part { 5 * 1024 * 1024 } else { 0 };
            assert_eq!(&retained[offset..], b"original process bytes");

            assert!(
                GuardManager::open(&path)?
                    .try_acquire(GuardTarget::Shard(selected.shard()), GuardMode::Exclusive)?
                    .is_none(),
                "{point}: canceled native read released its shard"
            );

            connection
                .put_object(put("peer-write"), b"peer remains usable".as_slice())
                .await?;

            let (_, mut peer_body) = connection.get_object(read("peer-write")).await?;
            assert_eq!(peer_body.read_all(100).await?, b"peer remains usable");
            child.finish_async().await?;

            super::super::maintenance::batch(
                Arc::new(connection.admit()?),
                path.clone(),
                selected.shard(),
            )
            .await?;

            assert!(
                !selected_payload_path.exists(),
                "{point}: settled read prevented cleanup"
            );

            connection.close().await?;
            let reopened = Connection::open(path, None).await?;

            assert_eq!(
                reopened
                    .head_object(read("selected"))
                    .await
                    .unwrap_err()
                    .failure(),
                OperationFailure::NoSuchKey
            );

            let (_, mut peer_body) = reopened.get_object(read("peer-write")).await?;
            assert_eq!(peer_body.read_all(100).await?, b"peer remains usable");
            reopened.close().await?;
            Ok::<_, Box<dyn std::error::Error>>(())
        })?;

        directory.finish()?;
    }

    Ok(())
}

fn pause_native_read(point: &str) -> io::Result<()> {
    let mode = std::env::var("mikrio_probe_mode").map_err(io::Error::other)?;

    if point != mode.strip_prefix("part.").unwrap_or(&mode) {
        return Ok(());
    }

    announce("NATIVE_READ_BLOCKED")?;
    READ_BLOCKED.store(true, Ordering::SeqCst);
    process::read_exit()
}

async fn cancel_native_read(connection: &Connection, part: bool) -> TestResult {
    connection.pause_maintenance();
    let mut request = read("selected");
    request.part_number = part.then(|| crate::multipart::PartNumber::new(2).unwrap());
    let (selected, mut body) = connection.get_object(request).await?;

    assert_eq!(
        selected.content_offset(),
        if part { 5 * 1024 * 1024 } else { 0 }
    );

    assert_eq!(selected.content_length(), 22);
    announce(&selected.blob_id.to_string())?;
    crate::protocol_fault::install(pause_native_read)?;
    let reading = tokio::spawn(async move { body.read_chunk(8).await });

    tokio::time::timeout(Duration::from_secs(10), async {
        while !READ_BLOCKED.load(Ordering::SeqCst) {
            tokio::task::yield_now().await;
        }
    })
    .await?;

    reading.abort();
    assert!(reading.await.unwrap_err().is_cancelled());
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

    // Dropping the close waiter also cannot release the native read's owners.
    drop(closing);
    let mut closing = Box::pin(connection.close());

    std::future::poll_fn(|context| {
        assert!(closing.as_mut().poll(context).is_pending());
        Poll::Ready(())
    })
    .await;

    announce("CLOSE_WAITING")?;
    tokio::time::timeout(Duration::from_secs(10), closing).await??;

    assert!(
        GuardManager::open(&connection.info().directory)?
            .try_acquire(
                GuardTarget::Shard(selected.blob_id.shard()),
                GuardMode::Exclusive
            )?
            .is_some()
    );

    Ok(())
}

#[test]
fn waiting_for_child_observation_keeps_parent_runtime_owners_progressing() -> TestResult {
    let _serial = process::process_scenario()?;
    let directory = TestDirectory::new()?.retain_on_failure();

    runtime()?.block_on(async {
        let path = directory.path().join("store");
        let connection = Connection::open(path.clone(), None).await?;
        let mut child = process::Probe::start(&path, "runtime-progress", CHILD)?;
        assert_eq!(child.observation_async().await?, "READY");
        let parent = connection.clone();

        let work = tokio::spawn(async move {
            parent
                .probe(move |_| {
                    std::fs::write(path.join("parent-progress"), b"runtime drove owner")
                })
                .await
        });

        // A blocking receive here prevents the newly spawned owner from even
        // submitting its command, so the child cannot produce this observation.
        assert_eq!(child.observation_async().await?, "PROGRESSED");
        work.await??;
        child.finish_async().await?;
        connection.close().await?;
        Ok::<_, Box<dyn std::error::Error>>(())
    })?;

    directory.finish()?;
    Ok(())
}

#[test]
fn independent_process_reader_survives_replacement_delete_and_other_connection_close() -> TestResult
{
    let _serial = process::process_scenario()?;
    let directory = TestDirectory::new()?;

    runtime()?.block_on(async {
        let path = directory.path().join("store");
        let connection = Connection::open(path.clone(), None).await?;

        connection
            .create_bucket(BucketName::parse("attachments")?)
            .await?;

        connection
            .put_object(put("selected"), b"original process bytes".as_slice())
            .await?;

        let mut child = process::Probe::start(&path, "reader", CHILD)?;
        let selected = BlobId::parse(&child.observation_async().await?)?;

        let selected_payload_path = path
            .join("objects")
            .join(format!("{:02x}", selected.shard()))
            .join(selected.to_string());

        connection
            .put_object(put("selected"), b"replacement".as_slice())
            .await?;

        connection.delete_object(read("selected"), None).await?;
        assert!(selected_payload_path.exists());

        assert!(
            GuardManager::open(&path)?
                .try_acquire(GuardTarget::Shard(selected.shard()), GuardMode::Exclusive)?
                .is_none()
        );

        connection.close().await?;
        child.finish_async().await?;

        let reopened = Connection::open(path, None).await?;

        assert_eq!(
            reopened
                .head_object(read("selected"))
                .await
                .unwrap_err()
                .failure(),
            OperationFailure::NoSuchKey
        );

        let (_, mut body) = reopened.get_object(read("peer-write")).await?;
        assert_eq!(body.read_all(100).await?, b"peer remains usable");

        tokio::time::timeout(Duration::from_secs(20), async {
            while selected_payload_path.exists() {
                tokio::time::sleep(Duration::from_millis(20)).await;
            }
        })
        .await?;

        reopened.close().await?;
        Ok::<_, Box<dyn std::error::Error>>(())
    })?;

    directory.finish()?;
    Ok(())
}

#[test]
fn killed_process_upload_reclaims_only_its_reservation_and_preserves_predecessor() -> TestResult {
    let _serial = process::process_scenario()?;
    let directory = TestDirectory::new()?;

    runtime()?.block_on(async {
        let path = directory.path().join("store");
        let connection = Connection::open(path.clone(), None).await?;

        connection
            .create_bucket(BucketName::parse("attachments")?)
            .await?;

        connection
            .put_object(put("selected"), b"original process bytes".as_slice())
            .await?;

        let mut child = process::Probe::start(&path, "writer", CHILD)?;
        assert_eq!(child.observation_async().await?, "SOURCE_BLOCKED");
        let sql = rusqlite::Connection::open(path.join("metadata.sqlite3"))?;

        let id: Vec<u8> = sql.query_row(
            "SELECT blob_id FROM blobs WHERE state = 'reserved'",
            [],
            |row| row.get(0),
        )?;

        sql.close().map_err(|(_, error)| error)?;

        let reserved =
            BlobId::from_bytes(id.try_into().map_err(|_| "invalid fixture reservation")?);

        let staging = path
            .join("staging")
            .join(format!("{:02x}", reserved.shard()))
            .join(reserved.to_string());

        assert!(staging.exists());

        assert!(
            GuardManager::open(&path)?
                .try_acquire(GuardTarget::Shard(reserved.shard()), GuardMode::Exclusive)?
                .is_none()
        );

        let (_, mut original) = connection.get_object(read("selected")).await?;
        assert_eq!(original.read_all(100).await?, b"original process bytes");
        child.kill_and_wait_async().await?;

        tokio::time::timeout(Duration::from_secs(20), async {
            while staging.exists() {
                tokio::time::sleep(Duration::from_millis(20)).await;
            }
        })
        .await?;

        let (_, mut original) = connection.get_object(read("selected")).await?;
        assert_eq!(original.read_all(100).await?, b"original process bytes");

        connection
            .put_object(put("after-crash"), b"new operation".as_slice())
            .await?;

        connection.close().await?;
        let reopened = Connection::open(path, None).await?;
        let (_, mut body) = reopened.get_object(read("after-crash")).await?;
        assert_eq!(body.read_all(100).await?, b"new operation");
        reopened.close().await?;
        Ok::<_, Box<dyn std::error::Error>>(())
    })?;

    directory.finish()?;
    Ok(())
}

fn announce(value: &str) -> io::Result<()> {
    let mut output = io::stdout().lock();
    writeln!(output, "PROBE {value}")?;
    output.flush()
}

struct PausedSource;

impl AsyncRead for PausedSource {
    fn poll_read(
        self: Pin<&mut Self>,
        _: &mut TaskContext<'_>,
        _: &mut ReadBuf<'_>,
    ) -> Poll<io::Result<()>> {
        // The owned child is deliberately killed with this callback blocked.
        // Reservation and staging file creation have completed before the first pull.
        Poll::Ready(announce("SOURCE_BLOCKED").and_then(|()| process::read_exit()))
    }
}

#[test]
#[ignore = "Owned subprocess entry for persistent object scenarios"]
fn object_process_child() -> TestResult {
    let path =
        PathBuf::from(std::env::var_os("mikrio_probe_directory").ok_or("missing owned directory")?);

    let mode = std::env::var("mikrio_probe_mode")?;

    runtime()?.block_on(async {
        let connection = Connection::open(path.clone(), None).await?;

        match mode.as_str() {
            "read.native.before"
            | "read.native.after"
            | "part.read.native.before"
            | "part.read.native.after" => {
                cancel_native_read(&connection, mode.starts_with("part.")).await?;
            }
            "runtime-progress" => {
                announce("READY")?;

                tokio::time::timeout(Duration::from_secs(10), async {
                    while !path.join("parent-progress").exists() {
                        tokio::time::sleep(Duration::from_millis(10)).await;
                    }
                })
                .await?;

                assert_eq!(
                    std::fs::read(path.join("parent-progress"))?,
                    b"runtime drove owner"
                );

                announce("PROGRESSED")?;
                process::wait_for_exit().await?;
            }
            "reader" => {
                let (selected, mut body) = connection.get_object(read("selected")).await?;
                assert_eq!(body.read_chunk(8).await?, Some(b"original".to_vec()));
                announce(&selected.blob_id.to_string())?;
                process::wait_for_exit().await?;
                assert_eq!(body.read_all(100).await?, b" process bytes");

                assert_eq!(
                    connection
                        .head_object(read("selected"))
                        .await
                        .unwrap_err()
                        .failure(),
                    OperationFailure::NoSuchKey
                );

                connection
                    .put_object(put("peer-write"), b"peer remains usable".as_slice())
                    .await?;
            }
            "writer" => {
                connection.put_object(put("selected"), PausedSource).await?;
            }
            _ => return Err("unknown object process mode".into()),
        }

        connection.close().await?;
        Ok::<_, Box<dyn std::error::Error>>(())
    })
}
