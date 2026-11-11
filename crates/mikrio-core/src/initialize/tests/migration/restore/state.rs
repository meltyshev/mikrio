//! Offline projections compare persisted identity, ages, allocation history and references.

use super::*;
use rusqlite::types::Value;

// Only the synthetic version and added bucket column may change. Explicit projections
// retain every preexisting application field, including secrets without printing them.
const PROJECTIONS: &[(&str, &str)] = &[
    (
        "store",
        "SELECT singleton_id, store_id, owner_account_id, canonical_owner_id,
        region, shard_count, shard_algorithm, continuation_token_key, created_at_ms
        FROM store_metadata ORDER BY singleton_id",
    ),
    (
        "principals",
        "SELECT principal_id, name, policy_generation, is_enabled, policy_json,
        created_at_ms FROM principals ORDER BY principal_id",
    ),
    (
        "keys",
        "SELECT access_key_id, principal_id, secret_access_key, created_at_ms
        FROM access_keys ORDER BY access_key_id",
    ),
    (
        "buckets",
        "SELECT bucket_id, name, lifecycle_generation, policy_generation,
        block_public_acls, ignore_public_acls, block_public_policy, restrict_public_buckets,
        cors_rules_json, policy_json, created_at_ms FROM buckets ORDER BY bucket_id",
    ),
    (
        "rules",
        "SELECT bucket_id, rule_index, rule_id, is_enabled, filter_kind, filter_prefix,
        expiration_days, abort_incomplete_multipart_days
        FROM bucket_lifecycle_rules ORDER BY bucket_id, rule_index",
    ),
    (
        "blobs",
        "SELECT row_id, blob_id, shard, state, write_kind, target_bucket_id, target_key,
        target_upload_id, target_part_number, byte_length, part_count, etag, md5_digest,
        checksum_type, checksums_json, created_at_ms, gc_pending, gc_retry_at_ms,
        gc_attempt_count, gc_error_code FROM blobs ORDER BY row_id",
    ),
    (
        "layout",
        "SELECT blob_id, part_number, byte_offset, byte_length, checksums_json
        FROM blob_parts ORDER BY blob_id, part_number",
    ),
    (
        "objects",
        "SELECT row_id, bucket_id, key, generation, blob_id, content_type,
        content_encoding, content_disposition, content_language, cache_control, expires_at_ms,
        user_metadata_json, tags_json, last_modified_at_ms, lifecycle_anchor_at_ms
        FROM objects ORDER BY row_id",
    ),
    (
        "uploads",
        "SELECT row_id, upload_id, bucket_id, key, initiator_id, state,
        completion_claim_id, completion_blob_id, completion_parts_digest, checksum_algorithm,
        checksum_type, content_type, content_encoding, content_disposition, content_language,
        cache_control, expires_at_ms, user_metadata_json, tags_json, initiated_at_ms
        FROM multipart_uploads ORDER BY row_id",
    ),
    (
        "parts",
        "SELECT upload_id, part_number, generation, blob_id, last_modified_at_ms
        FROM multipart_parts ORDER BY upload_id, part_number",
    ),
    (
        "receipts",
        "SELECT row_id, upload_id, bucket_id, key, outcome, completion_claim_id,
        completion_blob_id, completion_parts_digest, result_json, finished_at_ms
        FROM multipart_receipts ORDER BY row_id",
    ),
    (
        "sweeps",
        "SELECT task, shard, generation, cursor_row_id, upper_row_id, started_at_ms
        FROM maintenance_sweeps ORDER BY task, shard",
    ),
    (
        "sequences",
        "SELECT name, seq FROM sqlite_sequence ORDER BY name",
    ),
];

pub(super) struct State {
    tables: Vec<Vec<Vec<Value>>>,
}

impl State {
    pub(super) fn read(path: &Path) -> Result<Self, Box<dyn std::error::Error>> {
        let database = sql(path)?;
        assert!(!database.prepare("PRAGMA foreign_key_check")?.exists([])?);

        assert_eq!(
            database.query_row("PRAGMA integrity_check", [], |row| row.get::<_, String>(0))?,
            "ok"
        );

        let mut tables = Vec::new();

        for (_, query) in PROJECTIONS {
            let mut statement = database.prepare(query)?;
            let count = statement.column_count();

            let rows =
                statement.query_map([], |row| (0..count).map(|index| row.get(index)).collect())?;

            tables.push(rows.collect::<Result<_, _>>()?);
        }

        database.close().map_err(|(_, error)| error)?;
        Ok(Self { tables })
    }

    pub(super) fn verify(&self, path: &Path) -> TestResult {
        let actual = Self::read(path)?;

        for ((name, _), (expected, actual)) in PROJECTIONS
            .iter()
            .zip(self.tables.iter().zip(&actual.tables))
        {
            assert!(expected == actual, "migration changed persisted {name}");
        }

        Ok(())
    }

    pub(super) fn verify_populated(&self, path: &Path) -> TestResult {
        for ((name, _), rows) in PROJECTIONS.iter().zip(&self.tables) {
            assert!(!rows.is_empty(), "empty fixture projection: {name}");
        }

        let database = sql(path)?;

        for (query, count) in [
            (
                "SELECT count(*) FROM multipart_uploads WHERE state = 'open'",
                1,
            ),
            ("SELECT count(*) FROM multipart_parts", 1),
            ("SELECT count(*) FROM blob_parts", 2),
            ("SELECT count(*) FROM objects", 1),
            ("SELECT count(*) FROM blobs WHERE gc_pending = 1", 2),
            ("SELECT count(*) FROM multipart_receipts", 2),
            ("SELECT count(*) FROM access_keys", 1),
            ("SELECT count(*) FROM bucket_lifecycle_rules", 1),
        ] {
            assert_eq!(
                database.query_row(query, [], |row| row.get::<_, i64>(0))?,
                count,
                "{query}"
            );
        }

        let (high_water, surviving): (i64, i64) = database.query_row(
            "SELECT seq, (SELECT max(row_id) FROM multipart_uploads)
             FROM sqlite_sequence WHERE name = 'multipart_uploads'",
            [],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )?;

        assert!(
            high_water > surviving,
            "fixture must retain a deleted allocation maximum"
        );

        database.close().map_err(|(_, error)| error)?;
        Ok(())
    }
}
