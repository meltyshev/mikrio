//! Explicitly synthetic field evolution over the real bundled DDL and WAL.
//! This is not a registered format transition or a normal-open upgrade path.

use std::io::{BufRead as _, Write as _};

use super::*;
use crate::{GuardManager, GuardMode, GuardTarget};

use crate::test_process as process;

mod refusal;

const BASE_VERSION: i64 = 1;
const SYNTHETIC_VERSION: i64 = 9001;
const CHILD: &str = "metadata::tests::migration::migration_child";

type TestResult = Result<(), Box<dyn std::error::Error>>;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Phase {
    Started,
    Column,
    Backfill,
    Index,
    Version,
    Committed,
}

/// Checkpoints surround actual effects on the selected bundled SQLite. Only
/// this test binary contains the synthetic SQL and injected acknowledgment.
fn migrate(
    database: &mut MetadataDatabase,
    mut checkpoint: impl FnMut(Phase, &Connection) -> rusqlite::Result<()>,
) -> rusqlite::Result<()> {
    database.connection.flush_prepared_statement_cache();

    let transaction = database
        .connection
        .transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)?;

    let result = (|| {
        checkpoint(Phase::Started, &transaction)?;

        transaction.execute_batch(
            "ALTER TABLE buckets ADD COLUMN migration_probe_name_bytes INTEGER
             CHECK (migration_probe_name_bytes IS NULL OR migration_probe_name_bytes >= 0)",
        )?;

        checkpoint(Phase::Column, &transaction)?;

        transaction.execute(
            "UPDATE buckets SET migration_probe_name_bytes = length(CAST(name AS BLOB))",
            [],
        )?;

        checkpoint(Phase::Backfill, &transaction)?;

        transaction.execute_batch(
            "CREATE INDEX migration_probe_names_idx ON buckets (migration_probe_name_bytes)
             WHERE name IS NOT NULL",
        )?;

        checkpoint(Phase::Index, &transaction)?;

        transaction.execute(
            "UPDATE store_metadata SET format_version = ?1 WHERE singleton_id = 1",
            [SYNTHETIC_VERSION],
        )?;

        checkpoint(Phase::Version, &transaction)?;
        transaction.execute_batch("COMMIT")?;
        checkpoint(Phase::Committed, &transaction)
    })();

    if result.is_err() && !transaction.is_autocommit() {
        // Explicit settlement, including a constraint failure after the version
        // stamp. A failed acknowledgment after COMMIT must not trigger rollback.
        transaction.execute_batch("ROLLBACK")?;
    }

    transaction.flush_prepared_statement_cache();
    result
}

fn seed(
    directory: &TestDirectory,
    populated: bool,
) -> Result<MetadataDatabase, Box<dyn std::error::Error>> {
    let mut database = database(directory)?;

    let values = BootstrapValues::generate(
        StoreId::from_bytes([42; 16]),
        "us-east-1".into(),
        EpochMillis::new(1234)?,
    )?;

    database.initialize(&values)?;

    if populated {
        database.connection.execute_batch(
            "INSERT INTO buckets (bucket_id, name, created_at_ms) VALUES
             (x'01010101010101010101010101010101', 'photos', 12),
             (x'02020202020202020202020202020202', NULL, 13);
             INSERT INTO blobs (row_id, blob_id, shard, state, write_kind, target_bucket_id,
                 target_key, created_at_ms, gc_pending, gc_attempt_count) VALUES
             (7, x'01010101010101010101010101010101', 1, 'reserved', 'put_object',
                 x'01010101010101010101010101010101', x'652fcc81', 15, 1, 0),
             (99, x'02020202020202020202020202020202', 2, 'reserved', 'put_object',
                 x'01010101010101010101010101010101', x'61', 16, 1, 0);
             DELETE FROM blobs WHERE row_id = 99;",
        )?;
    }

    Ok(database)
}

#[derive(PartialEq, Eq)]
struct Preserved {
    store_id: Vec<u8>,
    owner: String,
    token: Vec<u8>,
    created_at: i64,
    buckets: Vec<(i64, Vec<u8>, Option<String>, i64)>,
    blobs: Vec<(i64, Vec<u8>, Vec<u8>)>,
    sequences: Vec<(String, i64)>,
}

impl std::fmt::Debug for Preserved {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        // Keep the random continuation-token key out of assertion failure logs.
        formatter
            .debug_struct("Preserved")
            .field("store_id", &self.store_id)
            .field("owner", &self.owner)
            .field("created_at", &self.created_at)
            .field("buckets", &self.buckets)
            .field("blobs", &self.blobs)
            .field("sequences", &self.sequences)
            .finish_non_exhaustive()
    }
}

