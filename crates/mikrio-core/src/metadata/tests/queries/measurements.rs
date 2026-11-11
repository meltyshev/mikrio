//! Repeatable observations, not timing-based pass thresholds or engine budgets.

use super::*;

#[derive(Debug, PartialEq, Eq)]
enum BlobState {
    Reserved,
    Ready,
    Deleting,
}

#[derive(Debug, PartialEq, Eq)]
struct Candidate {
    row_id: i64,
    blob_id: [u8; 16],
    state: BlobState,
    retry_at: Option<EpochMillis>,
}

fn candidates(
    statement: &mut Statement<'_>,
    shard: u8,
    after: i64,
    upper: i64,
    limit: u16,
) -> Result<Vec<Candidate>, MetadataError> {
    assert!(
        after >= 0 && upper >= after && (1..=256).contains(&limit),
        "bounded probe input"
    );

    let mut rows = statement.query(params![shard, after, upper, limit])?;
    let mut result = Vec::new();

    while let Some(row) = rows.next()? {
        let row_id = row.get::<_, i64>(0).map_err(field_error)?;
        let blob_id = row.get::<_, Vec<u8>>(1).map_err(field_error)?;

        let blob_id: [u8; 16] = blob_id
            .try_into()
            .map_err(|_| MetadataError::Corrupt("invalid candidate identity"))?;

        if row_id <= 0 || blob_id[0] != shard {
            return Err(MetadataError::Corrupt("invalid candidate scan identity"));
        }

        let state = row.get::<_, String>(2).map_err(field_error)?;

        let state = match state.as_str() {
            "reserved" => BlobState::Reserved,
            "ready" => BlobState::Ready,
            "deleting" => BlobState::Deleting,
            _ => return Err(MetadataError::Corrupt("invalid candidate state")),
        };

        let retry_at = row.get::<_, Option<i64>>(3).map_err(field_error)?;

        let retry_at = retry_at
            .map(EpochMillis::new)
            .transpose()
            .map_err(|_| MetadataError::Corrupt("invalid candidate retry time"))?;

        result.push(Candidate {
            row_id,
            blob_id,
            state,
            retry_at,
        });
    }

    Ok(result)
}

fn distribution(samples: &[Duration]) -> serde_json::Value {
    assert_ne!(samples, []);
    let mut sorted = samples.to_vec();
    sorted.sort_unstable();

    serde_json::json!({
        "samples": sorted.len(),
        "p50_ns": sorted[sorted.len() / 2].as_nanos(),
        "p95_ns": sorted[(sorted.len() * 95 / 100).min(sorted.len() - 1)].as_nanos(),
        "max_ns": sorted.last().unwrap().as_nanos(),
    })
}

fn counters(statement: &Statement<'_>) -> serde_json::Value {
    serde_json::json!({
        "fullscan_steps": statement.get_status(StatementStatus::FullscanStep),
        "sorts": statement.get_status(StatementStatus::Sort),
        "automatic_indexes": statement.get_status(StatementStatus::AutoIndex),
        "vm_steps": statement.get_status(StatementStatus::VmStep),
        "runs": statement.get_status(StatementStatus::Run),
        "statement_memory_bytes": statement.get_status(StatementStatus::MemUsed),
    })
}

fn plan(connection: &Connection, sql: &str, values: impl Params) -> rusqlite::Result<Vec<String>> {
    connection
        .prepare(&format!("EXPLAIN QUERY PLAN {sql}"))?
        .query_map(values, |row| row.get(3))?
        .collect()
}

