//! Real maintenance fences late parts and respects payload guards and restored ages.

use super::*;
use crate::access::BucketAccessRequest;
use crate::lifecycle::{LifecycleConfiguration, LifecycleFilter, LifecycleRule};
use crate::test_backup::{copy_directory, manifest};

fn bucket() -> BucketAccessRequest {
    BucketAccessRequest {
        bucket: request().bucket,
        expected_owner: None,
    }
}

fn configuration() -> LifecycleConfiguration {
    LifecycleConfiguration::new(vec![LifecycleRule {
        id: Some("abort-drafts".into()),
        is_enabled: true,
        filter: LifecycleFilter::Prefix("durable/".into()),
        expiration_days: None,
        abort_incomplete_multipart_days: Some(1),
    }])
    .unwrap()
}

async fn old_upload(
    connection: &Connection,
    path: &std::path::Path,
) -> Result<UploadId, Box<dyn std::error::Error>> {
    let created = connection.create_multipart_upload(request()).await?;
    let old = UploadId::new(EpochMillis::new(0)?, [1; 16]);
    let sql = rusqlite::Connection::open(path.join("metadata.sqlite3"))?;

    // Test-only age injection precedes every part/claim. Production always uses
    // the immutable time encoded by the real initiation operation.
    sql.execute(
        "UPDATE multipart_uploads SET upload_id=?1, initiated_at_ms=0 WHERE upload_id=?2",
        (old.to_string(), created.upload_id.to_string()),
    )?;

    sql.close().map_err(|(_, error)| error)?;

    Ok(old)
}

