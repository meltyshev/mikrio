//! Typed query and decoder tests over the real schema, using metadata-only
//! fixtures without payload files or object publication.

use std::time::{Duration, Instant};

use crate::object::{ByteLength, ObjectKey};
use rusqlite::{Params, Row, Statement, StatementStatus, params};

use super::*;

mod bucket_listing;
mod expiration;
mod listing;
mod measurements;
mod statement_cache;
mod values;

type TestResult = Result<(), Box<dyn std::error::Error>>;
const BUCKET: [u8; 16] = [1; 16];
const ETAG: &str = "\"d41d8cd98f00b204e9800998ecf8427e\"";
const MD5: [u8; 16] = [
    0xd4, 0x1d, 0x8c, 0xd9, 0x8f, 0x00, 0xb2, 0x04, 0xe9, 0x80, 0x09, 0x98, 0xec, 0xf8, 0x42, 0x7e,
];

const PROJECTION: &str =
    "SELECT objects.key, objects.blob_id, blobs.blob_id, blobs.state, blobs.byte_length,
    blobs.etag, objects.last_modified_at_ms FROM objects
    LEFT JOIN blobs ON blobs.blob_id = objects.blob_id";
const LOOKUP: &str = " WHERE objects.bucket_id = ?1 AND objects.key = ?2";
const PAGE: &str =
    " WHERE objects.bucket_id = ?1 AND objects.key >= ?2 ORDER BY objects.key LIMIT ?3";
const PAGE_UPPER: &str = " WHERE objects.bucket_id = ?1 AND objects.key >= ?2 AND objects.key < ?3
    ORDER BY objects.key LIMIT ?4";
const PAGE_AFTER: &str = " WHERE objects.bucket_id = ?1 AND objects.key >= ?2 AND objects.key > ?3
    ORDER BY objects.key LIMIT ?4";
const PAGE_UPPER_AFTER: &str =
    " WHERE objects.bucket_id = ?1 AND objects.key >= ?2 AND objects.key < ?3
    AND objects.key > ?4 ORDER BY objects.key LIMIT ?5";
const GC: &str = "SELECT row_id, blob_id, state, gc_retry_at_ms FROM blobs
    WHERE gc_pending = 1 AND shard = ?1 AND row_id > ?2 AND row_id <= ?3
    ORDER BY row_id LIMIT ?4";
const RESERVE: &str = "INSERT INTO blobs
    (blob_id, shard, state, write_kind, target_bucket_id, target_key, created_at_ms,
     gc_pending, gc_attempt_count)
    VALUES (?1, ?2, 'reserved', 'put_object', ?3, ?4, 0, 1, 0)";
const READY: &str = "UPDATE blobs SET state = 'ready', target_bucket_id = NULL,
    target_key = NULL, byte_length = 0, etag = ?2, md5_digest = ?3,
    checksums_json = '{}', gc_pending = 0 WHERE blob_id = ?1 AND state = 'reserved'";
const INSERT_OBJECT: &str = "INSERT INTO objects
    (bucket_id, key, generation, blob_id, user_metadata_json, tags_json,
     last_modified_at_ms, lifecycle_anchor_at_ms)
    VALUES (?1, ?2, ?3, ?4, '{}', '{}', 0, 0)";

#[derive(Debug, PartialEq, Eq)]
struct ListingRow {
    key: ObjectKey,
    blob_id: [u8; 16],
    size: u64,
    etag: String,
    last_modified: EpochMillis,
}