fn preserved(connection: &Connection) -> rusqlite::Result<Preserved> {
    let (store_id, owner, token, created_at) = connection.query_row(
        "SELECT store_id, owner_account_id, continuation_token_key, created_at_ms
         FROM store_metadata WHERE singleton_id = 1",
        [],
        |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?)),
    )?;

    let buckets = connection
        .prepare("SELECT rowid, bucket_id, name, created_at_ms FROM buckets ORDER BY rowid")?
        .query_map([], |row| {
            Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?))
        })?
        .collect::<Result<_, _>>()?;

    let blobs = connection
        .prepare("SELECT row_id, blob_id, target_key FROM blobs ORDER BY row_id")?
        .query_map([], |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)))?
        .collect::<Result<_, _>>()?;

    let sequences = connection
        .prepare("SELECT name, seq FROM sqlite_sequence ORDER BY name")?
        .query_map([], |row| Ok((row.get(0)?, row.get(1)?)))?
        .collect::<Result<_, _>>()?;

    Ok(Preserved {
        store_id,
        owner,
        token,
        created_at,
        buckets,
        blobs,
        sequences,
    })
}

fn verify(connection: &Connection, expected_version: i64, populated: bool) -> rusqlite::Result<()> {
    let version: i64 = connection.query_row(
        "SELECT format_version FROM store_metadata WHERE singleton_id = 1",
        [],
        |row| row.get(0),
    )?;

    assert_eq!(version, expected_version);

    let fields: i64 = connection.query_row(
        "SELECT count(*) FROM pragma_table_info('buckets') WHERE name = 'migration_probe_name_bytes'",
        [], |row| row.get(0),
    )?;

    assert_eq!(fields, i64::from(expected_version == SYNTHETIC_VERSION));

    if fields == 1 {
        let names = connection
            .prepare("SELECT name, migration_probe_name_bytes FROM buckets ORDER BY name")?
            .query_map([], |row| {
                Ok((
                    row.get::<_, Option<String>>(0)?,
                    row.get::<_, Option<i64>>(1)?,
                ))
            })?
            .collect::<Result<Vec<_>, _>>()?;

        let expected = if populated {
            vec![(None, None), (Some("photos".into()), Some(6))]
        } else {
            vec![]
        };

        assert_eq!(names, expected);
    }

    assert!(!connection.prepare("PRAGMA foreign_key_check")?.exists([])?);
    let integrity: String = connection.query_row("PRAGMA integrity_check", [], |row| row.get(0))?;
    assert_eq!(integrity, "ok");
    Ok(())
}

#[test]
fn synthetic_field_backfill_and_version_rollback_preserve_rows_and_sequence() -> TestResult {
    for populated in [false, true] {
        let directory = TestDirectory::new()?;
        let mut first = seed(&directory, populated)?;
        let before = preserved(&first.connection)?;
        let original_schema = schema(&first.connection)?;

        let second = MetadataDatabase::open(
            &directory.path().join("metadata.sqlite3"),
            MetadataOpen::SqliteFixture,
        )?;

        for failure in [
            Phase::Started,
            Phase::Column,
            Phase::Backfill,
            Phase::Index,
            Phase::Version,
        ] {
            let result = migrate(&mut first, |phase, transaction| {
                // A separate WAL reader sees the whole old state throughout
                // each provisional phase, even after the private version stamp.
                verify(&second.connection, BASE_VERSION, populated)?;
                assert_eq!(preserved(&second.connection)?, before);

                if phase == failure {
                    transaction.execute(
                        "INSERT INTO buckets (bucket_id, name, created_at_ms) VALUES (x'00', 'invalid', 0)",
                        [],
                    )?;
                }

                Ok(())
            });

            assert_eq!(
                result.unwrap_err().sqlite_error_code(),
                Some(ErrorCode::ConstraintViolation)
            );

            assert!(first.connection.is_autocommit());
            assert_eq!(schema(&first.connection)?, original_schema);
            assert_eq!(preserved(&first.connection)?, before);
            verify(&first.connection, BASE_VERSION, populated)?;
        }

        second.connection.execute_batch("BEGIN")?;
        verify(&second.connection, BASE_VERSION, populated)?;

        let result = migrate(&mut first, |phase, _| {
            if phase == Phase::Committed {
                return Err(rusqlite::Error::InvalidQuery);
            }

            Ok(())
        });

        assert!(matches!(result, Err(rusqlite::Error::InvalidQuery)));
        assert!(first.connection.is_autocommit());
        verify(&second.connection, BASE_VERSION, populated)?;
        second.connection.execute_batch("ROLLBACK")?;
        verify(&second.connection, SYNTHETIC_VERSION, populated)?;
        assert_eq!(preserved(&second.connection)?, before);
        close(second)?;
        close(first)?;

        let reopened = MetadataDatabase::open(
            &directory.path().join("metadata.sqlite3"),
            MetadataOpen::SqliteFixture,
        )?;

        verify(&reopened.connection, SYNTHETIC_VERSION, populated)?;
        assert_eq!(preserved(&reopened.connection)?, before);

        assert!(matches!(
            reopened.read_store(),
            Err(MetadataError::UnsupportedVersion(SYNTHETIC_VERSION))
        ));

        reopened.connection.execute(
            "INSERT INTO blobs (blob_id, shard, state, write_kind, target_bucket_id, target_key,
             created_at_ms, gc_pending, gc_attempt_count) VALUES
             (zeroblob(16), 0, 'reserved', 'put_object', zeroblob(16), x'61', 20, 1, 0)",
            [],
        )?;

        assert_eq!(
            reopened.connection.last_insert_rowid(),
            if populated { 100 } else { 1 }
        );

        close(reopened)?;
        directory.finish()?;
    }

    Ok(())
}

