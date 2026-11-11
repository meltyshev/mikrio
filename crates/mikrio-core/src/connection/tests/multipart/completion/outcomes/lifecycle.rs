//! Retention actions retain separate authority across a real guarded completion.

use super::*;
use crate::access::BucketAccessRequest;
use crate::connection::tests::{multipart::parts::part, objects};
use crate::lifecycle::{LifecycleConfiguration, LifecycleFilter, LifecycleRule};

const HELD_KEY: &str = "durable/held";

fn rules() -> LifecycleConfiguration {
    LifecycleConfiguration::new(vec![
        LifecycleRule {
            id: Some("objects".into()),
            is_enabled: true,
            filter: LifecycleFilter::Prefix(HELD_KEY.into()),
            expiration_days: Some(1),
            abort_incomplete_multipart_days: None,
        },
        LifecycleRule {
            id: Some("uploads".into()),
            is_enabled: true,
            filter: LifecycleFilter::Prefix(request().key.as_str().into()),
            expiration_days: None,
            abort_incomplete_multipart_days: Some(1),
        },
    ])
    .unwrap()
}

async fn seed(path: &std::path::Path) -> TestResult {
    let connection = Connection::open(path.to_path_buf(), None).await?;
    connection.pause_maintenance();
    connection.create_bucket(request().bucket).await?;

    connection
        .put_object(objects::put(HELD_KEY), b"held across expiration".as_slice())
        .await?;

    let created = connection.create_multipart_upload(request()).await?;
    let old = UploadId::new(EpochMillis::new(0)?, [1; 16]);
    let database = rusqlite::Connection::open(path.join("metadata.sqlite3"))?;

    // Inject old ages before creating child references. Normal initiation owns
    // both the encoded upload time and persisted initiation anchor.
    database.execute(
        "UPDATE multipart_uploads SET upload_id=?1, initiated_at_ms=0 WHERE upload_id=?2",
        (old.to_string(), created.upload_id.to_string()),
    )?;

    database.execute("UPDATE objects SET lifecycle_anchor_at_ms=0", [])?;
    database.close().map_err(|(_, error)| error)?;

    connection
        .upload_part(part(old, 1), b"abc".as_slice())
        .await?;

    connection.close().await?;

    Ok(())
}

#[test]
fn lifecycle_expiration_and_guarded_completion_keep_both_commit_orders_and_reopen() -> TestResult {
    let _serial = process::process_scenario()?;
    let directory = TestDirectory::new()?.retain_on_failure();

    for (complete_first, mode) in [
        (false, "pause:stored.read.before"),
        (true, "pause:multipart.complete.commit.after"),
    ] {
        let path = directory.path().join(if complete_first {
            "completed"
        } else {
            "aborted"
        });

        runtime()?.block_on(seed(&path))?;
        let mut child = process::Probe::start(&path, mode, CHILD)?;
        assert_eq!(child.observation()?, "CHECKPOINT");
        let completion_blob = output(&path)?;

        runtime()?.block_on(async {
            let connection = Connection::open(path.clone(), None).await?;
            let (_, mut held) = connection.get_object(objects::read(HELD_KEY)).await?;

            connection
                .put_bucket_lifecycle(
                    BucketAccessRequest {
                        bucket: request().bucket,
                        expected_owner: None,
                    },
                    rules(),
                )
                .await?;

            tokio::time::timeout(Duration::from_secs(20), async {
                while count(&path, "SELECT count(*) FROM multipart_uploads")? != 0
                    || count(
                        &path,
                        "SELECT count(*) FROM objects WHERE key=CAST('durable/held' AS BLOB)",
                    )? != 0
                {
                    tokio::time::sleep(Duration::from_millis(20)).await;
                }

                Ok::<_, Box<dyn std::error::Error>>(())
            })
            .await??;

            assert!(
                GuardManager::open(&path)?
                    .try_acquire(
                        GuardTarget::Shard(completion_blob.shard()),
                        GuardMode::Exclusive
                    )?
                    .is_none()
            );

            assert_eq!(held.read_all(100).await?, b"held across expiration");
            child.finish_async().await?;

            assert_eq!(
                child.observation()?,
                if complete_first {
                    "Committed"
                } else {
                    "NotCommitted"
                }
            );

            connection.close().await?;

            let reopened = Connection::open(path.clone(), None).await?;

            assert_eq!(
                reopened
                    .head_object(objects::read(HELD_KEY))
                    .await
                    .unwrap_err()
                    .failure(),
                OperationFailure::NoSuchKey
            );

            if complete_first {
                assert_eq!(bytes(&reopened).await?, b"abc");
            } else {
                assert_eq!(
                    reopened
                        .head_object(objects::read(request().key.as_str()))
                        .await
                        .unwrap_err()
                        .failure(),
                    OperationFailure::NoSuchKey
                );
            }

            assert_eq!(count(&path, "SELECT count(*) FROM multipart_uploads")?, 0);
            assert_eq!(count(&path, "SELECT count(*) FROM multipart_parts")?, 0);
            reopened.close().await?;
            Ok::<_, Box<dyn std::error::Error>>(())
        })?;
    }

    directory.finish()?;

    Ok(())
}