async fn wait_retired(path: &std::path::Path, upload_id: &UploadId) -> TestResult {
    let sql = rusqlite::Connection::open(path.join("metadata.sqlite3"))?;

    tokio::time::timeout(Duration::from_secs(20), async {
        loop {
            let count: i64 = sql.query_row(
                "SELECT count(*) FROM multipart_uploads WHERE upload_id=?1",
                [upload_id.to_string()],
                |row| row.get(0),
            )?;

            if count == 0 {
                return Ok::<_, rusqlite::Error>(());
            }

            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    })
    .await??;

    assert_eq!(
        sql.query_row(
            "SELECT count(*) FROM multipart_parts WHERE upload_id=?1",
            [upload_id.to_string()],
            |row| row.get::<_, i64>(0)
        )?,
        0
    );

    sql.close().map_err(|(_, error)| error)?;

    Ok(())
}

#[test]
fn lifecycle_abort_fences_in_flight_parts_and_defers_guarded_payload_reclamation() -> TestResult {
    let directory = TestDirectory::new()?.retain_on_failure();
    let path = directory.path().join("store");

    runtime()?.block_on(async {
        let connection = Connection::open(path.clone(), None).await?;
        connection.pause_maintenance();
        connection.create_bucket(request().bucket).await?;
        let upload = old_upload(&connection, &path).await?;

        connection
            .upload_part(part(upload.clone(), 1), b"retained part".as_slice())
            .await?;

        let sql = rusqlite::Connection::open(path.join("metadata.sqlite3"))?;

        let blob_id: Vec<u8> = sql.query_row(
            "SELECT blob_id FROM multipart_parts WHERE upload_id=?1",
            [upload.to_string()],
            |row| row.get(0),
        )?;

        let blob = BlobId::from_bytes(blob_id.try_into().map_err(|_| "invalid blob id")?);
        sql.close().map_err(|(_, error)| error)?;

        let payload_path = path
            .join("objects")
            .join(format!("{:02x}", blob.shard()))
            .join(blob.to_string());

        let guards = GuardManager::open(&path)?;

        let held = guards
            .try_acquire(GuardTarget::Shard(blob.shard()), GuardMode::Shared)?
            .ok_or("missing payload guard")?;

        let (entered, observed) = oneshot::channel();
        let (release, released) = oneshot::channel();
        let writer = connection.clone();
        let delayed = upload.clone();

        let task = tokio::spawn(async move {
            writer
                .upload_part(
                    part(delayed, 2),
                    Gated {
                        entered: Some(entered),
                        release: released,
                        ready: false,
                        bytes: b"late part",
                    },
                )
                .await
        });

        observed.await?;

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
        wait_retired(&path, &upload).await?;
        assert_eq!(std::fs::read(&payload_path)?, b"retained part");
        release.send(()).map_err(|()| "missing delayed writer")?;
        rejected(&task.await?.unwrap_err(), OperationFailure::NoSuchUpload);
        drop(held);

        tokio::time::timeout(Duration::from_secs(20), async {
            while payload_path.exists() {
                tokio::time::sleep(Duration::from_millis(20)).await;
            }
        })
        .await?;

        connection.close().await?;
        let reopened = Connection::open(path.clone(), None).await?;

        rejected(
            &reopened
                .upload_part(part(upload.clone(), 1), b"late after reopen".as_slice())
                .await
                .unwrap_err(),
            OperationFailure::NoSuchUpload,
        );

        reopened.close().await?;
        Ok::<_, Box<dyn std::error::Error>>(())
    })?;

    directory.finish()?;

    Ok(())
}

#[test]
fn lifecycle_abort_offline_restore_catches_up_without_reanchoring_uploads() -> TestResult {
    let directory = TestDirectory::new()?.retain_on_failure();
    let source = directory.path().join("source");
    let restored = directory.path().join("restored");

    let upload = runtime()?.block_on(async {
        let connection = Connection::open(source.clone(), None).await?;
        connection.pause_maintenance();
        connection.create_bucket(request().bucket).await?;
        let upload = old_upload(&connection, &source).await?;

        connection
            .upload_part(part(upload.clone(), 1), b"old part".as_slice())
            .await?;

        connection
            .put_bucket_lifecycle(bucket(), configuration())
            .await?;

        connection.close().await?;
        Ok::<_, Box<dyn std::error::Error>>(upload)
    })?;

    let frozen = manifest(&source)?;
    copy_directory(&source, &restored)?;
    assert_eq!(manifest(&restored)?, frozen);

    assert_eq!(
        retained_ids(&restored)?.as_slice(),
        std::slice::from_ref(&upload)
    );

    runtime()?.block_on(async {
        for _ in 0..2 {
            let connection = Connection::open(restored.clone(), None).await?;

            assert_eq!(
                connection.get_bucket_lifecycle(bucket()).await?,
                Some(configuration())
            );

            wait_retired(&restored, &upload).await?;
            connection.close().await?;
        }

        Ok::<_, Box<dyn std::error::Error>>(())
    })?;

    assert_eq!(manifest(&source)?, frozen);
    directory.finish()?;

    Ok(())
}

#[test]
fn lifecycle_abort_process_death_preserves_one_atomic_retirement_and_resumes() -> TestResult {
    let _serial = process::process_scenario()?;
    let directory = TestDirectory::new()?.retain_on_failure();

    for effect in ["before", "after"] {
        let path = directory.path().join(effect);

        let upload = runtime()?.block_on(async {
            let connection = Connection::open(path.clone(), None).await?;
            connection.pause_maintenance();
            connection.create_bucket(request().bucket).await?;
            let upload = old_upload(&connection, &path).await?;

            connection
                .upload_part(part(upload.clone(), 1), b"part bytes".as_slice())
                .await?;

            connection
                .put_bucket_lifecycle(bucket(), configuration())
                .await?;

            connection.close().await?;
            Ok::<_, Box<dyn std::error::Error>>(upload)
        })?;

        let checkpoint = format!("lifecycle.upload.expire.commit.{effect}");

        let mut child = process::Probe::start(
            &path,
            &format!("expire:{checkpoint}"),
            "connection::tests::expiration::recovery::lifecycle_process_child",
        )?;

        assert_eq!(child.observation()?, checkpoint);
        child.kill_and_wait()?;
        let sql = rusqlite::Connection::open(path.join("metadata.sqlite3"))?;

        let counts: (i64, i64, i64) = sql.query_row(
            "SELECT (SELECT count(*) FROM multipart_uploads), (SELECT count(*) FROM multipart_parts), (SELECT count(*) FROM multipart_receipts WHERE outcome='aborted')",
            [],
            |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
        )?;

        assert_eq!(
            counts,
            if effect == "before" {
                (1, 1, 0)
            } else {
                (0, 0, 1)
            }
        );

        sql.close().map_err(|(_, error)| error)?;

        runtime()?.block_on(async {
            for _ in 0..2 {
                let connection = Connection::open(path.clone(), None).await?;
                wait_retired(&path, &upload).await?;

                rejected(
                    &connection
                        .upload_part(part(upload.clone(), 2), b"late".as_slice())
                        .await
                        .unwrap_err(),
                    OperationFailure::NoSuchUpload,
                );

                connection.close().await?;
            }

            Ok::<_, Box<dyn std::error::Error>>(())
        })?;
    }

    directory.finish()?;

    Ok(())
}
