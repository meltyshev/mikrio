//! Parent handshakes leave runtime and native capacity available for cleanup.

use std::sync::{Mutex, mpsc};

use tokio::sync::oneshot;

use super::*;

const CHILD: &str = "connection::tests::processes::control::cleanup_control_child";
const SHARD: u8 = 0x41;
static CHECKPOINT: Mutex<Option<Checkpoint>> = Mutex::new(None);

struct Checkpoint {
    entered: oneshot::Sender<()>,
    release: mpsc::Receiver<()>,
}

#[test]
fn parent_control_wait_allows_cleanup_and_native_reads_to_finish() -> TestResult {
    let _serial = process::process_scenario()?;
    let directory = TestDirectory::new()?.retain_on_failure();

    runtime()?.block_on(async {
        let path = directory.path().join("store");
        let connection = Connection::open(path.clone(), None).await?;
        connection.pause_maintenance();
        let live = BlobId::from_bytes([SHARD; 16]);
        let mut orphan = [0x42; 16];
        orphan[0] = SHARD;
        let orphan = BlobId::from_bytes(orphan);

        connection
            .owner
            .state
            .context
            .test_hooks
            .put_outputs
            .lock()
            .unwrap()
            .extend([live, orphan]);

        connection
            .create_bucket(BucketName::parse("attachments")?)
            .await?;

        connection
            .put_object(put("selected"), b"predecessor".as_slice())
            .await?;

        connection
            .put_object(put("orphan"), b"retired bytes".as_slice())
            .await?;

        connection.delete_object(read("orphan"), None).await?;

        let orphan_path = path
            .join("objects")
            .join(format!("{SHARD:02x}"))
            .join(orphan.to_string());

        assert!(orphan_path.exists());
        let mut child = process::Probe::start(&path, "cleanup", CHILD)?;
        assert_eq!(child.observation_async().await?, "CLEANUP_LOCKED");
        let (selected, mut body) = connection.get_object(read("selected")).await?;
        assert_eq!(selected.blob_id, live);
        assert_eq!(body.read_all(100).await?, b"predecessor");
        assert_eq!(child.observation_async().await?, "CLEANUP_FINISHED");
        assert!(!orphan_path.exists());
        child.finish_async().await?;
        connection.close().await?;
        Ok::<_, Box<dyn std::error::Error>>(())
    })?;

    directory.finish()?;
    Ok(())
}

fn checkpoint(point: &str) -> io::Result<()> {
    if point != "fs.Unlink.Before" {
        return Ok(());
    }

    let checkpoint = CHECKPOINT
        .lock()
        .map_err(|_| io::Error::other("cleanup checkpoint poisoned"))?
        .take();

    let Some(checkpoint) = checkpoint else {
        return Ok(());
    };

    checkpoint
        .entered
        .send(())
        .map_err(|()| io::Error::other("cleanup checkpoint observer stopped"))?;

    checkpoint
        .release
        .recv_timeout(Duration::from_secs(10))
        .map_err(io::Error::other)
}

async fn cleanup(connection: Connection) -> io::Result<()> {
    crate::connection::maintenance::batch(
        Arc::new(connection.admit().map_err(io::Error::other)?),
        connection.info().directory.clone(),
        SHARD,
    )
    .await
    .map_err(io::Error::other)?;

    // This read also requires the single native worker after the unlink has
    // settled, so moving stdin into that worker pool cannot pass the scenario.
    let (_, mut body) = connection
        .get_object(read("selected"))
        .await
        .map_err(io::Error::other)?;

    assert_eq!(
        body.read_all(100).await.map_err(io::Error::other)?,
        b"predecessor"
    );

    announce("CLEANUP_FINISHED")
}

#[test]
#[ignore = "Owned subprocess for cleanup progress during a parent control wait"]
fn cleanup_control_child() -> TestResult {
    let path =
        PathBuf::from(std::env::var_os("mikrio_probe_directory").ok_or("missing directory")?);

    runtime()?.block_on(async {
        let connection = Connection::open(path, None).await?;
        connection.pause_maintenance();
        let (entered, observed) = oneshot::channel();
        let (release, released) = mpsc::sync_channel(1);

        *CHECKPOINT.lock().unwrap() = Some(Checkpoint {
            entered,
            release: released,
        });

        crate::protocol_fault::install(checkpoint)?;
        let cleanup = tokio::spawn(cleanup(connection.clone()));
        tokio::time::timeout(Duration::from_secs(10), observed).await??;
        announce("CLEANUP_LOCKED")?;
        release.send(())?;

        // No await separates native release from the parent wait. Cleanup still
        // owns the shard until this runtime can poll its continuation.
        let result = process::wait_for_exit().await;
        cleanup.await??;
        result?;
        connection.close().await?;
        Ok::<_, Box<dyn std::error::Error>>(())
    })
}
