//! Offline recovery from preserved whole-store copies with an external manifest.

use std::io::BufRead as _;
use std::path::Path;
use std::sync::atomic::AtomicBool;

use sha2::{Digest as _, Sha256};

use super::objects::{put, read};
use super::*;
use crate::object::{BucketName, ByteLength, ObjectTags, UserMetadata};
use crate::test_backup::{copy_directory, manifest};
use crate::test_process as process;
use crate::{MutationOutcome, OpenFailure, OperationFailure};

const CHILD: &str = "connection::tests::restore::restore_fixture_child";
static MARKED: AtomicBool = AtomicBool::new(false);

fn payload() -> Vec<u8> {
    (0u8..251).cycle().take(257 * 1024).collect()
}

fn lost_mark(point: &str) -> io::Result<()> {
    if point == "gc.mark.commit.after" && !MARKED.swap(true, Ordering::SeqCst) {
        return Err(io::Error::other("deletion-marking acknowledgment lost"));
    }

    Ok(())
}

fn pending(path: &Path) -> rusqlite::Result<i64> {
    rusqlite::Connection::open(path.join("metadata.sqlite3"))?.query_row(
        "SELECT count(*) FROM blobs WHERE gc_pending = 1",
        [],
        |row| row.get(0),
    )
}

async fn verify(path: &Path, identity: &str) -> TestResult {
    let connection = Connection::open(path.to_path_buf(), None).await?;
    assert_eq!(connection.info().record.store_id.to_string(), identity);
    assert_eq!(connection.info().record.region, "eu-west-1");
    assert_eq!(connection.info().record.format_version, 1);
    let (selected, mut body) = connection.get_object(read("kept")).await?;

    assert_eq!(
        selected.metadata.entries().get("title").map(String::as_str),
        Some("café")
    );

    assert_eq!(
        selected.tags.entries().get("purpose").map(String::as_str),
        Some("restore")
    );

    assert_eq!(selected.length.get(), 257 * 1024);
    let bytes = body.read_all(300 * 1024).await?;
    assert_eq!(Sha256::digest(&bytes), Sha256::digest(payload()));
    assert_eq!(bytes, payload());

    tokio::time::timeout(Duration::from_secs(20), async {
        while pending(path)? != 0 {
            tokio::time::sleep(Duration::from_millis(20)).await;
        }

        Ok::<_, rusqlite::Error>(())
    })
    .await??;

    connection.close().await?;
    Ok(())
}

#[test]
#[allow(
    clippy::print_stdout,
    reason = "qualification retains the external backup record"
)]
fn offline_unclean_backup_restores_bytes_identity_and_pending_cleanup_independently() -> TestResult
{
    let _serial = process::process_scenario()?;
    let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../artifacts/restore");
    std::fs::create_dir_all(&root)?;
    // The complete frozen backup and any failed working copy survive this test.
    let directory = TestDirectory::new_in(&root)?.preserve();
    let source = directory.join("source");
    let backup = directory.join("backup");
    let working = directory.join("working");

    // Only empty layout creation precedes the timed child. The child owns all
    // seeded data and unclean WAL/cleanup state preserved by the backup drill.
    OpenedStore::open(&source, Some("eu-west-1"))?
        .close()
        .map_err(|(_, error)| error)?;

    let mut child = process::Probe::start(&source, "seed", CHILD)?;
    let identity = child.observation()?;
    assert_eq!(child.observation()?, "UNCLEAN_READY");
    let manager = GuardManager::open(&source)?;

    assert!(
        manager
            .try_acquire(GuardTarget::Store, GuardMode::Exclusive)?
            .is_none()
    );

    child.kill_and_wait()?;

    let exclusion = manager
        .try_acquire(GuardTarget::Store, GuardMode::Exclusive)?
        .ok_or("source still in use after fixture termination")?;

    let frozen = manifest(&source)?;

    assert!(
        frozen
            .get(Path::new("metadata.sqlite3-wal"))
            .is_some_and(|entry| entry.as_ref().is_some_and(|(length, _)| *length > 0))
    );

    assert!(frozen.contains_key(Path::new("metadata.sqlite3-shm")));
    assert!(frozen.contains_key(Path::new("objects/00/.operator-note")));

    let refused = crate::OpenedStore::open(&source, None)
        .err()
        .ok_or("source exclusion bypassed")?;

    assert_eq!(refused.failure(), OpenFailure::StoreBusy);

    copy_directory(&source, &backup)?;
    assert_eq!(manifest(&source)?, frozen);
    assert_eq!(manifest(&backup)?, frozen);

    std::fs::write(
        directory.join("manifest.json"),
        serde_json::to_vec_pretty(&frozen)?,
    )?;

    copy_directory(&backup, &working)?;
    assert_eq!(manifest(&working)?, frozen);
    drop(exclusion);

    // Recovery happens in another process, twice, with the immutable backup
    // retained separately. The initial source is not opened during that drill.
    for _ in 0..2 {
        let mut verifier = process::Probe::start(&working, &format!("verify:{identity}"), CHILD)?;
        assert_eq!(verifier.observation()?, "VERIFIED");
        verifier.finish()?;
    }

    runtime()?.block_on(async {
        let original = Connection::open(source.clone(), None).await?;
        let restored = Connection::open(working.clone(), None).await?;
        assert_eq!(original.info().record, restored.info().record);

        original
            .put_object(put("kept"), b"original changed".as_slice())
            .await?;

        original.close().await?;
        let (_, mut body) = restored.get_object(read("kept")).await?;
        assert_eq!(body.read_all(300 * 1024).await?, payload());

        restored
            .put_object(put("restored-only"), b"independent".as_slice())
            .await?;

        restored.close().await?;
        let original = Connection::open(source.clone(), None).await?;

        assert_eq!(
            original
                .head_object(read("restored-only"))
                .await
                .unwrap_err()
                .failure(),
            OperationFailure::NoSuchKey
        );

        original.close().await?;
        Ok::<_, Box<dyn std::error::Error>>(())
    })?;

    assert_eq!(manifest(&backup)?, frozen);
    incomplete_and_linked_copies(&directory, &backup, &frozen)?;
    assert_eq!(manifest(&backup)?, frozen);

    let record = serde_json::json!({
        "scope": "instrumented offline restore fixture",
        "os": std::env::consts::OS, "architecture": std::env::consts::ARCH,
        "store_id": identity, "region": "eu-west-1", "format_version": 1,
        "verified_payload_bytes": 257 * 1024,
        "manifest_entries": frozen.len(), "recovery_processes": 2,
        "unclean_wal": true, "reserved_and_deleting": true,
        "backup_preserved": true,
    });

    std::fs::write(
        directory.join("result.json"),
        serde_json::to_vec_pretty(&record)?,
    )?;

    println!("OFFLINE_RESTORE {}", directory.display());
    Ok(())
}

