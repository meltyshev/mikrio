//! Bounded observations at actual BEGIN/work/COMMIT effects in one owned child.

use std::collections::HashMap;
use std::io;
use std::path::Path;
use std::sync::{Mutex, OnceLock};
use std::thread::ThreadId;
use std::time::Instant;

use serde::Serialize;

pub(super) fn storage(path: &Path) -> Result<serde_json::Value, Box<dyn std::error::Error>> {
    let sql = rusqlite::Connection::open_with_flags(
        path.join("metadata.sqlite3"),
        rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY,
    )?;

    let page_count: i64 = sql.pragma_query_value(None, "page_count", |row| row.get(0))?;
    let page_size: i64 = sql.pragma_query_value(None, "page_size", |row| row.get(0))?;
    let free_pages: i64 = sql.pragma_query_value(None, "freelist_count", |row| row.get(0))?;

    let mut statement = sql.prepare(
        "SELECT state, gc_pending, count(*), coalesce(sum(byte_length), 0) FROM blobs GROUP BY state, gc_pending"
    )?;

    let ledger = statement
        .query_map([], |row| {
            Ok(serde_json::json!({
                "state": row.get::<_, String>(0)?,
                "gc_pending": row.get::<_, i64>(1)?,
                "count": row.get::<_, i64>(2)?,
                "payload_bytes": row.get::<_, i64>(3)?,
            }))
        })?
        .collect::<rusqlite::Result<Vec<_>>>()?;

    let mut payload = serde_json::Map::new();

    for tree in ["staging", "objects"] {
        let mut files = 0_u64;
        let mut bytes = 0_u64;

        for shard in std::fs::read_dir(path.join(tree))? {
            for entry in std::fs::read_dir(shard?.path())? {
                let metadata = entry?.metadata()?;
                assert!(metadata.is_file());
                files += 1;

                bytes = bytes
                    .checked_add(metadata.len())
                    .ok_or("payload inventory overflow")?;
            }
        }

        payload.insert(
            tree.into(),
            serde_json::json!({"files": files, "bytes": bytes}),
        );
    }

    let wal_bytes = match std::fs::metadata(path.join("metadata.sqlite3-wal")) {
        Ok(metadata) => metadata.len(),
        Err(error) if error.kind() == io::ErrorKind::NotFound => 0,
        Err(error) => return Err(error.into()),
    };

    Ok(serde_json::json!({
        "page_count": page_count, "page_size": page_size, "free_pages": free_pages,
        "wal_file_bytes": wal_bytes, "ledger": ledger, "payload": payload,
    }))
}

static SAMPLES: OnceLock<Mutex<Samples>> = OnceLock::new();

struct Transaction {
    started: Instant,
    begun: Option<Instant>,
    committing: Option<Instant>,
}

struct Samples {
    active: HashMap<ThreadId, Transaction>,
    completed: Vec<Timing>,
}

#[derive(Serialize)]
#[allow(
    clippy::struct_field_names,
    reason = "serialized primitive quantities carry explicit time units"
)]
pub(super) struct Timing {
    begin_wait_ns: u128,
    transaction_ns: u128,
    commit_ns: u128,
}

pub(super) fn install() -> io::Result<()> {
    SAMPLES
        .set(Mutex::new(Samples {
            active: HashMap::with_capacity(32),
            completed: Vec::with_capacity(8192),
        }))
        .map_err(|_| io::Error::other("SQL measurement already installed"))?;

    crate::protocol_fault::install(checkpoint)
}

pub(super) fn finish() -> io::Result<Vec<Timing>> {
    let mut samples = SAMPLES
        .get()
        .ok_or_else(|| io::Error::other("missing SQL measurement"))?
        .lock()
        .map_err(|_| io::Error::other("poisoned SQL measurement"))?;

    if !samples.active.is_empty() {
        return Err(io::Error::other("unfinished SQL timing interval"));
    }

    Ok(std::mem::take(&mut samples.completed))
}

fn checkpoint(point: &str) -> io::Result<()> {
    let Some(phase) = transaction_phase(point) else {
        return Ok(());
    };

    let mut samples = SAMPLES
        .get()
        .ok_or_else(|| io::Error::other("missing SQL measurement"))?
        .lock()
        .map_err(|_| io::Error::other("poisoned SQL measurement"))?;

    let thread = std::thread::current().id();

    if phase == "begin.before" {
        if samples.active.len() >= 32 || samples.active.contains_key(&thread) {
            return Err(io::Error::other("SQL measurement admission exceeded"));
        }

        samples.active.insert(
            thread,
            Transaction {
                started: Instant::now(),
                begun: None,
                committing: None,
            },
        );

        return Ok(());
    }

    let transaction = samples
        .active
        .get_mut(&thread)
        .ok_or_else(|| io::Error::other("SQL measurement lacks BEGIN"))?;

    match phase {
        "begun" => transaction.begun = Some(Instant::now()),
        "commit.before" => transaction.committing = Some(Instant::now()),
        "commit.after" => {
            let now = Instant::now();

            let begun = transaction
                .begun
                .ok_or_else(|| io::Error::other("missing BEGIN result"))?;

            let commit = transaction
                .committing
                .ok_or_else(|| io::Error::other("missing COMMIT entry"))?;

            let observation = Timing {
                begin_wait_ns: begun.duration_since(transaction.started).as_nanos(),
                transaction_ns: now.duration_since(transaction.started).as_nanos(),
                commit_ns: now.duration_since(commit).as_nanos(),
            };

            if samples.completed.len() >= 8192 {
                return Err(io::Error::other("SQL measurement sample bound exceeded"));
            }

            samples.completed.push(observation);
            samples.active.remove(&thread);
        }
        _ => return Err(io::Error::other("unknown SQL measurement phase")),
    }

    Ok(())
}

fn transaction_phase(point: &str) -> Option<&'static str> {
    ["begin.before", "begun", "commit.before", "commit.after"]
        .into_iter()
        .find(|phase| {
            point
                .strip_suffix(phase)
                .is_some_and(|protocol| protocol.len() > 1 && protocol.ends_with('.'))
        })
}

#[test]
fn transaction_phases_preserve_dotted_protocol_names() {
    for protocol in [
        "payload",
        "commit_fence",
        "principal.policy.put",
        "bucket.public_access_block.put",
    ] {
        for phase in ["begin.before", "begun", "commit.before", "commit.after"] {
            assert_eq!(
                transaction_phase(&format!("{protocol}.{phase}")),
                Some(phase)
            );
        }
    }

    for other in [
        "begun",
        ".begun",
        "payload.begun.extra",
        "payload.committed",
        "bucket.policy.put",
    ] {
        assert_eq!(transaction_phase(other), None);
    }
}