/// This projection validates its fields only. Full representation/checksum/tag
/// decoding belong to the consuming engine.
fn decode(row: &Row<'_>) -> Result<ListingRow, MetadataError> {
    let key = row.get_ref(0).map_err(field_error)?;

    let key = key
        .as_blob()
        .map_err(|_| MetadataError::Corrupt("invalid stored key type"))?;

    let key =
        ObjectKey::from_bytes(key).map_err(|_| MetadataError::Corrupt("invalid stored key"))?;

    let blob_id = row.get::<_, Vec<u8>>(1).map_err(field_error)?;

    let blob_id: [u8; 16] = blob_id
        .try_into()
        .map_err(|_| MetadataError::Corrupt("invalid stored blob identity"))?;

    let stored_id = row.get::<_, Option<Vec<u8>>>(2).map_err(field_error)?;
    let state = row.get::<_, Option<String>>(3).map_err(field_error)?;

    if stored_id.as_deref() != Some(blob_id.as_slice()) || state.as_deref() != Some("ready") {
        return Err(MetadataError::Corrupt(
            "missing or non-ready referenced blob",
        ));
    }

    let size = row.get::<_, i64>(4).map_err(field_error)?;

    let size = ByteLength::from_stored(size)
        .map_err(|_| MetadataError::Corrupt("invalid stored object length"))?
        .get();

    let etag = row.get::<_, String>(5).map_err(field_error)?;

    let unquoted = etag
        .strip_prefix('"')
        .and_then(|value| value.strip_suffix('"'))
        .ok_or(MetadataError::Corrupt("invalid stored ETag"))?;

    let (digest, count) = unquoted
        .split_once('-')
        .map_or((unquoted, None), |(digest, count)| (digest, Some(count)));

    if crate::values::decode_hex::<16>(digest.as_bytes()).is_err() {
        return Err(MetadataError::Corrupt("invalid stored ETag"));
    }

    if let Some(count) = count {
        if count.starts_with('0') {
            return Err(MetadataError::Corrupt("invalid stored ETag"));
        }

        if !count.bytes().all(|byte| byte.is_ascii_digit()) {
            return Err(MetadataError::Corrupt("invalid stored ETag"));
        }

        if !count
            .parse::<u16>()
            .is_ok_and(|value| (1..=10_000).contains(&value))
        {
            return Err(MetadataError::Corrupt("invalid stored ETag"));
        }
    }

    let time = row.get::<_, i64>(6).map_err(field_error)?;

    let last_modified = EpochMillis::new(time)
        .map_err(|_| MetadataError::Corrupt("invalid stored modification time"))?;

    Ok(ListingRow {
        key,
        blob_id,
        size,
        etag,
        last_modified,
    })
}

fn field_error(error: rusqlite::Error) -> MetadataError {
    match error {
        rusqlite::Error::InvalidColumnType(..)
        | rusqlite::Error::IntegralValueOutOfRange(..)
        | rusqlite::Error::FromSqlConversionFailure(..) => {
            MetadataError::Corrupt("invalid stored query field type")
        }

        error => MetadataError::Sqlite(error),
    }
}

fn collect(
    statement: &mut Statement<'_>,
    values: impl Params,
) -> Result<Vec<ListingRow>, MetadataError> {
    let mut rows = statement.query(values)?;
    let mut result = Vec::new();

    while let Some(row) = rows.next()? {
        result.push(decode(row)?);
    }

    Ok(result)
}

fn lookup(connection: &Connection, key: &str) -> Result<Option<ListingRow>, MetadataError> {
    let mut statement = connection.prepare(&format!("{PROJECTION}{LOOKUP}"))?;
    let mut rows = statement.query(params![BUCKET, key.as_bytes()])?;
    rows.next()?.map(decode).transpose()
}

/// Exclusive byte successor; an SQL BLOB bound need not be valid UTF-8.
fn successor(prefix: &[u8]) -> Option<Vec<u8>> {
    let end = prefix.iter().rposition(|byte| *byte != u8::MAX)?;
    let mut upper = prefix[..=end].to_vec();
    upper[end] += 1;
    Some(upper)
}

fn page(
    connection: &Connection,
    prefix: &str,
    after: Option<&str>,
    limit: u16,
) -> Result<Vec<ListingRow>, MetadataError> {
    assert!((1..=1000).contains(&limit), "bounded test input");
    let lower = prefix.as_bytes();
    let upper = successor(lower);
    let after = after.map(str::as_bytes);

    let suffix = match (&upper, after) {
        (None, None) => PAGE,
        (Some(_), None) => PAGE_UPPER,
        (None, Some(_)) => PAGE_AFTER,
        (Some(_), Some(_)) => PAGE_UPPER_AFTER,
    };

    let mut statement = connection.prepare(&format!("{PROJECTION}{suffix}"))?;

    let rows = match (&upper, after) {
        (None, None) => collect(&mut statement, params![BUCKET, lower, limit]),
        (Some(upper), None) => collect(&mut statement, params![BUCKET, lower, upper, limit]),
        (None, Some(after)) => collect(&mut statement, params![BUCKET, lower, after, limit]),
        (Some(upper), Some(after)) => {
            collect(&mut statement, params![BUCKET, lower, upper, after, limit])
        }
    }?;

    assert_indexed(&statement);
    Ok(rows)
}

