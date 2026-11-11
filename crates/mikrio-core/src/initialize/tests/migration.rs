//! Synthetic formats enter through the same open/guard/registry path as production.
//! Successor formats exist only in tests; the production registry has no migrations.

use super::*;
use crate::metadata::migration::Phase;
use crate::metadata::migration::{Format, Registry, Transition};
use std::io::BufRead as _;

use crate::test_process as process;

const CHILD: &str = "initialize::tests::migration::normal_open_migration_child";

mod identity_race;
mod rebuild;
mod restore;

const BASE: &str = include_str!("../../../schema.sql");
const EXTEND: &str = "ALTER TABLE buckets ADD COLUMN migration_name_bytes INTEGER
    CHECK (migration_name_bytes IS NULL OR migration_name_bytes >= 0);
    CREATE INDEX migration_names_idx ON buckets (migration_name_bytes) WHERE name IS NOT NULL;";
static FORMATS: &[Format] = &[
    Format {
        version: 1,
        schema: &[BASE],
    },
    Format {
        version: 2,
        schema: &[BASE, EXTEND],
    },
];
static SYNTHETIC: Registry = Registry {
    checkpoint: None,
    formats: FORMATS,
    transitions: &[Transition {
        from: 1,
        to: 2,
        prepare,
        apply,
        rebuild: false,
    }],
};

#[allow(
    clippy::unnecessary_wraps,
    reason = "matches the compiled prerequisite callback type"
)]
fn prepare(_: &Path) -> Result<(), MetadataError> {
    Ok(())
}

fn apply(connection: &rusqlite::Connection) -> Result<(), MetadataError> {
    connection.execute_batch(EXTEND)?;

    connection.execute(
        "UPDATE buckets SET migration_name_bytes = length(CAST(name AS BLOB))",
        [],
    )?;

    Ok(())
}

fn sql(path: &Path) -> Result<rusqlite::Connection, rusqlite::Error> {
    rusqlite::Connection::open(path.join("metadata.sqlite3"))
}

fn seed(path: &Path, populated: bool) -> Result<StoreRecord, Box<dyn std::error::Error>> {
    let store = OpenedStore::open(path, Some("eu-west-1"))?;
    let record = store.record().clone();
    close(store)?;

    if populated {
        let guard = acquire(&GuardManager::open(path)?, GuardMode::Exclusive)?;
        let connection = sql(path)?;

        connection.execute_batch(
            "PRAGMA foreign_keys = ON;
            INSERT INTO buckets (bucket_id, name, created_at_ms) VALUES
            (x'01010101010101010101010101010101', 'photos', 12),
            (x'02020202020202020202020202020202', NULL, 13);
            INSERT INTO blobs (row_id, blob_id, shard, state, write_kind, target_bucket_id,
                target_key, created_at_ms, gc_pending, gc_attempt_count) VALUES
            (7, x'01010101010101010101010101010101', 1, 'reserved', 'put_object',
                x'01010101010101010101010101010101', x'61', 15, 1, 0),
            (99, x'02020202020202020202020202020202', 2, 'reserved', 'put_object',
                x'01010101010101010101010101010101', x'62', 16, 1, 0);
            DELETE FROM blobs WHERE row_id = 99;",
        )?;

        connection.close().map_err(|(_, error)| error)?;
        drop(guard);
    }

    Ok(record)
}

fn schema(path: &Path) -> Result<Vec<(String, String)>, rusqlite::Error> {
    sql(path)?
        .prepare(
            "SELECT name, sql FROM sqlite_schema WHERE name NOT GLOB 'sqlite_*' ORDER BY name",
        )?
        .query_map([], |row| Ok((row.get(0)?, row.get(1)?)))?
        .collect()
}

