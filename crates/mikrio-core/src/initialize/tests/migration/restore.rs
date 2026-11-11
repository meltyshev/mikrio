//! Populated offline restores through exclusive synthetic upgrades and process death.

use super::*;
use crate::test_backup::{Manifest, copy_directory, manifest};

mod fixture;
mod state;

fn interrupted_restore(phase: Phase) -> TestResult {
    let _serial = process::process_scenario()?;
    let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../artifacts/restore");
    fs::create_dir_all(&root)?;
    let directory = TestDirectory::new_in(&root)?.preserve();
    let source = directory.join("source");
    let backup = directory.join("backup");
    let working = directory.join("migrated");
    let restored = directory.join("restored");
    let expected = fixture::seed(&source)?;

    let manager = GuardManager::open(&source)?;
    let exclusion = acquire(&manager, GuardMode::Exclusive)?;
    let state = state::State::read(&source)?;
    state.verify_populated(&source)?;
    let frozen = manifest(&source)?;
    copy_directory(&source, &backup)?;
    copy_directory(&backup, &working)?;
    assert_eq!(manifest(&backup)?, frozen);
    assert_eq!(manifest(&working)?, frozen);

    fs::write(
        directory.join("manifest.json"),
        serde_json::to_vec_pretty(&frozen)?,
    )?;

    let mut child = process::Probe::start(&working, &format!("{phase:?}"), CHILD)?;
    assert_eq!(child.observation()?, "MIGRATION_CHECKPOINT");
    // An actual second opener attempts admission while migration owns exclusion.
    assert_eq!(failure(&working, None), OpenFailure::StoreBusy);
    child.kill_and_wait()?;
    let committed_version = if phase == Phase::Committed { 2 } else { 1 };

    assert_eq!(
        sql(&working)?.query_row("SELECT format_version FROM store_metadata", [], |row| {
            row.get::<_, i64>(0)
        })?,
        committed_version
    );

    state.verify(&working)?;
    assert_eq!(payloads(&working)?, payloads(&backup)?);

    // The ordinary registry must refuse the test successor; it is not a released format.
    for _ in 0..2 {
        let opened = OpenedStore::open_registered(&working, None, &SYNTHETIC)?;
        let mut identity = expected.identity.clone();
        identity.format_version = 2;
        assert_eq!(opened.record(), &identity);
        close(opened)?;
        state.verify(&working)?;
        assert_eq!(payloads(&working)?, payloads(&backup)?);

        assert_eq!(
            failure(&working, None),
            OpenFailure::UnsupportedStoreVersion
        );
    }

    assert_eq!(
        sql(&working)?.query_row(
            "SELECT migration_name_bytes FROM buckets WHERE name = 'attachments'",
            [],
            |row| row.get::<_, i64>(0)
        )?,
        11
    );

    // Resume the original format in a separate working copy. Its upload is still usable
    // and its completed layout outlives the original parts and pending cleanup.
    copy_directory(&backup, &restored)?;
    fixture::verify_and_resume(&restored, &expected)?;
    assert_eq!(manifest(&source)?, frozen);
    assert_eq!(manifest(&backup)?, frozen);
    drop(exclusion);

    fs::write(
        directory.join("result.json"),
        serde_json::to_vec_pretty(&serde_json::json!({
            "scope": "instrumented populated offline restore and synthetic migration",
            "phase": format!("{phase:?}"), "version_after_kill": committed_version,
            "store_id": expected.identity.store_id.to_string(),
            "second_open": "StoreBusy", "same_version_reopens": 2,
            "active_upload_resumed": true, "completed_parts": 2,
            "source_and_unopened_backup_preserved": true,
            "listener_started": false
        }))?,
    )?;

    Ok(())
}

fn payloads(path: &Path) -> io::Result<Manifest> {
    Ok(manifest(path)?
        .into_iter()
        .filter(|(name, _)| name.starts_with("objects") || name.starts_with("staging"))
        .collect())
}

#[test]
fn restore_migration_killed_after_begin_preserves_features_and_resumes_upload() -> TestResult {
    interrupted_restore(Phase::Begun)
}

#[test]
fn restore_migration_killed_after_transform_preserves_features_and_resumes_upload() -> TestResult {
    interrupted_restore(Phase::Transformed)
}

#[test]
fn restore_migration_killed_after_stamp_preserves_features_and_resumes_upload() -> TestResult {
    interrupted_restore(Phase::Stamped)
}

#[test]
fn restore_migration_killed_after_commit_preserves_features_and_resumes_upload() -> TestResult {
    interrupted_restore(Phase::Committed)
}
