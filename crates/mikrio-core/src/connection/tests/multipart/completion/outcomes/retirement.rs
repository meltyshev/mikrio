//! Real upload/assembly owners across namespace retirement and interrupted cleanup.

use super::*;
use crate::OperationError;
use crate::access::BucketAccessRequest;
use crate::connection::{Command as WorkerCommand, maintenance};
use crate::lifecycle::{LifecycleConfiguration, LifecycleFilter, LifecycleRule};

const RETIREMENT_CHILD: &str =
    "connection::tests::multipart::completion::outcomes::retirement::retirement_process_child";

async fn retire(connection: &Connection) -> Result<Option<[u8; 16]>, OperationError> {
    let operation = Arc::new(connection.admit().map_err(OperationError::from)?);
    let (reply, response) = oneshot::channel();

    connection
        .owner
        .state
        .context
        .sender
        .send(WorkerCommand::Maintenance(maintenance::Command::Retire {
            after: None,
            reply,
            _operation: operation,
        }))
        .await
        .map_err(|_| OperationError::new(OperationFailure::IoError))?;

    let (bucket, result) = response
        .await
        .map_err(|_| OperationError::new(OperationFailure::IoError))?;

    result?;
    Ok(bucket)
}

fn prepare_retirement(path: &std::path::Path) -> TestResult {
    prepare(path)?;

    runtime()?.block_on(async {
        let connection = Connection::open(path.to_path_buf(), None).await?;
        connection.pause_maintenance();

        connection
            .delete_object(
                super::super::super::super::objects::read(request().key.as_str()),
                None,
            )
            .await?;

        let rules = (0..1_000)
            .map(|index| LifecycleRule {
                id: Some(format!("rule-{index}")),
                is_enabled: false,
                filter: LifecycleFilter::All,
                expiration_days: Some(1),
                abort_incomplete_multipart_days: Some(1),
            })
            .collect();

        connection
            .put_bucket_lifecycle(
                BucketAccessRequest {
                    bucket: request().bucket,
                    expected_owner: None,
                },
                LifecycleConfiguration::new(rules)?,
            )
            .await?;

        for index in 0..2 {
            let mut initiation = request();
            initiation.key = ObjectKey::parse(&format!("zz/extra-{index}"))?;
            let key = initiation.key.clone();
            let upload = connection.create_multipart_upload(initiation).await?;
            let mut part = super::super::super::parts::part(upload.upload_id, 1);
            part.key = key;
            connection.upload_part(part, b"extra".as_slice()).await?;
        }

        connection.close().await?;
        Ok::<_, Box<dyn std::error::Error>>(())
    })
}