#[test]
fn gc_pages_keep_upper_fence_and_decode_failures_visible() -> TestResult {
    let directory = TestDirectory::new()?;

    let database = seed(
        &directory,
        &(0..40)
            .map(|index| format!("p/{index}"))
            .collect::<Vec<_>>(),
    )?;

    database
        .connection
        .execute("UPDATE blobs SET gc_pending = 1 WHERE row_id % 3 = 0", [])?;

    let mut statement = database.connection.prepare(GC)?;
    let mut after = 0;
    let mut actual = Vec::new();

    loop {
        let rows = candidates(&mut statement, 0, after, 30, 4)?;

        if rows.is_empty() {
            break;
        }

        after = rows.last().unwrap().row_id;
        actual.extend(rows);
    }

    assert_eq!(
        actual.iter().map(|row| row.row_id).collect::<Vec<_>>(),
        (3..=30).step_by(3).collect::<Vec<_>>()
    );

    assert!(
        actual
            .iter()
            .all(|row| row.state == BlobState::Ready && row.retry_at.is_none())
    );

    assert_eq!(candidates(&mut statement, 1, 0, 40, 4)?, vec![]);
    assert_indexed(&statement);
    drop(statement);

    for corrupt in [
        "UPDATE blobs SET state = 'unknown' WHERE row_id = 3",
        "UPDATE blobs SET gc_retry_at_ms = -62135596800001 WHERE row_id = 3",
        "UPDATE blobs SET shard = 1 WHERE row_id = 3",
    ] {
        database
            .connection
            .execute_batch("PRAGMA ignore_check_constraints = ON; BEGIN")?;

        database.connection.execute_batch(corrupt)?;
        let mut statement = database.connection.prepare(GC)?;
        let shard = u8::from(corrupt.contains("shard = 1"));

        assert!(
            matches!(
                candidates(&mut statement, shard, 0, 40, 256),
                Err(MetadataError::Corrupt(_))
            ),
            "{corrupt}"
        );

        drop(statement);

        database
            .connection
            .execute_batch("ROLLBACK; PRAGMA ignore_check_constraints = OFF")?;
    }

    close(database)?;
    directory.finish()?;
    Ok(())
}