#[test]
fn normal_open_upgrades_synthetic_formats_exclusively_and_matches_fresh_creation() -> TestResult {
    for populated in [false, true] {
        let directory = TestDirectory::new()?;
        let path = directory.path().join("store");
        let original = seed(&path, populated)?;
        let peer = OpenedStore::open(&path, None)?;
        let result = OpenedStore::open_registered(&path, None, &SYNTHETIC);
        assert!(matches!(result, Err(error) if error.failure() == OpenFailure::StoreBusy));
        assert_eq!(peer.record(), &original);
        close(peer)?;

        let upgraded = OpenedStore::open_registered(&path, None, &SYNTHETIC)?;
        let mut expected = original;
        expected.format_version = 2;
        assert_eq!(upgraded.record(), &expected);
        close(upgraded)?;
        let fresh = directory.path().join("fresh");
        close(OpenedStore::open_registered(&fresh, None, &SYNTHETIC)?)?;
        assert_eq!(schema(&path)?, schema(&fresh)?);
        close(OpenedStore::open_registered(&path, None, &SYNTHETIC)?)?;
        // The ordinary compiled registry still refuses the synthetic successor.
        assert_eq!(failure(&path, None), OpenFailure::UnsupportedStoreVersion);

        let connection = sql(&path)?;

        let names = connection
            .prepare("SELECT name, migration_name_bytes FROM buckets ORDER BY name")?
            .query_map([], |row| {
                Ok((
                    row.get::<_, Option<String>>(0)?,
                    row.get::<_, Option<i64>>(1)?,
                ))
            })?
            .collect::<Result<Vec<_>, _>>()?;

        assert_eq!(
            names,
            if populated {
                vec![(None, None), (Some("photos".into()), Some(6))]
            } else {
                vec![]
            }
        );

        if populated {
            let sequence: i64 = connection.query_row(
                "SELECT seq FROM sqlite_sequence WHERE name = 'blobs'",
                [],
                |row| row.get(0),
            )?;

            assert_eq!(sequence, 99);
        }

        assert!(!connection.prepare("PRAGMA foreign_key_check")?.exists([])?);

        assert_eq!(
            connection.query_row("PRAGMA integrity_check", [], |row| row.get::<_, String>(0))?,
            "ok"
        );

        connection.close().map_err(|(_, error)| error)?;
        directory.finish()?;
    }

    Ok(())
}

fn fail_prepare(_: &Path) -> Result<(), MetadataError> {
    Err(MetadataError::Io(io::Error::new(
        io::ErrorKind::PermissionDenied,
        "test backup denied",
    )))
}

fn fail_apply(connection: &rusqlite::Connection) -> Result<(), MetadataError> {
    apply(connection)?;

    connection.execute(
        "INSERT INTO buckets (bucket_id, name, created_at_ms) VALUES (x'00', 'invalid', 0)",
        [],
    )?;

    Ok(())
}

#[test]
fn failed_migration_prerequisite_or_sql_preserves_the_original_store() -> TestResult {
    static PREREQUISITE: Registry = Registry {
        checkpoint: None,
        formats: FORMATS,
        transitions: &[Transition {
            from: 1,
            to: 2,
            prepare: fail_prepare,
            apply,
            rebuild: false,
        }],
    };

    static SQL: Registry = Registry {
        checkpoint: None,
        formats: FORMATS,
        transitions: &[Transition {
            from: 1,
            to: 2,
            prepare,
            apply: fail_apply,
            rebuild: false,
        }],
    };

    let directory = TestDirectory::new()?;
    let path = directory.path().join("store");
    let record = seed(&path, true)?;
    let original_schema = schema(&path)?;

    for registry in [&PREREQUISITE, &SQL] {
        assert!(OpenedStore::open_registered(&path, None, registry).is_err());
        assert_eq!(schema(&path)?, original_schema);
        let reopened = OpenedStore::open(&path, None)?;
        assert_eq!(reopened.record(), &record);
        close(reopened)?;
    }

    directory.finish()?;
    Ok(())
}