fn assert_indexed(statement: &Statement<'_>) {
    assert_eq!(statement.get_status(StatementStatus::FullscanStep), 0);
    assert_eq!(statement.get_status(StatementStatus::Sort), 0);
    assert_eq!(statement.get_status(StatementStatus::AutoIndex), 0);
}

fn seed(
    directory: &TestDirectory,
    keys: &[String],
) -> Result<MetadataDatabase, Box<dyn std::error::Error>> {
    let mut database = database(directory)?;

    database.initialize(&BootstrapValues::generate(
        StoreId::from_bytes([42; 16]),
        "us-east-1".into(),
        EpochMillis::new(0)?,
    )?)?;

    database.connection.execute(
        "INSERT INTO buckets (bucket_id, name, created_at_ms) VALUES (?1, 'photos', 0)",
        [BUCKET],
    )?;

    let transaction = database.connection.transaction()?;

    {
        let mut reserve = transaction.prepare(RESERVE)?;
        let mut ready = transaction.prepare(READY)?;
        let mut object = transaction.prepare(INSERT_OBJECT)?;

        for (index, key) in keys.iter().enumerate() {
            let id = (u128::try_from(index)? + 1).to_be_bytes();
            reserve.execute(params![id, id[0], BUCKET, key.as_bytes()])?;
            ready.execute(params![id, ETAG, MD5])?;
            object.execute(params![BUCKET, key.as_bytes(), id, id])?;
        }
    }

    transaction.commit()?;
    Ok(database)
}

#[test]
fn blob_bound_queries_preserve_literal_keys_and_bounded_prefix_pages() -> TestResult {
    let directory = TestDirectory::new()?;

    let mut keys: Vec<String> = [
        "a",
        "a/b",
        "a//b/",
        "a/./b",
        "a/../b",
        "a%2Fb",
        "a+b",
        "a\0b",
        "e\u{301}",
        "é",
        "\u{10ffff}",
        "x' OR 1=1 --",
    ]
    .into_iter()
    .map(str::to_owned)
    .collect();

    keys.push("é".repeat(512));
    let database = seed(&directory, &keys)?;
    keys.sort_by(|left, right| left.as_bytes().cmp(right.as_bytes()));
    assert!(lookup(&database.connection, "missing")?.is_none());

    for key in &keys {
        assert_eq!(
            lookup(&database.connection, key)?.unwrap().key.as_str(),
            key
        );
    }

    for prefix in [
        "",
        "a",
        "a/",
        "a//",
        "a/..",
        "é",
        "e\u{301}",
        "\u{10ffff}",
        "missing",
    ] {
        let expected: Vec<_> = keys
            .iter()
            .filter(|key| key.starts_with(prefix))
            .cloned()
            .collect();

        let mut actual = Vec::new();
        let mut after = None;

        loop {
            let rows = page(&database.connection, prefix, after.as_deref(), 2)?;

            if rows.is_empty() {
                break;
            }

            after = rows.last().map(|row| row.key.as_str().to_owned());
            actual.extend(rows.into_iter().map(|row| row.key.as_str().to_owned()));
        }

        assert_eq!(actual, expected, "prefix {prefix:?}");
    }

    assert_eq!(successor(&[]), None);
    assert_eq!(successor(&[255, 255]), None);
    assert_eq!(successor(&[97, 255]), Some(vec![98]));
    assert_eq!(successor("ÿ".as_bytes()), Some(vec![0xc3, 0xc0]));
    assert_eq!(page(&database.connection, "a/", Some("z"), 1)?, vec![]);
    close(database)?;
    directory.finish()?;
    Ok(())
}

