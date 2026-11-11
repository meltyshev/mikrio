//! Process interruptions and offline copies use the real configuration and GC paths.

use std::io::BufRead as _;

use super::*;
use crate::test_backup::{copy_directory, manifest};
use crate::test_process as process;

const CHILD: &str = "connection::tests::expiration::recovery::lifecycle_process_child";

fn replacement() -> LifecycleConfiguration {
    LifecycleConfiguration::new(vec![LifecycleRule {
        id: Some("replacement".into()),
        is_enabled: false,
        filter: LifecycleFilter::Prefix("later/".into()),
        expiration_days: Some(7),
        abort_incomplete_multipart_days: None,
    }])
    .unwrap()
}

fn checkpoint(point: &str) -> io::Result<()> {
    let mode = std::env::var("mikrio_probe_mode").map_err(io::Error::other)?;

    let (_, target) = mode
        .split_once(':')
        .ok_or_else(|| io::Error::other("missing lifecycle checkpoint"))?;

    if point != target {
        return Ok(());
    }

    let mut output = io::stdout().lock();
    writeln!(output, "PROBE {point}")?;
    output.flush()?;
    drop(output);

    let mut line = String::new();
    io::stdin().lock().read_line(&mut line)?;

    Err(io::Error::other(
        "lifecycle checkpoint unexpectedly released",
    ))
}

fn interrupt(path: &Path, mode: &str) -> TestResult {
    let (_, target) = mode.split_once(':').ok_or("missing checkpoint")?;
    let mut child = process::Probe::start(path, mode, CHILD)?;
    assert_eq!(child.observation()?, target);
    child.kill_and_wait()?;
    Ok(())
}

#[test]
fn configuration_process_death_preserves_the_whole_committed_replacement_or_deletion() -> TestResult
{
    let _serial = process::process_scenario()?;
    let directory = TestDirectory::new()?.retain_on_failure();

    for operation in ["put", "delete"] {
        for effect in ["before", "after"] {
            let path = directory.path().join(format!("{operation}-{effect}"));

            runtime()?.block_on(async {
                let connection = Connection::open(path.clone(), None).await?;
                connection.pause_maintenance();
                connection.create_bucket(bucket().bucket).await?;

                connection
                    .put_bucket_lifecycle(bucket(), configuration())
                    .await?;

                connection.close().await?;
                Ok::<_, Box<dyn std::error::Error>>(())
            })?;

            interrupt(
                &path,
                &format!("{operation}:bucket.lifecycle.{operation}.commit.{effect}"),
            )?;

            let expected = match (operation, effect) {
                (_, "before") => Some(configuration()),
                ("put", "after") => Some(replacement()),
                ("delete", "after") => None,
                _ => return Err("unknown lifecycle interruption scenario".into()),
            };

            // Two independent opens must agree after WAL recovery and checkpointing.
            for _ in 0..2 {
                runtime()?.block_on(async {
                    let connection = Connection::open(path.clone(), None).await?;
                    assert_eq!(connection.get_bucket_lifecycle(bucket()).await?, expected);
                    connection.close().await?;
                    Ok::<_, Box<dyn std::error::Error>>(())
                })?;
            }
        }
    }

    directory.finish()?;
    Ok(())
}