#[test]
fn invalid_registry_or_matching_version_with_wrong_schema_never_repairs_a_store() -> TestResult {
    static MISSING: Registry = Registry {
        checkpoint: None,
        formats: FORMATS,
        transitions: &[],
    };

    static GAPPED: Registry = Registry {
        checkpoint: None,
        formats: FORMATS,
        transitions: &[Transition {
            from: 1,
            to: 3,
            prepare,
            apply,
            rebuild: false,
        }],
    };

    static DUPLICATE: Registry = Registry {
        checkpoint: None,
        formats: &[
            Format {
                version: 1,
                schema: &[BASE],
            },
            Format {
                version: 1,
                schema: &[BASE],
            },
        ],
        transitions: &[Transition {
            from: 1,
            to: 1,
            prepare,
            apply,
            rebuild: false,
        }],
    };

    let directory = TestDirectory::new()?;
    let path = directory.path().join("store");
    seed(&path, true)?;
    let original_schema = schema(&path)?;

    for registry in [&MISSING, &GAPPED, &DUPLICATE] {
        assert!(OpenedStore::open_registered(&path, None, registry).is_err());
        assert_eq!(schema(&path)?, original_schema);
    }

    let connection = sql(&path)?;

    connection
        .execute_batch("CREATE TRIGGER unexpected AFTER INSERT ON buckets BEGIN SELECT 1; END;")?;

    connection.close().map_err(|(_, error)| error)?;
    let corrupt_schema = schema(&path)?;
    let result = OpenedStore::open_registered(&path, None, &SYNTHETIC);
    assert!(matches!(result, Err(error) if error.failure() == OpenFailure::StoreCorrupt));
    assert_eq!(schema(&path)?, corrupt_schema);
    directory.finish()?;
    Ok(())
}

fn lost_acknowledgment(phase: Phase) -> Result<(), MetadataError> {
    if phase == Phase::Committed {
        return Err(MetadataError::Io(io::Error::other(
            "test acknowledgment lost",
        )));
    }

    Ok(())
}

fn change_owner(connection: &rusqlite::Connection) -> Result<(), MetadataError> {
    apply(connection)?;

    connection.execute(
        "UPDATE store_metadata SET owner_account_id = '123456789012'",
        [],
    )?;

    Ok(())
}

fn change_canonical_owner(connection: &rusqlite::Connection) -> Result<(), MetadataError> {
    apply(connection)?;

    connection.execute(
        "UPDATE store_metadata SET canonical_owner_id = lower(hex(zeroblob(32)))",
        [],
    )?;

    Ok(())
}

fn change_token_key(connection: &rusqlite::Connection) -> Result<(), MetadataError> {
    apply(connection)?;

    connection.execute(
        "UPDATE store_metadata SET continuation_token_key = zeroblob(32)",
        [],
    )?;

    Ok(())
}

fn remove_sweep(connection: &rusqlite::Connection) -> Result<(), MetadataError> {
    apply(connection)?;

    connection.execute(
        "DELETE FROM maintenance_sweeps WHERE task = 'gc_blobs' AND shard = 0",
        [],
    )?;

    Ok(())
}

#[test]
fn migration_rejects_changed_immutable_identity_and_missing_sweeps_before_commit() -> TestResult {
    let transforms = [
        change_owner,
        change_canonical_owner,
        change_token_key,
        remove_sweep,
    ];

    for (index, transform) in transforms.into_iter().enumerate() {
        let directory = TestDirectory::new()?;
        let path = directory.path().join("store");
        let record = seed(&path, true)?;
        let original_schema = schema(&path)?;
        let original = crate::test_backup::manifest(&path)?;

        let registry = Box::leak(Box::new(Registry {
            checkpoint: None,
            formats: FORMATS,
            transitions: Box::leak(Box::new([Transition {
                from: 1,
                to: 2,
                prepare,
                apply: transform,
                rebuild: false,
            }])),
        }));

        let result = OpenedStore::open_registered(&path, None, registry);

        assert!(
            matches!(result, Err(error) if error.failure() == OpenFailure::StoreCorrupt),
            "damaging migration {index} must fail before commit"
        );

        assert_eq!(schema(&path)?, original_schema);
        let reopened = OpenedStore::open(&path, None)?;
        assert_eq!(reopened.record(), &record);
        close(reopened)?;
        assert_eq!(crate::test_backup::manifest(&path)?, original);
        directory.finish()?;
    }

    Ok(())
}

