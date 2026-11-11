use std::path::Path;

use super::*;
use crate::OperationFailure;
use crate::access::BucketAccessRequest;
use crate::lifecycle::{LifecycleConfiguration, LifecycleFilter, LifecycleRule};
use crate::object::{BucketName, ByteRange};

mod recovery;

fn configuration() -> LifecycleConfiguration {
    LifecycleConfiguration::new(vec![LifecycleRule {
        id: Some("expire".into()),
        is_enabled: true,
        filter: LifecycleFilter::All,
        expiration_days: Some(1),
        abort_incomplete_multipart_days: None,
    }])
    .unwrap()
}

fn bucket() -> BucketAccessRequest {
    BucketAccessRequest {
        bucket: BucketName::parse("attachments").unwrap(),
        expected_owner: None,
    }
}

// This test-only clock fixture changes the age basis of real acknowledged bytes.
// Production publication remains the sole source of that persisted timestamp.
fn age(path: &Path, key: &str, anchor: i64) -> rusqlite::Result<()> {
    let sql = rusqlite::Connection::open(path.join("metadata.sqlite3"))?;

    assert_eq!(
        sql.execute(
            "UPDATE objects SET lifecycle_anchor_at_ms = ?2 WHERE key = ?1",
            (key.as_bytes(), anchor)
        )?,
        1
    );

    sql.close().map_err(|(_, error)| error)
}

async fn wait_missing(connection: &Connection, key: &str) -> TestResult {
    tokio::time::timeout(Duration::from_secs(20), async {
        loop {
            match connection.head_object(objects::read(key)).await {
                Ok(_) => tokio::time::sleep(Duration::from_millis(20)).await,
                Err(error) => {
                    assert_eq!(error.failure(), OperationFailure::NoSuchKey);
                    return;
                }
            }
        }
    })
    .await?;

    Ok(())
}

#[test]
fn automatic_expiration_skips_failed_candidates_and_preserves_full_and_range_readers() -> TestResult
{
    let directory = TestDirectory::new()?.retain_on_failure();

    runtime()?.block_on(async {
        let path = directory.path().join("store");
        let connection = Connection::open(path.clone(), None).await?;
        connection.pause_maintenance();
        connection.create_bucket(bucket().bucket).await?;

        connection
            .put_object(objects::put("bad-age"), b"retained".as_slice())
            .await?;

        connection
            .put_object(objects::put("selected"), b"testcontent".as_slice())
            .await?;

        age(&path, "bad-age", 253_402_300_799_999)?;
        age(&path, "selected", 0)?;
        let (selected, mut full) = connection.get_object(objects::read("selected")).await?;
        let mut ranged = objects::read("selected");
        ranged.range = Some(ByteRange::parse("bytes=4-7")?);
        let (_, mut part) = connection.get_object(ranged).await?;
        assert_eq!(full.read_chunk(4).await?, Some(b"test".to_vec()));
        assert_eq!(part.read_chunk(1).await?, Some(b"c".to_vec()));

        connection
            .put_bucket_lifecycle(bucket(), configuration())
            .await?;

        connection
            .owner
            .state
            .context
            .test_hooks
            .pause_maintenance
            .store(false, Ordering::SeqCst);

        connection.owner.state.context.wake_maintenance.notify_one();
        wait_missing(&connection, "selected").await?;
        connection.head_object(objects::read("bad-age")).await?;

        let payload_path = path
            .join("objects")
            .join(format!("{:02x}", selected.blob_id.shard()))
            .join(selected.blob_id.to_string());

        assert!(payload_path.is_file());
        let manager = GuardManager::open(&path)?;

        assert!(
            manager
                .try_acquire(
                    GuardTarget::Shard(selected.blob_id.shard()),
                    GuardMode::Exclusive
                )?
                .is_none()
        );

        connection.delete_bucket_lifecycle(bucket()).await?;
        assert_eq!(full.read_all(100).await?, b"content");
        assert_eq!(part.read_all(100).await?, b"ont");

        tokio::time::timeout(Duration::from_secs(20), async {
            while payload_path.exists() {
                tokio::time::sleep(Duration::from_millis(20)).await;
            }
        })
        .await?;

        connection.close().await?;
        let reopened = Connection::open(path, None).await?;

        assert_eq!(
            reopened
                .head_object(objects::read("selected"))
                .await
                .unwrap_err()
                .failure(),
            OperationFailure::NoSuchKey
        );

        let (_, mut retained) = reopened.get_object(objects::read("bad-age")).await?;
        assert_eq!(retained.read_all(100).await?, b"retained");
        reopened.close().await?;
        Ok::<_, Box<dyn std::error::Error>>(())
    })?;

    directory.finish()?;
    Ok(())
}

#[test]
fn reopen_resumes_configured_expiration_from_the_persisted_age() -> TestResult {
    let directory = TestDirectory::new()?.retain_on_failure();

    runtime()?.block_on(async {
        let path = directory.path().join("store");
        let connection = Connection::open(path.clone(), None).await?;
        connection.pause_maintenance();
        connection.create_bucket(bucket().bucket).await?;

        connection
            .put_object(objects::put("old"), b"durable".as_slice())
            .await?;

        age(&path, "old", 0)?;

        connection
            .put_bucket_lifecycle(bucket(), configuration())
            .await?;

        connection.head_object(objects::read("old")).await?;
        connection.close().await?;
        let reopened = Connection::open(path, None).await?;
        wait_missing(&reopened, "old").await?;

        assert_eq!(
            reopened.get_bucket_lifecycle(bucket()).await?,
            Some(configuration())
        );

        reopened.close().await?;
        Ok::<_, Box<dyn std::error::Error>>(())
    })?;

    directory.finish()?;
    Ok(())
}