#[test]
fn offline_lifecycle_restore_survives_repeated_expiration_and_gc_interruptions() -> TestResult {
    let _serial = process::process_scenario()?;
    let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../artifacts/restore");
    std::fs::create_dir_all(&root)?;
    let directory = TestDirectory::new_in(&root)?.preserve();
    let source = directory.join("source");
    let backup = directory.join("backup");
    let working = directory.join("working");

    let (identity, expired) = runtime()?.block_on(async {
        let connection = Connection::open(source.clone(), None).await?;
        connection.pause_maintenance();
        connection.create_bucket(bucket().bucket).await?;

        connection
            .put_object(objects::put("old"), b"expired bytes".as_slice())
            .await?;

        connection
            .put_object(objects::put("fresh"), b"retained bytes".as_slice())
            .await?;

        connection
            .put_bucket_lifecycle(bucket(), configuration())
            .await?;

        let expired = connection.head_object(objects::read("old")).await?.blob_id;
        let identity = connection.info().record.store_id;
        connection.close().await?;
        Ok::<_, Box<dyn std::error::Error>>((identity, expired))
    })?;

    // Change only the age of acknowledged bytes while every engine is closed.
    age(&source, "old", 0)?;
    interrupt(&source, "expire:lifecycle.object.expire.commit.before")?;
    let manager = GuardManager::open(&source)?;

    let exclusion = manager
        .try_acquire(GuardTarget::Store, GuardMode::Exclusive)?
        .ok_or("lifecycle source still in use")?;

    let frozen = manifest(&source)?;
    assert!(frozen.contains_key(Path::new("metadata.sqlite3-wal")));
    copy_directory(&source, &backup)?;
    copy_directory(&backup, &working)?;
    assert_eq!(manifest(&source)?, frozen);
    assert_eq!(manifest(&backup)?, frozen);
    assert_eq!(manifest(&working)?, frozen);

    std::fs::write(
        directory.join("manifest.json"),
        serde_json::to_vec_pretty(&frozen)?,
    )?;

    // Lose the logical deletion reply, then kill recovery after deletion marking commits.
    interrupt(&working, "expire:lifecycle.object.expire.commit.after")?;
    interrupt(&working, "expire:gc.mark.commit.after")?;

    let expired_file = working
        .join("objects")
        .join(format!("{:02x}", expired.shard()))
        .join(expired.to_string());

    for _ in 0..2 {
        runtime()?.block_on(async {
            let connection = Connection::open(working.clone(), None).await?;
            assert_eq!(connection.info().record.store_id, identity);

            assert_eq!(
                connection.get_bucket_lifecycle(bucket()).await?,
                Some(configuration())
            );

            assert_eq!(
                connection
                    .head_object(objects::read("old"))
                    .await
                    .unwrap_err()
                    .failure(),
                OperationFailure::NoSuchKey
            );

            let (_, mut body) = connection.get_object(objects::read("fresh")).await?;
            assert_eq!(body.read_all(100).await?, b"retained bytes");

            tokio::time::timeout(Duration::from_secs(20), async {
                while expired_file.exists() {
                    tokio::time::sleep(Duration::from_millis(20)).await;
                }
            })
            .await?;

            connection.close().await?;
            Ok::<_, Box<dyn std::error::Error>>(())
        })?;
    }

    // Recovery and physical deletion in the working copy cannot alter either original.
    assert_eq!(manifest(&source)?, frozen);
    assert_eq!(manifest(&backup)?, frozen);
    drop(exclusion);

    std::fs::write(
        directory.join("result.json"),
        serde_json::to_vec_pretty(&serde_json::json!({
            "scope": "instrumented lifecycle offline restore",
            "store_id": identity.to_string(),
            "interruptions": [
                "lifecycle.object.expire.commit.before",
                "lifecycle.object.expire.commit.after",
                "gc.mark.commit.after"
            ],
            "independent_reopens": 2,
            "source_and_backup_preserved": true
        }))?,
    )?;

    Ok(())
}

#[test]
#[ignore = "Owned lifecycle transaction and recovery process"]
fn lifecycle_process_child() -> TestResult {
    let path =
        PathBuf::from(std::env::var_os("mikrio_probe_directory").ok_or("missing directory")?);

    let mode = std::env::var("mikrio_probe_mode")?;
    crate::protocol_fault::install(checkpoint)?;

    runtime()?.block_on(async {
        let connection = Connection::open(path, None).await?;

        if mode.starts_with("put:") {
            connection.pause_maintenance();

            connection
                .put_bucket_lifecycle(bucket(), replacement())
                .await?;

            return Err("lifecycle replacement missed checkpoint".into());
        }

        if mode.starts_with("delete:") {
            connection.pause_maintenance();
            connection.delete_bucket_lifecycle(bucket()).await?;
            return Err("lifecycle deletion missed checkpoint".into());
        }

        // The real scheduler reaches the selected effect; the parent owns its watchdog.
        std::future::pending::<TestResult>().await
    })
}