#[test]
fn process_death_at_migration_effects_recovers_one_committed_format() -> TestResult {
    let _scenario = process::process_scenario()?;

    for (phase, version, kill) in [
        ("started", BASE_VERSION, true),
        ("column", BASE_VERSION, true),
        ("backfill", BASE_VERSION, true),
        ("index", BASE_VERSION, true),
        ("version", BASE_VERSION, true),
        ("committed", SYNTHETIC_VERSION, true),
        ("committed", SYNTHETIC_VERSION, false),
    ] {
        let directory = TestDirectory::new()?;
        let first = seed(&directory, true)?;
        let before = preserved(&first.connection)?;
        close(first)?;
        let manager = GuardManager::open(directory.path())?;
        let mut child = process::Probe::start(directory.path(), phase, CHILD)?;
        assert_eq!(child.observation()?, "MIGRATION_CHECKPOINT");

        assert!(
            manager
                .try_acquire(GuardTarget::Store, GuardMode::Shared)?
                .is_none()
        );

        if kill {
            child.kill_and_wait()?;
        } else {
            child.finish()?;
        }

        let guard = manager
            .try_acquire(GuardTarget::Store, GuardMode::Exclusive)?
            .unwrap();

        let reopened = MetadataDatabase::open(
            &directory.path().join("metadata.sqlite3"),
            MetadataOpen::SqliteFixture,
        )?;

        verify(&reopened.connection, version, true)?;
        assert_eq!(preserved(&reopened.connection)?, before);
        close(reopened)?;
        drop(guard);
        drop(manager);
        directory.finish()?;
    }

    Ok(())
}

#[test]
#[ignore = "Owned subprocess entry for migration crash scenarios; not a scenario"]
fn migration_child() -> TestResult {
    let directory = std::env::var_os("mikrio_probe_directory").ok_or("missing owned directory")?;
    let path = Path::new(&directory);
    let mode = std::env::var("mikrio_probe_mode")?;

    let target = match mode.as_str() {
        "started" => Phase::Started,
        "column" => Phase::Column,
        "backfill" => Phase::Backfill,
        "index" => Phase::Index,
        "version" => Phase::Version,
        "committed" => Phase::Committed,
        _ => return Err("unknown synthetic migration checkpoint".into()),
    };

    let manager = GuardManager::open(path)?;

    let _guard = manager
        .try_acquire(GuardTarget::Store, GuardMode::Exclusive)?
        .ok_or("migration guard busy")?;

    let mut database =
        MetadataDatabase::open(&path.join("metadata.sqlite3"), MetadataOpen::SqliteFixture)?;

    migrate(&mut database, |phase, _| {
        if phase == target {
            let mut output = std::io::stdout().lock();
            writeln!(output, "PROBE MIGRATION_CHECKPOINT").unwrap();
            output.flush().unwrap();
            drop(output);
            let mut line = String::new();
            std::io::stdin().lock().read_line(&mut line).unwrap();
            assert_eq!(line, "exit\n");
        }

        Ok(())
    })?;

    close(database)?;
    Ok(())
}