#[test]
fn object_projection_reports_corruption_instead_of_hiding_or_defaulting_rows() -> TestResult {
    // Deliberately corrupt a real on-disk schema in isolated test transactions.
    // CHECK bypass is unavailable to the ordinary metadata API.
    for corrupt in [
        "DELETE FROM blobs",
        "UPDATE blobs SET state = 'reserved'",
        "UPDATE blobs SET byte_length = -1",
        "UPDATE blobs SET byte_length = NULL",
        "UPDATE blobs SET etag = NULL",
        "UPDATE blobs SET etag = '\"abc\"'",
        "UPDATE blobs SET etag = char(10)",
        "UPDATE objects SET last_modified_at_ms = 253402300800000",
        "UPDATE objects SET key = x'ff'",
        "UPDATE objects SET key = x''",
        "UPDATE objects SET key = zeroblob(1025)",
        "UPDATE objects SET key = CAST('../invalid' AS BLOB)",
    ] {
        let directory = TestDirectory::new()?;
        let database = seed(&directory, &["a".into()])?;

        database.connection.execute_batch(
            "PRAGMA foreign_keys = OFF; PRAGMA ignore_check_constraints = ON; BEGIN",
        )?;

        database.connection.execute_batch(corrupt)?;

        assert!(
            matches!(
                page(&database.connection, "", None, 10),
                Err(MetadataError::Corrupt(_))
            ),
            "{corrupt}"
        );

        database.connection.execute_batch(
            "ROLLBACK; PRAGMA ignore_check_constraints = OFF; PRAGMA foreign_keys = ON",
        )?;

        assert_eq!(page(&database.connection, "", None, 10)?.len(), 1);
        close(database)?;
        directory.finish()?;
    }

    let directory = TestDirectory::new()?;
    let database = seed(&directory, &["large".into()])?;

    database
        .connection
        .execute("UPDATE blobs SET byte_length = ?1", [i64::MAX])?;

    assert_eq!(
        lookup(&database.connection, "large")?.unwrap().size,
        u64::try_from(i64::MAX)?
    );

    database.connection.execute_batch("DROP TABLE objects")?;

    assert!(matches!(
        lookup(&database.connection, "large"),
        Err(MetadataError::Sqlite(_))
    ));

    close(database)?;
    directory.finish()?;
    Ok(())
}

#[test]
fn publication_shaped_sql_rolls_back_atomically_and_retains_object_row_identity() -> TestResult {
    let directory = TestDirectory::new()?;
    let mut database = seed(&directory, &["a".into()])?;
    let before = lookup(&database.connection, "a")?.unwrap();
    let second = [2; 16];

    database
        .connection
        .execute(RESERVE, params![second, second[0], BUCKET, b"a"])?;

    let reader = MetadataDatabase::open(
        &directory.path().join("metadata.sqlite3"),
        MetadataOpen::Existing,
    )?;

    for fail in [true, false] {
        let transaction = database
            .connection
            .transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)?;

        assert_eq!(transaction.execute(READY, params![second, ETAG, MD5])?, 1);
        assert_eq!(lookup(&reader.connection, "a")?.unwrap(), before);
        let generation: &[u8] = if fail { &[] } else { &second };

        let update = transaction.execute(
            "UPDATE objects SET blob_id = ?1, generation = ?2 WHERE bucket_id = ?3 AND key = ?4",
            params![second, generation, BUCKET, b"a"],
        );

        if fail {
            assert_eq!(
                update.unwrap_err().sqlite_error_code(),
                Some(ErrorCode::ConstraintViolation)
            );

            transaction.rollback()?;
            assert_eq!(lookup(&database.connection, "a")?.unwrap(), before);

            let state: String = database.connection.query_row(
                "SELECT state FROM blobs WHERE blob_id = ?1",
                [second],
                |row| row.get(0),
            )?;

            assert_eq!(state, "reserved");
            continue;
        }

        assert_eq!(update?, 1);

        transaction.execute(
            "UPDATE blobs SET gc_pending = 1 WHERE blob_id = ?1",
            [before.blob_id],
        )?;

        transaction.commit()?;
    }

    assert_eq!(lookup(&reader.connection, "a")?.unwrap().blob_id, second);

    let identity: (i64, Vec<u8>) = database.connection.query_row(
        "SELECT row_id, generation FROM objects WHERE bucket_id = ?1 AND key = ?2",
        params![BUCKET, b"a"],
        |row| Ok((row.get(0)?, row.get(1)?)),
    )?;

    assert_eq!(identity, (1, second.to_vec()));

    assert!(
        !database
            .connection
            .prepare("PRAGMA foreign_key_check")?
            .exists([])?
    );

    close(reader)?;
    close(database)?;

    let reopened = MetadataDatabase::open(
        &directory.path().join("metadata.sqlite3"),
        MetadataOpen::Existing,
    )?;

    assert_eq!(lookup(&reopened.connection, "a")?.unwrap().blob_id, second);
    close(reopened)?;
    directory.finish()?;
    Ok(())
}
