use std::io::BufRead as _;

use super::*;
use crate::integrity::{ChecksumAlgorithm, ChecksumType};
use crate::multipart::{CreateRequest, MultipartChecksum};
use crate::object::{BucketName, ObjectKey, Representation, UserMetadata};
use crate::test_process as process;
use crate::{EpochMillis, MutationOutcome, OperationFailure, UploadId};

mod completion;
mod copy;
mod parts;
mod uploads;

const CHILD: &str = "connection::tests::multipart::initiation_process_child";

fn request() -> CreateRequest {
    CreateRequest {
        bucket: BucketName::parse("attachments").unwrap(),
        key: ObjectKey::parse("durable/initiation").unwrap(),
        expected_owner: None,
        representation: Representation::default(),
        metadata: UserMetadata::default(),
        tags: None,
        checksum: MultipartChecksum::new(ChecksumAlgorithm::Crc64Nvme, ChecksumType::FullObject)
            .unwrap(),
    }
}

fn retained_ids(path: &std::path::Path) -> Result<Vec<UploadId>, Box<dyn std::error::Error>> {
    // Call only after every native connection and child has settled.
    let database = rusqlite::Connection::open(path.join("metadata.sqlite3"))?;

    let mut statement = database
        .prepare("SELECT upload_id, initiated_at_ms FROM multipart_uploads ORDER BY upload_id")?;

    let rows = statement.query_map([], |row| {
        Ok((row.get::<_, String>(0)?, row.get::<_, i64>(1)?))
    })?;

    let mut ids = Vec::new();

    for row in rows {
        let (value, time) = row?;
        ids.push(UploadId::parse(&value, EpochMillis::new(time)?)?);
    }

    drop(statement);
    database.close().map_err(|(_, error)| error)?;
    Ok(ids)
}

#[test]
fn canceled_and_close_fenced_initiation_create_no_upload_but_acknowledged_state_survives_close()
-> TestResult {
    let directory = TestDirectory::new()?.retain_on_failure();
    let path = directory.path().join("store");

    let acknowledged = runtime()?.block_on(async {
        let peer = Connection::open(path.clone(), None).await?;
        peer.create_bucket(request().bucket).await?;
        peer.pause_maintenance();

        for close_connection in [false, true] {
            let connection = Connection::open(path.clone(), None).await?;
            connection.pause_maintenance();
            let (entered, observed) = oneshot::channel();
            let (release, released) = std::sync::mpsc::sync_channel(1);
            let worker = connection.clone();

            let blocker = tokio::spawn(async move {
                worker
                    .probe(move |_| {
                        entered
                            .send(())
                            .map_err(|()| io::Error::other("missing waiter"))?;

                        released
                            .recv_timeout(Duration::from_secs(10))
                            .map_err(io::Error::other)
                    })
                    .await
            });

            observed.await?;
            let capacity = connection.owner.state.context.sender.capacity();
            let mut initiation = Box::pin(connection.create_multipart_upload(request()));

            std::future::poll_fn(|context| {
                assert!(initiation.as_mut().poll(context).is_pending());
                Poll::Ready(())
            })
            .await;

            assert_eq!(
                connection.owner.state.context.sender.capacity(),
                capacity - 1
            );

            if close_connection {
                let mut closing = Box::pin(connection.close());

                std::future::poll_fn(|context| {
                    assert!(closing.as_mut().poll(context).is_pending());
                    Poll::Ready(())
                })
                .await;

                release.send(())?;
                blocker.await??;
                let error = initiation.await.unwrap_err();
                assert_eq!(error.failure(), OperationFailure::StoreClosing);
                assert_eq!(error.outcome(), Some(MutationOutcome::NotCommitted));
                closing.await?;
            } else {
                drop(initiation);
                release.send(())?;
                blocker.await??;
                connection.get_owner_info().await?;
                connection.close().await?;
            }
        }

        let created = peer.create_multipart_upload(request()).await?;
        peer.close().await?;
        Ok::<_, Box<dyn std::error::Error>>(created.upload_id)
    })?;

    assert_eq!(retained_ids(&path)?, vec![acknowledged.clone()]);

    runtime()?.block_on(async {
        let reopened = Connection::open(path.clone(), None).await?;
        reopened.get_owner_info().await?;
        reopened.close().await?;
        Ok::<_, Box<dyn std::error::Error>>(())
    })?;

    assert_eq!(retained_ids(&path)?, vec![acknowledged]);
    directory.finish()?;
    Ok(())
}

#[test]
fn process_death_at_initiation_commit_and_after_acknowledgment_reopens_complete_state() -> TestResult
{
    let _serial = process::process_scenario()?;

    for (mode, committed) in [
        ("multipart.initiate.commit.before", false),
        ("multipart.initiate.commit.after", true),
        ("acknowledged", true),
    ] {
        let directory = TestDirectory::new()?.retain_on_failure();
        let path = directory.path().join("store");

        runtime()?.block_on(async {
            let connection = Connection::open(path.clone(), None).await?;
            connection.create_bucket(request().bucket).await?;
            connection.close().await?;
            Ok::<_, Box<dyn std::error::Error>>(())
        })?;

        let mut child = process::Probe::start(&path, mode, CHILD)?;
        assert_eq!(child.observation()?, "PAUSED");
        child.kill_and_wait()?;
        let original = retained_ids(&path)?;
        assert_eq!(original.len(), usize::from(committed), "{mode}");

        let created = runtime()?.block_on(async {
            let reopened = Connection::open(path.clone(), None).await?;
            let upload = reopened.create_multipart_upload(request()).await?;
            reopened.close().await?;
            Ok::<_, Box<dyn std::error::Error>>(upload)
        })?;

        let retained = retained_ids(&path)?;
        assert_eq!(retained.len(), original.len() + 1);
        assert!(retained.contains(&created.upload_id));

        for upload_id in original {
            assert!(retained.contains(&upload_id));
        }

        directory.finish()?;
    }

    Ok(())
}

fn pause() -> io::Result<()> {
    let mut output = io::stdout().lock();
    writeln!(output, "PROBE PAUSED")?;
    output.flush()?;
    drop(output);
    let mut line = String::new();
    io::stdin().lock().read_line(&mut line)?;

    Err(io::Error::other(
        "owned process must be killed at its barrier",
    ))
}

fn checkpoint(point: &str) -> io::Result<()> {
    if point == std::env::var("mikrio_probe_mode").map_err(io::Error::other)? {
        return pause();
    }

    Ok(())
}

#[test]
#[ignore = "Owned subprocess entry for durable multipart initiation"]
fn initiation_process_child() -> TestResult {
    let path =
        PathBuf::from(std::env::var_os("mikrio_probe_directory").ok_or("missing owned directory")?);

    let mode = std::env::var("mikrio_probe_mode")?;

    runtime()?.block_on(async {
        let connection = Connection::open(path, None).await?;
        connection.pause_maintenance();
        crate::protocol_fault::install(checkpoint)?;
        connection.create_multipart_upload(request()).await?;

        if mode == "acknowledged" {
            pause()?;
        }

        Err::<(), Box<dyn std::error::Error>>("child missed its owned checkpoint".into())
    })
}