fn incomplete_and_linked_copies(
    directory: &Path,
    backup: &Path,
    frozen: &crate::test_backup::Manifest,
) -> TestResult {
    let incomplete = directory.join("incomplete");
    copy_directory(backup, &incomplete)?;

    let current = runtime()?.block_on(async {
        let connection = Connection::open(incomplete.clone(), None).await?;
        connection.pause_maintenance();
        let selected = connection.head_object(read("kept")).await?;
        connection.close().await?;
        Ok::<_, Box<dyn std::error::Error>>(selected.blob_id)
    })?;

    std::fs::remove_file(
        incomplete
            .join("objects")
            .join(format!("{:02x}", current.shard()))
            .join(current.to_string()),
    )?;

    assert_ne!(&manifest(&incomplete)?, frozen);

    runtime()?.block_on(async {
        let connection = Connection::open(incomplete.clone(), None).await?;

        let Err(error) = connection.get_object(read("kept")).await else {
            return Err("incomplete restore served missing bytes".into());
        };

        assert_eq!(error.failure(), OperationFailure::StoreCorrupt);
        connection.close().await?;
        Ok::<_, Box<dyn std::error::Error>>(())
    })?;

    let linked = directory.join("linked");
    copy_directory(backup, &linked)?;

    std::fs::hard_link(
        linked.join("metadata.sqlite3"),
        directory.join("database-alias"),
    )?;

    assert_eq!(
        crate::OpenedStore::open(&linked, None)
            .err()
            .ok_or("hard-linked metadata accepted")?
            .failure(),
        OpenFailure::StoreCorrupt
    );

    Ok(())
}

#[test]
#[ignore = "Owned seed and independent restore process"]
fn restore_fixture_child() -> TestResult {
    let path =
        PathBuf::from(std::env::var_os("mikrio_probe_directory").ok_or("missing directory")?);

    let mode = std::env::var("mikrio_probe_mode")?;

    if let Some(identity) = mode.strip_prefix("verify:") {
        runtime()?.block_on(verify(&path, identity))?;
        announce("VERIFIED")?;
        return control();
    }

    runtime()?.block_on(async {
        let connection = Connection::open(path.clone(), Some("eu-west-1".into())).await?;
        connection.pause_maintenance();

        connection
            .create_bucket(BucketName::parse("attachments")?)
            .await?;

        let mut request = put("kept");
        request.metadata = UserMetadata::new([("title".into(), "café".into())])?;
        request.tags = Some(ObjectTags::new([("purpose".into(), "restore".into())])?);

        connection
            .put_object(request, std::io::Cursor::new(payload()))
            .await?;

        connection
            .put_object(put("retired"), b"retired bytes".as_slice())
            .await?;

        let orphan = connection.head_object(read("retired")).await?.blob_id;
        connection.delete_object(read("retired"), None).await?;
        crate::protocol_fault::install(lost_mark)?;

        let result = super::super::maintenance::batch(
            Arc::new(connection.admit()?),
            path.clone(),
            orphan.shard(),
        )
        .await;

        assert_eq!(
            result.unwrap_err().outcome(),
            Some(MutationOutcome::Unknown)
        );

        let mut request = put("unfinished");
        request.content_length = Some(ByteLength::new(100)?);

        let error = connection
            .put_object(request, b"partial".as_slice())
            .await
            .unwrap_err();

        assert_eq!(error.failure(), OperationFailure::IncompleteBody);
        let sql = rusqlite::Connection::open(path.join("metadata.sqlite3"))?;

        for state in ["reserved", "deleting"] {
            assert_eq!(
                sql.query_row(
                    "SELECT count(*) FROM blobs WHERE state = ?1",
                    [state],
                    |row| row.get::<_, i64>(0)
                )?,
                1
            );
        }

        sql.close().map_err(|(_, error)| error)?;

        std::fs::write(
            path.join("objects/00/.operator-note"),
            b"included hidden file",
        )?;

        announce(&connection.info().record.store_id.to_string())?;
        announce("UNCLEAN_READY")?;
        control()?;
        connection.close().await?;
        Ok::<_, Box<dyn std::error::Error>>(())
    })
}

fn announce(value: &str) -> io::Result<()> {
    let mut output = io::stdout().lock();
    writeln!(output, "PROBE {value}")?;
    output.flush()
}

fn control() -> TestResult {
    let mut line = String::new();
    io::stdin().lock().read_line(&mut line)?;

    if line != "exit\n" {
        return Err("fixture control pipe closed".into());
    }

    Ok(())
}
