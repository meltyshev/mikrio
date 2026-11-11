//! Failed recovery preserves corrupt metadata and every physical payload.

use std::io::{BufRead as _, Seek as _};
use std::path::Path;

use super::objects::{put, read};
use super::*;
use crate::OpenFailure;
use crate::object::BucketName;
use crate::test_backup::{Manifest, manifest};
use crate::test_process as process;

const CHILD: &str = "connection::tests::corrupt_recovery::corrupt_store_child";

fn preserved(path: &Path) -> io::Result<Manifest> {
    let mut files = manifest(path)?;
    // SQLite's shared-memory index is reconstructed during recovery. The
    // database, WAL, marker, guards and payload files retain their own bytes.
    files.remove(Path::new("metadata.sqlite3-shm"));
    Ok(files)
}

fn refusal(mode: &str) -> TestResult {
    let _serial = process::process_scenario()?;
    let directory = TestDirectory::new()?.retain_on_failure();
    let path = directory.path().join("store");

    OpenedStore::open(&path, None)?
        .close()
        .map_err(|(_, error)| error)?;

    let mut child = process::Probe::start(&path, mode, CHILD)?;
    assert_eq!(child.observation()?, "CORRUPTION_READY");
    child.kill_and_wait()?;

    if mode == "database" {
        let mut database = std::fs::OpenOptions::new()
            .write(true)
            .open(path.join("metadata.sqlite3"))?;

        database.seek(io::SeekFrom::Start(0))?;
        database.write_all(b"not a SQLite db!")?;
        database.sync_all()?;
    } else {
        assert!(path.join("metadata.sqlite3-wal").metadata()?.len() > 32);
    }

    let before = preserved(&path)?;

    let payloads = before
        .iter()
        .filter(|(name, facts)| name.starts_with("objects") && facts.is_some())
        .count();

    assert_eq!(
        payloads, 2,
        "live and retired payloads must precede refusal"
    );

    runtime()?.block_on(async {
        for _ in 0..2 {
            let error = Connection::open(path.clone(), None)
                .await
                .err()
                .ok_or("corrupt store unexpectedly opened")?;

            assert!(matches!(
                error,
                ConnectionError::Open(ref cause) if cause.failure() == OpenFailure::StoreCorrupt
            ));

            let after = preserved(&path)?;

            let changed: std::collections::BTreeSet<_> = before
                .keys()
                .chain(after.keys())
                .filter(|name| before.get(*name) != after.get(*name))
                .collect();

            assert!(changed.is_empty(), "{mode} refusal changed {changed:?}");
        }

        Ok::<_, Box<dyn std::error::Error>>(())
    })?;

    directory.finish()?;
    Ok(())
}

#[test]
fn corrupt_database_refusal_preserves_live_and_retired_payloads() -> TestResult {
    refusal("database")
}

#[test]
fn corrupt_schema_in_wal_refusal_preserves_live_and_retired_payloads() -> TestResult {
    refusal("wal")
}

#[test]
fn corrupt_singleton_in_wal_refusal_preserves_live_and_retired_payloads() -> TestResult {
    refusal("singleton")
}

#[test]
fn valid_wal_recovery_restores_normal_close_checkpoint() -> TestResult {
    let _serial = process::process_scenario()?;
    let directory = TestDirectory::new()?.retain_on_failure();
    let path = directory.path().join("store");

    OpenedStore::open(&path, None)?
        .close()
        .map_err(|(_, error)| error)?;

    let mut child = process::Probe::start(&path, "valid", CHILD)?;
    assert_eq!(child.observation()?, "CORRUPTION_READY");
    child.kill_and_wait()?;
    assert!(path.join("metadata.sqlite3-wal").metadata()?.len() > 32);

    runtime()?.block_on(async {
        for _ in 0..2 {
            let connection = Connection::open(path.clone(), Some("eu-west-1".into())).await?;
            let (_, mut body) = connection.get_object(read("live")).await?;
            assert_eq!(body.read_all(32).await?, b"live bytes");
            connection.close().await?;
            assert!(!path.join("metadata.sqlite3-wal").exists());
        }

        Ok::<_, Box<dyn std::error::Error>>(())
    })?;

    directory.finish()?;
    Ok(())
}

#[test]
#[ignore = "Owned subprocess leaves actual metadata and payloads for refused recovery"]
fn corrupt_store_child() -> TestResult {
    let path =
        PathBuf::from(std::env::var_os("mikrio_probe_directory").ok_or("missing owned directory")?);

    runtime()?.block_on(async {
        let connection = Connection::open(path.clone(), None).await?;
        connection.pause_maintenance();

        connection
            .create_bucket(BucketName::parse("attachments")?)
            .await?;

        connection
            .put_object(put("live"), b"live bytes".as_slice())
            .await?;

        connection
            .put_object(put("retired"), b"retired bytes".as_slice())
            .await?;

        connection.delete_object(read("retired"), None).await?;
        connection.close().await?;
        Ok::<_, Box<dyn std::error::Error>>(())
    })?;

    let sql = rusqlite::Connection::open(path.join("metadata.sqlite3"))?;
    sql.execute_batch("PRAGMA wal_autocheckpoint = 0;")?;

    match std::env::var("mikrio_probe_mode")?.as_str() {
        "wal" => sql.execute_batch(
            "PRAGMA writable_schema = ON;
             BEGIN IMMEDIATE;
             UPDATE sqlite_schema SET sql = 'CREATE broken' WHERE name = 'objects';
             PRAGMA schema_version = 777;
             COMMIT;
             PRAGMA writable_schema = OFF;",
        )?,
        "singleton" => sql.execute_batch(
            "PRAGMA ignore_check_constraints = ON;
             UPDATE store_metadata SET owner_account_id = 'invalid';",
        )?,
        "valid" => sql.execute_batch("UPDATE store_metadata SET region = 'eu-west-1';")?,
        "database" => {}
        _ => return Err("unknown corruption fixture".into()),
    }

    let mut output = io::stdout().lock();
    writeln!(output, "PROBE CORRUPTION_READY")?;
    output.flush()?;
    drop(output);
    let mut command = String::new();
    io::stdin().lock().read_line(&mut command)?;
    Err("corrupt fixture must be killed without closing SQLite".into())
}