#[test]
#[allow(
    clippy::print_stdout,
    reason = "explicit qualification command retains structured measurements"
)]
fn bundled_query_costs_separate_prepare_execute_decode_and_full_sync_commit() -> TestResult {
    for object_count in [32, 8192] {
        let directory = TestDirectory::new()?;

        let keys: Vec<_> = (0..object_count)
            .map(|index| format!("{}/{index:08}", if index % 2 == 0 { "p" } else { "q" }))
            .collect();

        let mut database = seed(&directory, &keys)?;

        database
            .connection
            .execute("UPDATE blobs SET gc_pending = 1 WHERE row_id % 16 = 0", [])?;

        let lookup_sql = format!("{PROJECTION}{LOOKUP}");
        let page_sql = format!("{PROJECTION}{PAGE_UPPER_AFTER}");

        let plans = serde_json::json!({
            "lookup": plan(&database.connection, &lookup_sql, params![BUCKET, keys[0].as_bytes()])?,
            "page": plan(&database.connection, &page_sql, params![BUCKET, b"p/", b"p0", b"p/00000010", 64])?,
            "gc": plan(&database.connection, GC, params![0, 0, object_count, 64])?,
        });

        let mut preparation = Vec::new();
        let mut execution = Vec::new();
        let mut decoding = Vec::new();
        let mut total_reused = Vec::new();
        let mut total_prepared = Vec::new();
        let mut total_cached = Vec::new();

        for _ in 0..200 {
            let start = Instant::now();
            let statement = database.connection.prepare(&lookup_sql)?;
            preparation.push(start.elapsed());
            drop(statement);
        }

        let mut statement = database.connection.prepare(&lookup_sql)?;

        for index in 0..200 {
            let key = &keys[index * 37 % keys.len()];
            let start = Instant::now();

            {
                let mut rows = statement.query(params![BUCKET, key.as_bytes()])?;
                let row = rows.next()?.unwrap();
                execution.push(start.elapsed());
                let decode_start = Instant::now();
                let decoded = decode(row)?;
                decoding.push(decode_start.elapsed());
                assert_eq!(decoded.key.as_str(), key);
                assert!(rows.next()?.is_none());
            }

            total_reused.push(start.elapsed());
        }

        assert_indexed(&statement);
        let lookup_counters = counters(&statement);
        drop(statement);

        for index in 0..200 {
            let key = &keys[index * 37 % keys.len()];
            let start = Instant::now();

            assert_eq!(
                lookup(&database.connection, key)?.unwrap().key.as_str(),
                key
            );

            total_prepared.push(start.elapsed());
        }

        for index in 0..200 {
            let key = &keys[index * 37 % keys.len()];
            let start = Instant::now();

            {
                let mut statement = database.connection.prepare_cached(&lookup_sql)?;
                let rows = collect(&mut statement, params![BUCKET, key.as_bytes()])?;
                assert_eq!(rows.len(), 1);
                assert_eq!(rows[0].key.as_str(), key);
            }

            total_cached.push(start.elapsed());
        }

        let cached_counters = {
            let statement = database.connection.prepare_cached(&lookup_sql)?;
            assert_eq!(statement.get_status(StatementStatus::Run), 200);
            assert_indexed(&statement);
            counters(&statement)
        };

        let mut listing = Vec::new();
        let mut statement = database.connection.prepare(&page_sql)?;

        for _ in 0..100 {
            let start = Instant::now();

            let rows = collect(
                &mut statement,
                params![BUCKET, b"p/", b"p0", b"p/00000010", 64],
            )?;

            listing.push(start.elapsed());
            assert_eq!(rows.len(), if object_count == 32 { 10 } else { 64 });
        }

        assert_indexed(&statement);
        let page_counters = counters(&statement);
        drop(statement);
        let mut statement = database.connection.prepare(GC)?;
        let candidates = candidates(&mut statement, 0, 0, object_count, 64)?;

        assert_eq!(
            candidates.len(),
            usize::try_from((object_count / 16).min(64))?
        );

        assert_indexed(&statement);
        let gc_counters = counters(&statement);
        drop(statement);

        let wal_path = directory.path().join("metadata.sqlite3-wal");
        let wal_before = wal_path.metadata()?.len();
        let mut begin = Vec::new();
        let mut transition = Vec::new();
        let mut commit = Vec::new();

        for index in 0..64 {
            let start = Instant::now();

            let transaction = database
                .connection
                .transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)?;

            begin.push(start.elapsed());
            let start = Instant::now();

            assert_eq!(
                transaction.execute(
                    "UPDATE blobs SET gc_pending = ?1 WHERE row_id = 1",
                    [index % 2]
                )?,
                1
            );

            transition.push(start.elapsed());
            let start = Instant::now();
            transaction.commit()?;
            commit.push(start.elapsed());
        }

        let observation = serde_json::json!({
            "scope": "warm single-connection metadata probe; no payload, queue, cache eviction or engine throughput claim",
            "object_count": object_count,
            "runtime": database.runtime_identity()?,
            "synchronous": database.connection.query_row(
                "PRAGMA synchronous", [], |row| row.get::<_, i64>(0)
            )?,
            "plans": plans,
            "lookup": {
                "prepare": distribution(&preparation),
                "execute": distribution(&execution),
                "decode": distribution(&decoding),
                "reused_total": distribution(&total_reused),
                "prepared_total": distribution(&total_prepared),
                "cached_total": distribution(&total_cached),
                "counters": lookup_counters,
                "cached_counters": cached_counters
            },
            "page": {"limit": 64, "elapsed": distribution(&listing), "counters": page_counters},
            "gc": gc_counters,
            "transition": {
                "begin": distribution(&begin),
                "execute": distribution(&transition),
                "commit": distribution(&commit)
            },
            "wal": {"before_bytes": wal_before, "after_bytes": wal_path.metadata()?.len()},
        });

        println!("QUERY_MEASUREMENT {observation}");
        close(database)?;
        directory.finish()?;
    }

    Ok(())
}
