//! Compile real alternate SQLite configurations and require runtime admission refusal.
//! The expected profile is read only by this test binary, never by the library.

use std::io::Write as _;

use super::*;

#[test]
fn actual_sqlite_build_policy_is_enforced_before_fixture_changes()
-> Result<(), Box<dyn std::error::Error>> {
    let expected = match std::env::var("MIKRIO_SQLITE_BUILD_EXPECTATION") {
        Ok(value) => value,
        Err(std::env::VarError::NotPresent) => "qualified".to_owned(),
        Err(error) => return Err(error.into()),
    };

    let (expected_armor, expected_omit_check, refusal) = match expected.as_str() {
        "qualified" => (true, false, None),
        "missing-api-armor" => (false, false, Some("missing SQLite compile option")),
        "omitted-check" => (true, true, Some("unsupported SQLite compile option")),
        _ => return Err("unrecognized test-only SQLite build expectation".into()),
    };

    let directory = TestDirectory::new()?.retain_on_failure();
    let path = directory.path().join("metadata.sqlite3");
    let connection = Connection::open(&path)?;
    connection.pragma_update(None, "journal_mode", "WAL")?;

    connection.execute_batch(
        "CREATE TABLE sentinel (value INTEGER NOT NULL CHECK(value > 0)); INSERT INTO sentinel VALUES (1);",
    )?;

    let source: String = connection.query_row("SELECT sqlite_source_id()", [], |row| row.get(0))?;
    assert_eq!(rusqlite::version_number(), 3_054_001);

    assert_eq!(
        source,
        "2026-10-16 14:45:20 555e07ef33f69a0957d24d8e555b702464a131d6f1607f4c3c1e4551e8e6ca76"
    );

    for name in ["THREADSAFE=1", "DEFAULT_FOREIGN_KEYS"] {
        assert!(
            connection.query_row("SELECT sqlite_compileoption_used(?1)", [name], |row| {
                row.get::<_, bool>(0)
            })?,
            "missing SQLite option {name}"
        );
    }

    let armor: bool = connection.query_row(
        "SELECT sqlite_compileoption_used('ENABLE_API_ARMOR')",
        [],
        |row| row.get(0),
    )?;

    let omit_check: bool = connection.query_row(
        "SELECT sqlite_compileoption_used('OMIT_CHECK')",
        [],
        |row| row.get(0),
    )?;

    assert_eq!((armor, omit_check), (expected_armor, expected_omit_check));

    for name in ["OMIT_WAL", "OMIT_FOREIGN_KEY", "OMIT_TRIGGER"] {
        assert!(
            !connection.query_row("SELECT sqlite_compileoption_used(?1)", [name], |row| {
                row.get::<_, bool>(0)
            })?,
            "unexpected SQLite option {name}"
        );
    }

    let insertion = connection.execute("INSERT INTO sentinel VALUES (0)", []);

    if omit_check {
        assert_eq!(insertion?, 1);
    } else {
        assert!(matches!(
            insertion,
            Err(rusqlite::Error::SqliteFailure(error, _)) if error.code == rusqlite::ErrorCode::ConstraintViolation
        ));
    }

    let result = validate_runtime(&connection);

    if let Some(message) = refusal {
        assert!(matches!(result, Err(MetadataError::Incompatible(actual)) if actual == message));
    } else {
        result?;
    }

    connection.close().map_err(|(_, error)| error)?;
    let original = std::fs::read(&path)?;
    // SqliteFixture skips format discovery for this deliberately minimal table;
    // it still executes the real runtime admission before configuring a handle.
    let opened = MetadataDatabase::open(&path, MetadataOpen::SqliteFixture);

    if let Some(message) = refusal {
        assert!(matches!(opened, Err(MetadataError::Incompatible(actual)) if actual == message));
    } else {
        close(opened?)?;
    }

    assert_eq!(std::fs::read(&path)?, original);
    let mut output = std::io::stdout().lock();

    writeln!(
        output,
        "SQLITE_BUILD_PROBE profile={expected} version={} api_armor={armor} omit_check={omit_check} source={source}",
        rusqlite::version_number()
    )?;

    output.flush()?;
    drop(output);
    directory.finish()?;
    Ok(())
}
