use super::*;
use crate::object::BucketName;
use std::path::Path;

type TestResult = Result<(), Box<dyn std::error::Error>>;
type Rows = Vec<Vec<rusqlite::types::Value>>;

#[derive(Debug, PartialEq)]
struct Snapshot {
    schema: Vec<(String, String, String, String)>,
    tables: Vec<Rows>,
}

fn snapshot(connection: &Connection) -> Result<Snapshot, rusqlite::Error> {
    let mut tables = Vec::new();

    for table in [
        "store_metadata",
        "principals",
        "access_keys",
        "buckets",
        "bucket_lifecycle_rules",
        "blobs",
        "blob_parts",
        "objects",
        "multipart_uploads",
        "multipart_parts",
        "multipart_receipts",
        "maintenance_sweeps",
        "sqlite_sequence",
    ] {
        // Identifiers come only from this fixed schema inventory.
        let mut statement = connection.prepare(&format!("SELECT * FROM {table} ORDER BY 1, 2"))?;
        let columns = statement.column_count();

        tables.push(
            statement
                .query_map([], |row| {
                    (0..columns)
                        .map(|column| row.get(column))
                        .collect::<Result<_, _>>()
                })?
                .collect::<Result<Rows, _>>()?,
        );
    }

    Ok(Snapshot {
        schema: schema(connection)?,
        tables,
    })
}

fn initialized(directory: &TestDirectory) -> Result<StoreRecord, Box<dyn std::error::Error>> {
    let mut opened = database(directory)?;

    opened.initialize(&BootstrapValues::generate(
        StoreId::from_bytes([17; 16]),
        "eu-west-1".into(),
        EpochMillis::new(123)?,
    )?)?;

    opened.create_bucket(&BucketName::parse("preserved-policy-fixture")?)?;
    let identity = opened.read_store()?;
    close(opened)?;
    Ok(identity)
}

fn native_close(connection: Connection) -> Result<(), rusqlite::Error> {
    connection.close().map_err(|(_, error)| error)
}

fn verify_reopen(path: &Path, identity: &StoreRecord) -> TestResult {
    let mut opened = MetadataDatabase::open(path, MetadataOpen::Existing)?;
    assert_eq!(&opened.read_store()?, identity);

    assert_eq!(
        opened.head_bucket(&BucketName::parse("preserved-policy-fixture")?, None)?,
        "eu-west-1"
    );

    close(opened)?;
    Ok(())
}

#[test]
fn recognized_delete_journal_is_refused_without_rewriting_namespace() -> TestResult {
    let directory = TestDirectory::new()?;
    let identity = initialized(&directory)?;
    let path = directory.path().join("metadata.sqlite3");
    let native = Connection::open(&path)?;
    native.pragma_update(None, "journal_mode", "DELETE")?;
    let before = snapshot(&native)?;
    native_close(native)?;

    assert!(matches!(
        MetadataDatabase::open(&path, MetadataOpen::Existing),
        Err(MetadataError::Corrupt(
            "unexpected SQLite journal or locking mode"
        ))
    ));

    let native = Connection::open(&path)?;

    assert_eq!(
        native.pragma_query_value::<String, _>(None, "journal_mode", |row| row.get(0))?,
        "delete"
    );

    assert_eq!(snapshot(&native)?, before);
    native.pragma_update(None, "journal_mode", "WAL")?;
    native_close(native)?;
    verify_reopen(&path, &identity)?;
    directory.finish()?;
    Ok(())
}

fn refuses_auto_vacuum(mode: i64) -> TestResult {
    let directory = TestDirectory::new()?;
    let identity = initialized(&directory)?;
    let path = directory.path().join("metadata.sqlite3");
    let native = Connection::open(&path)?;
    native.pragma_update(None, "auto_vacuum", mode)?;
    native.execute_batch("VACUUM")?;

    assert_eq!(
        native.pragma_query_value::<i64, _>(None, "auto_vacuum", |row| row.get(0))?,
        mode
    );

    let before = snapshot(&native)?;
    native_close(native)?;

    assert!(matches!(
        MetadataDatabase::open(&path, MetadataOpen::Existing),
        Err(MetadataError::Incompatible(
            "SQLite setting did not take effect"
        ))
    ));

    let native = Connection::open(&path)?;

    assert_eq!(
        native.pragma_query_value::<i64, _>(None, "auto_vacuum", |row| row.get(0))?,
        mode
    );

    assert_eq!(
        native.pragma_query_value::<String, _>(None, "journal_mode", |row| row.get(0))?,
        "wal"
    );

    assert_eq!(snapshot(&native)?, before);
    native.pragma_update(None, "auto_vacuum", 0)?;
    native.execute_batch("VACUUM")?;
    native_close(native)?;
    verify_reopen(&path, &identity)?;
    directory.finish()?;
    Ok(())
}

#[test]
fn full_auto_vacuum_is_refused_without_erasing_namespace() -> TestResult {
    refuses_auto_vacuum(1)
}

#[test]
fn incremental_auto_vacuum_is_refused_without_erasing_namespace() -> TestResult {
    refuses_auto_vacuum(2)
}

#[test]
fn directory_metadata_is_refused_without_mutation() -> TestResult {
    let directory = TestDirectory::new()?;
    let path = directory.path().join("metadata.sqlite3");
    std::fs::create_dir(&path)?;
    let witness = path.join("keep");
    std::fs::write(&witness, b"unowned directory content")?;

    assert!(matches!(
        MetadataDatabase::open(&path, MetadataOpen::Existing),
        Err(MetadataError::Corrupt("metadata is not a regular file"))
    ));

    assert!(path.symlink_metadata()?.is_dir());
    assert_eq!(std::fs::read(witness)?, b"unowned directory content");

    let entries = std::fs::read_dir(path)?
        .map(|entry| entry.map(|entry| entry.file_name()))
        .collect::<Result<Vec<_>, _>>()?;

    assert_eq!(entries, vec![std::ffi::OsString::from("keep")]);
    directory.finish()?;
    Ok(())
}

#[cfg(any(unix, windows))]
#[test]
fn symlink_metadata_is_refused_without_mutating_its_target() -> TestResult {
    let directory = TestDirectory::new()?;
    let identity = initialized(&directory)?;
    let path = directory.path().join("metadata.sqlite3");
    let target = directory.path().join("retained.sqlite3");
    std::fs::rename(&path, &target)?;
    let before = std::fs::read(&target)?;
    #[cfg(unix)]
    std::os::unix::fs::symlink("retained.sqlite3", &path)?;
    #[cfg(windows)]
    std::os::windows::fs::symlink_file("retained.sqlite3", &path)?;

    assert!(matches!(
        MetadataDatabase::open(&path, MetadataOpen::Existing),
        Err(MetadataError::Corrupt("metadata is not a regular file"))
    ));

    assert_eq!(std::fs::read_link(&path)?, Path::new("retained.sqlite3"));
    assert_eq!(std::fs::read(&target)?, before);
    assert!(!directory.path().join("retained.sqlite3-wal").exists());
    assert!(!directory.path().join("retained.sqlite3-shm").exists());
    std::fs::remove_file(&path)?;
    std::fs::rename(target, &path)?;
    verify_reopen(&path, &identity)?;
    directory.finish()?;
    Ok(())
}