async fn wait_clean(path: &std::path::Path) -> TestResult {
    tokio::time::timeout(Duration::from_secs(15), async {
        loop {
            let pending = count(
                path,
                "SELECT (SELECT count(*) FROM blobs WHERE gc_pending=1)
                 + (SELECT count(*) FROM multipart_receipts)
                 + (SELECT count(*) FROM buckets WHERE name IS NULL)",
            )?;

            if pending == 0 {
                return Ok::<_, Box<dyn std::error::Error>>(());
            }

            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    })
    .await?
}

#[test]
fn namespace_reuse_fences_guarded_completion_and_preserves_a_committed_receipt() -> TestResult {
    let _serial = process::process_scenario()?;
    let directory = TestDirectory::new()?.retain_on_failure();

    for (index, mode) in [
        "pause:stored.read.before",
        "pause:payload.published",
        "witness:retire",
    ]
    .into_iter()
    .enumerate()
    {
        let path = directory.path().join(format!("store-{index}"));
        prepare_retirement(&path)?;
        let mut child = process::Probe::start(&path, mode, CHILD)?;
        assert_eq!(child.observation()?, "CHECKPOINT");
        let output = output(&path)?;

        runtime()?.block_on(async {
            let connection = Connection::open(path.clone(), None).await?;
            connection.pause_maintenance();

            if index == 2 {
                let error = connection
                    .delete_bucket(request().bucket, None)
                    .await
                    .unwrap_err();

                assert_eq!(error.failure(), OperationFailure::BucketNotEmpty);

                connection
                    .delete_object(
                        super::super::super::super::objects::read(request().key.as_str()),
                        None,
                    )
                    .await?;
            }

            connection.delete_bucket(request().bucket, None).await?;
            connection.create_bucket(request().bucket).await?;

            connection
                .put_object(
                    super::super::super::super::objects::put(request().key.as_str()),
                    b"new namespace".as_slice(),
                )
                .await?;

            let new_upload = connection.create_multipart_upload(request()).await?;

            let mut retired_bucket = None;

            for (pass, remaining_rules) in [744, 488, 232, 0].into_iter().enumerate() {
                let selected = retire(&connection).await?;
                assert!(selected.is_some());

                if pass == 0 {
                    retired_bucket = selected;
                }

                assert_eq!(selected, retired_bucket);

                assert_eq!(
                    count(&path, "SELECT count(*) FROM bucket_lifecycle_rules")?,
                    remaining_rules
                );

                let old_uploads = if index == 2 { 2_i64 } else { 3_i64 };
                let retired_uploads = i64::try_from(pass + 1)?.min(old_uploads);

                assert_eq!(
                    count(&path, "SELECT count(*) FROM multipart_uploads")?,
                    old_uploads - retired_uploads + 1
                );
            }

            assert_eq!(retire(&connection).await?, None);
            assert_eq!(count(&path, "SELECT count(*) FROM multipart_receipts")?, 3);
            assert_eq!(count(&path, "SELECT count(*) FROM multipart_parts")?, 0);
            assert_eq!(bytes(&connection).await?, b"new namespace");

            let manager = GuardManager::open(&path)?;

            assert!(
                manager
                    .try_acquire(GuardTarget::Shard(output.shard()), GuardMode::Exclusive)?
                    .is_none()
            );

            // Resume ordinary maintenance while the writer still owns its
            // guard, then release the writer and require full reclamation.
            connection
                .owner
                .state
                .context
                .test_hooks
                .pause_maintenance
                .store(false, Ordering::SeqCst);

            connection.owner.state.context.wake_maintenance.notify_one();

            child.finish_async().await?;

            assert_eq!(
                child.observation()?,
                if index == 2 {
                    "Committed"
                } else {
                    "NotCommitted"
                }
            );

            wait_clean(&path).await?;
            assert_eq!(bytes(&connection).await?, b"new namespace");

            let listed = connection
                .list_parts(ListPartsRequest {
                    upload: upload(new_upload.upload_id),
                    part_number_marker: 0,
                    max_parts: PartPageSize::default(),
                })
                .await?;

            assert_eq!(listed.parts, Vec::<crate::multipart::ListedPart>::new());
            connection.close().await?;

            let reopened = Connection::open(path.clone(), None).await?;
            assert_eq!(bytes(&reopened).await?, b"new namespace");
            reopened.close().await?;
            Ok::<_, Box<dyn std::error::Error>>(())
        })?;
    }

    directory.finish()?;
    Ok(())
}

fn interrupted_namespace(mode: &str) -> TestResult {
    let _serial = process::process_scenario()?;
    let directory = TestDirectory::new()?.retain_on_failure();

    let path = directory.path().join("store");
    prepare_retirement(&path)?;

    runtime()?.block_on(async {
        let connection = Connection::open(path.clone(), None).await?;
        connection.pause_maintenance();
        connection.delete_bucket(request().bucket, None).await?;
        connection.create_bucket(request().bucket).await?;

        connection
            .put_object(
                super::super::super::super::objects::put(request().key.as_str()),
                b"new namespace".as_slice(),
            )
            .await?;

        connection.close().await?;
        Ok::<_, Box<dyn std::error::Error>>(())
    })?;

    let mut child = process::Probe::start(&path, mode, RETIREMENT_CHILD)?;

    if mode.starts_with("kill:") {
        assert_eq!(child.observation()?, "CHECKPOINT");
        child.kill_and_wait()?;
    } else {
        assert_eq!(
            child.observation()?,
            if mode.ends_with("after") {
                "Unknown"
            } else {
                "NotCommitted"
            }
        );

        child.finish()?;
    }

    let committed = mode.ends_with("after");

    assert_eq!(
        count(&path, "SELECT count(*) FROM bucket_lifecycle_rules")?,
        if committed { 744 } else { 1_000 }
    );

    assert_eq!(
        count(&path, "SELECT count(*) FROM multipart_uploads")?,
        if committed { 2 } else { 3 }
    );

    assert_eq!(
        count(&path, "SELECT count(*) FROM multipart_receipts")?,
        i64::from(committed)
    );

    runtime()?.block_on(async {
        let connection = Connection::open(path.clone(), None).await?;
        wait_clean(&path).await?;
        assert_eq!(bytes(&connection).await?, b"new namespace");
        assert_eq!(count(&path, "SELECT count(*) FROM multipart_uploads")?, 0);

        assert_eq!(
            count(&path, "SELECT count(*) FROM bucket_lifecycle_rules")?,
            0
        );

        connection.close().await?;
        Ok::<_, Box<dyn std::error::Error>>(())
    })?;

    directory.finish()?;
    Ok(())
}

#[test]
fn kill_retirement_begun_preserves_reused_namespace() -> TestResult {
    interrupted_namespace("kill:retirement.begun")
}

#[test]
fn kill_retirement_commit_before_preserves_reused_namespace() -> TestResult {
    interrupted_namespace("kill:retirement.commit.before")
}

#[test]
fn kill_retirement_commit_after_preserves_reused_namespace() -> TestResult {
    interrupted_namespace("kill:retirement.commit.after")
}

#[test]
fn error_retirement_commit_before_preserves_reused_namespace() -> TestResult {
    interrupted_namespace("error:retirement.commit.before")
}

#[test]
fn error_retirement_commit_after_preserves_reused_namespace() -> TestResult {
    interrupted_namespace("error:retirement.commit.after")
}

#[test]
#[ignore = "Owned subprocess entry for namespace cleanup commit outcomes"]
fn retirement_process_child() -> TestResult {
    let path =
        PathBuf::from(std::env::var_os("mikrio_probe_directory").ok_or("missing owned directory")?);

    runtime()?.block_on(async {
        let connection = Connection::open(path, None).await?;
        connection.pause_maintenance();
        crate::protocol_fault::install(checkpoint)?;
        let error = retire(&connection).await.unwrap_err();

        announce(match error.outcome() {
            Some(MutationOutcome::NotCommitted) => "NotCommitted",
            Some(MutationOutcome::Unknown) => "Unknown",
            _ => return Err("invalid retirement outcome".into()),
        })?;

        release()?;
        connection.close().await?;
        Ok::<_, Box<dyn std::error::Error>>(())
    })
}
