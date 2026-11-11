//! Destructive synthetic rebuild with a verified full offline backup prerequisite.

use super::*;
use crate::test_backup::{copy_directory, manifest};

fn backup(directory: &Path) -> Result<(), MetadataError> {
    let target = directory.with_extension("backup");
    let original = manifest(directory).map_err(MetadataError::Io)?;
    copy_directory(directory, &target).map_err(MetadataError::Io)?;

    if original != manifest(&target).map_err(MetadataError::Io)? {
        return Err(MetadataError::Corrupt("backup manifest mismatch"));
    }

    Ok(())
}

fn rebuild(connection: &rusqlite::Connection) -> Result<(), MetadataError> {
    let sequence: i64 = connection.query_row(
        "SELECT seq FROM sqlite_sequence WHERE name = 'blobs'",
        [],
        |row| row.get(0),
    )?;

    let begin = BASE
        .find("CREATE TABLE blobs (")
        .ok_or(MetadataError::Corrupt("test DDL missing blobs"))?;

    let end = BASE
        .find("CREATE INDEX blobs_gc_idx")
        .ok_or(MetadataError::Corrupt("test DDL missing index"))?;

    let source = BASE
        .get(begin..end)
        .ok_or(MetadataError::Corrupt("test DDL boundaries"))?;

    connection.execute_batch(&source.replacen(
        "CREATE TABLE blobs",
        "CREATE TABLE replacement_blobs",
        1,
    ))?;

    connection.execute_batch(
        "INSERT INTO replacement_blobs (
        row_id, blob_id, shard, state, write_kind, target_bucket_id, target_key,
        target_upload_id, target_part_number, byte_length, part_count, etag, md5_digest,
        checksum_type, checksums_json, created_at_ms, gc_pending, gc_retry_at_ms,
        gc_attempt_count, gc_error_code)
        SELECT row_id, blob_id, shard, state, write_kind, target_bucket_id, target_key,
        target_upload_id, target_part_number, byte_length, part_count, etag, md5_digest,
        checksum_type, checksums_json, created_at_ms, gc_pending, gc_retry_at_ms,
        gc_attempt_count, gc_error_code FROM blobs;
        DROP TABLE blobs;
        ALTER TABLE replacement_blobs RENAME TO blobs;
        CREATE INDEX blobs_gc_idx ON blobs (shard, row_id) WHERE gc_pending = 1;",
    )?;

    // The explicit copied rows alone cannot retain a deleted maximum or the
    // sequence of an empty table. Restore its committed allocation high water.
    connection.execute(
        "UPDATE sqlite_sequence SET seq = ?1 WHERE name = 'blobs'",
        [sequence],
    )?;

    connection.execute(
        "UPDATE maintenance_sweeps SET cursor_row_id = 0, upper_row_id = 0, generation = randomblob(16) WHERE task = 'gc_blobs'",
        [],
    )?;

    Ok(())
}

fn registry() -> &'static Registry {
    // The target fixture is a separate direct-creation DDL. ALTER RENAME records
    // a quoted table name; exact schema checks intentionally include that text.
    let ddl = Box::leak(
        BASE.replacen("CREATE TABLE blobs (", "CREATE TABLE \"blobs\" (", 1)
            .into_boxed_str(),
    );

    let target = Box::leak(Box::new([&*ddl]));

    let formats = Box::leak(Box::new([
        Format {
            version: 1,
            schema: &[BASE],
        },
        Format {
            version: 2,
            schema: target,
        },
    ]));

    Box::leak(Box::new(Registry {
        formats,
        transitions: &[Transition {
            from: 1,
            to: 2,
            prepare: backup,
            apply: rebuild,
            rebuild: true,
        }],
        checkpoint: None,
    }))
}

fn rebuilt_tables_preserve_sequence_and_verified_backup(empty: bool) -> TestResult {
    use std::io::Write as _;

    let started = std::time::Instant::now();

    let progress = |phase: &str| {
        writeln!(
            std::io::stderr().lock(),
            "rebuild empty={empty} {phase}: {:?}",
            started.elapsed()
        )
    };

    let directory = TestDirectory::new()?;
    let path = directory.path().join("store");
    let identity = seed(&path, true)?;
    progress("seeded")?;

    if empty {
        let connection = sql(&path)?;
        connection.execute("DELETE FROM blobs", [])?;
        connection.close().map_err(|(_, error)| error)?;
    } else {
        fs::write(
            path.join("staging/01/01010101010101010101010101010101"),
            b"reserved bytes",
        )?;
    }

    let registry = registry();
    let store = OpenedStore::open_registered(&path, None, registry)?;
    assert_eq!(store.record().store_id, identity.store_id);
    close(store)?;
    progress("rebuilt with verified backup")?;
    let backup = path.with_extension("backup");
    let frozen = manifest(&backup)?;
    let restored = directory.path().join("restored");
    copy_directory(&backup, &restored)?;
    assert_eq!(manifest(&restored)?, frozen);
    let original = OpenedStore::open(&restored, None)?;
    assert_eq!(original.record(), &identity);
    close(original)?;
    assert_eq!(manifest(&backup)?, frozen);
    progress("restored backup verified")?;

    let connection = sql(&path)?;
    let count: i64 = connection.query_row("SELECT count(*) FROM blobs", [], |row| row.get(0))?;
    assert_eq!(count, i64::from(!empty));

    connection.execute(
        "INSERT INTO blobs (blob_id, shard, state, write_kind, target_bucket_id,
         target_key, created_at_ms, gc_pending, gc_attempt_count)
         VALUES (zeroblob(16), 0, 'reserved', 'put_object', zeroblob(16), x'61', 20, 1, 0)",
        [],
    )?;

    assert_eq!(connection.last_insert_rowid(), 100);
    assert!(!connection.prepare("PRAGMA foreign_key_check")?.exists([])?);

    assert_eq!(
        connection.query_row("PRAGMA integrity_check", [], |row| row.get::<_, String>(0))?,
        "ok"
    );

    connection.close().map_err(|(_, error)| error)?;
    let fresh = directory.path().join("fresh");
    close(OpenedStore::open_registered(&fresh, None, registry)?)?;
    assert_eq!(schema(&path)?, schema(&fresh)?);
    close(OpenedStore::open_registered(&path, None, registry)?)?;
    directory.finish()?;
    progress("fresh schema, reopen and cleanup verified")?;
    Ok(())
}

#[test]
fn rebuilt_empty_table_preserves_sequence_and_verified_backup() -> TestResult {
    rebuilt_tables_preserve_sequence_and_verified_backup(true)
}

#[test]
fn rebuilt_populated_table_preserves_sequence_and_verified_backup() -> TestResult {
    rebuilt_tables_preserve_sequence_and_verified_backup(false)
}
