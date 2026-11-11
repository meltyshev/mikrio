//! Preserve a real identity replacement in the shared-to-exclusive upgrade gap.

use std::cell::RefCell;

use super::*;
use crate::test_backup::{Manifest, manifest};

#[derive(Clone, Copy)]
enum ChangedField {
    StoreId,
    Region,
    CreatedAt,
}

struct Replacement {
    path: PathBuf,
    field: ChangedField,
    original: StoreRecord,
    manifest: Option<Manifest>,
    calls: usize,
}

thread_local! {
    static REPLACEMENT: RefCell<Option<Replacement>> = const { RefCell::new(None) };
}

struct ReplacementScope;

impl Drop for ReplacementScope {
    fn drop(&mut self) {
        REPLACEMENT.with_borrow_mut(|state| *state = None);
    }
}

fn replace_under_exclusive_guard(state: &mut Replacement) -> TestResult {
    assert_eq!(state.calls, 0);
    let manager = GuardManager::open(&state.path)?;

    let guard = manager
        .try_acquire(GuardTarget::Store, GuardMode::Exclusive)?
        .expect("the original shared store and SQLite connection are already closed");

    let connection = sql(&state.path)?;

    let changed = match state.field {
        ChangedField::StoreId => {
            let mut bytes = *state.original.store_id.as_bytes();
            bytes[0] ^= 1;
            let id = StoreId::from_bytes(bytes);

            let changed = connection.execute(
                "UPDATE store_metadata SET store_id = ?1",
                [id.as_bytes().as_slice()],
            )?;

            fs::write(
                state.path.join("store.json"),
                IdentityMarker::initializing(id).ready().encode()?,
            )?;

            changed
        }
        ChangedField::Region => {
            connection.execute("UPDATE store_metadata SET region = 'ap-south-1'", [])?
        }
        ChangedField::CreatedAt => connection.execute(
            "UPDATE store_metadata SET created_at_ms = created_at_ms + 1",
            [],
        )?,
    };

    assert_eq!(changed, 1);
    connection.close().map_err(|(_, error)| error)?;
    state.manifest = Some(manifest(&state.path)?);
    state.calls += 1;
    drop(guard);
    Ok(())
}

fn replace_after_shared_close(point: &str) -> io::Result<()> {
    if point != "upgrade.shared_close.after" {
        return Ok(());
    }

    REPLACEMENT.with_borrow_mut(|state| {
        let state = state.as_mut().expect("owned replacement fixture installed");
        replace_under_exclusive_guard(state).map_err(|error| io::Error::other(error.to_string()))
    })
}

fn refuse_replaced_identity(field: ChangedField) -> TestResult {
    let directory = TestDirectory::new()?.retain_on_failure();
    let path = directory.path().join("store");
    let original = seed(&path, true)?;
    let original_marker = fs::read(path.join("store.json"))?;
    let original_schema = schema(&path)?;

    assert!(
        REPLACEMENT
            .replace(Some(Replacement {
                path: path.clone(),
                field,
                original: original.clone(),
                manifest: None,
                calls: 0,
            }))
            .is_none()
    );

    let state_scope = ReplacementScope;
    let fault_scope = fault::install(replace_after_shared_close);
    let result = OpenedStore::open_registered(&path, None, &SYNTHETIC);
    drop(fault_scope);
    let state = REPLACEMENT.take().unwrap();
    drop(state_scope);
    assert_eq!(state.calls, 1);
    assert!(matches!(result, Err(error) if error.failure() == OpenFailure::StoreCorrupt));
    assert_eq!(manifest(&path)?, state.manifest.unwrap());
    assert_eq!(schema(&path)?, original_schema);

    // The exact externally replaced bytes survive; no format transition or repair ran.
    let changed = OpenedStore::open(&path, None)?;
    assert_eq!(changed.record().format_version, 1);
    let mut expected = original.clone();

    match field {
        ChangedField::StoreId => {
            let mut bytes = *original.store_id.as_bytes();
            bytes[0] ^= 1;
            expected.store_id = StoreId::from_bytes(bytes);
        }
        ChangedField::Region => expected.region = "ap-south-1".to_owned(),
        ChangedField::CreatedAt => {
            expected.created_at = EpochMillis::new(original.created_at.get() + 1)?;
        }
    }

    assert_eq!(changed.record(), &expected);
    close(changed)?;

    // Only this fixture's actor restores its own mutation under real exclusion.
    let manager = GuardManager::open(&path)?;

    let guard = manager
        .try_acquire(GuardTarget::Store, GuardMode::Exclusive)?
        .unwrap();

    let connection = sql(&path)?;

    assert_eq!(
        connection.execute(
            "UPDATE store_metadata SET store_id = ?1, region = ?2, created_at_ms = ?3",
            rusqlite::params![
                original.store_id.as_bytes().as_slice(),
                original.region,
                original.created_at.get()
            ],
        )?,
        1
    );

    connection.close().map_err(|(_, error)| error)?;
    fs::write(path.join("store.json"), original_marker)?;
    drop(guard);
    let restored = OpenedStore::open(&path, None)?;
    assert_eq!(restored.record(), &original);
    close(restored)?;
    let upgraded = OpenedStore::open_registered(&path, None, &SYNTHETIC)?;
    let mut upgraded_record = original;
    upgraded_record.format_version = 2;
    assert_eq!(upgraded.record(), &upgraded_record);
    close(upgraded)?;
    directory.finish()?;
    Ok(())
}

#[test]
fn changed_store_id_between_shared_close_and_exclusive_open_is_preserved_and_refused() -> TestResult
{
    refuse_replaced_identity(ChangedField::StoreId)
}

#[test]
fn changed_region_between_shared_close_and_exclusive_open_is_preserved_and_refused() -> TestResult {
    refuse_replaced_identity(ChangedField::Region)
}

#[test]
fn changed_created_at_between_shared_close_and_exclusive_open_is_preserved_and_refused()
-> TestResult {
    refuse_replaced_identity(ChangedField::CreatedAt)
}