#[test]
fn normal_open_resolves_migration_commit_acknowledgment_from_fresh_state() -> TestResult {
    static LOST_ACK: Registry = Registry {
        formats: FORMATS,
        transitions: SYNTHETIC.transitions,
        checkpoint: Some(lost_acknowledgment),
    };

    let directory = TestDirectory::new()?;
    let path = directory.path().join("store");
    seed(&path, true)?;
    let store = OpenedStore::open_registered(&path, None, &LOST_ACK)?;
    assert_eq!(store.record().format_version, 2);
    close(store)?;
    close(OpenedStore::open_registered(&path, None, &LOST_ACK)?)?;
    directory.finish()?;
    Ok(())
}

fn process_checkpoint(phase: Phase) -> Result<(), MetadataError> {
    let mode = std::env::var("mikrio_probe_mode")
        .map_err(|error| MetadataError::Io(io::Error::other(error)))?;

    if mode != format!("{phase:?}") {
        return Ok(());
    }

    let mut output = io::stdout().lock();
    writeln!(output, "PROBE MIGRATION_CHECKPOINT").map_err(MetadataError::Io)?;
    output.flush().map_err(MetadataError::Io)?;
    drop(output);
    let mut line = String::new();

    io::stdin()
        .lock()
        .read_line(&mut line)
        .map_err(MetadataError::Io)?;

    if line != "exit\n" {
        return Err(MetadataError::Io(io::Error::other(
            "test control pipe closed",
        )));
    }

    Ok(())
}

#[test]
fn killed_normal_open_migrations_recover_old_or_new_state_and_reopen_twice() -> TestResult {
    let _serial = process::process_scenario()?;
    let directory = TestDirectory::new()?;

    for phase in [
        Phase::Begun,
        Phase::Transformed,
        Phase::Stamped,
        Phase::Committed,
    ] {
        let path = directory.path().join(format!("{phase:?}"));
        let original = seed(&path, true)?;
        let mut child = process::Probe::start(&path, &format!("{phase:?}"), CHILD)?;
        assert_eq!(child.observation()?, "MIGRATION_CHECKPOINT");
        // The child owns exclusive store use even after COMMIT acknowledgment.
        assert_eq!(failure(&path, None), OpenFailure::StoreBusy);
        let connection = sql(&path)?;

        let observed: i64 =
            connection.query_row("SELECT format_version FROM store_metadata", [], |row| {
                row.get(0)
            })?;

        assert_eq!(observed, if phase == Phase::Committed { 2 } else { 1 });
        connection.close().map_err(|(_, error)| error)?;
        child.kill_and_wait()?;

        if phase != Phase::Committed {
            let store = OpenedStore::open(&path, None)?;
            assert_eq!(store.record(), &original);
            close(store)?;
        }

        for _ in 0..2 {
            let store = OpenedStore::open_registered(&path, None, &SYNTHETIC)?;
            assert_eq!(store.record().store_id, original.store_id);
            assert_eq!(store.record().format_version, 2);
            close(store)?;
        }
    }

    // Also exercise a clean control-pipe release rather than only SIGKILL exits.
    let path = directory.path().join("released");
    seed(&path, false)?;
    let mut child = process::Probe::start(&path, "Stamped", CHILD)?;
    assert_eq!(child.observation()?, "MIGRATION_CHECKPOINT");
    child.finish()?;
    close(OpenedStore::open_registered(&path, None, &SYNTHETIC)?)?;
    directory.finish()?;
    Ok(())
}

#[test]
#[ignore = "Owned subprocess entry for normal-open migration scenarios"]
fn normal_open_migration_child() -> TestResult {
    static PAUSED: Registry = Registry {
        formats: FORMATS,
        transitions: SYNTHETIC.transitions,
        checkpoint: Some(process_checkpoint),
    };

    let directory = std::env::var_os("mikrio_probe_directory").ok_or("missing owned directory")?;

    close(OpenedStore::open_registered(
        Path::new(&directory),
        None,
        &PAUSED,
    )?)?;

    Ok(())
}
