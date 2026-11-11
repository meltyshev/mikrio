//! Bounded scan positions are generation-fenced hints, never cleanup authority.

use rusqlite::{Transaction, params};

use super::{MetadataDatabase, MetadataError, decode_error};
use crate::{EpochMillis, OperationError, StoreId};

#[derive(Clone, Copy, Debug)]
pub(crate) enum Task {
    Blobs(u8),
    ExpireObjects,
    AbortUploads,
    Receipts,
}

impl Task {
    fn identity(self) -> (&'static str, i64) {
        match self {
            Self::Blobs(shard) => ("gc_blobs", i64::from(shard)),
            Self::ExpireObjects => ("expire_objects", -1),
            Self::AbortUploads => ("abort_uploads", -1),
            Self::Receipts => ("purge_receipts", -1),
        }
    }
}

#[derive(Clone, Copy, Debug)]
struct Position {
    generation: [u8; 16],
    cursor: i64,
    upper: i64,
}

#[derive(Clone, Copy, Debug)]
pub(crate) struct Progress {
    task: Task,
    expected: Position,
    reset: Option<Reset>,
    pub(crate) cursor: i64,
    pub(super) upper: i64,
    pub(crate) examined: i64,
}

#[derive(Debug)]
pub(crate) struct Page<T> {
    pub(crate) progress: Progress,
    pub(crate) candidates: Vec<(i64, T)>,
}

#[derive(Clone, Copy, Debug)]
pub(super) struct Reset {
    generation: StoreId,
    started_at: EpochMillis,
}

impl Reset {
    pub(super) fn prepare() -> Result<Self, OperationError> {
        Ok(Self {
            generation: StoreId::generate().map_err(MetadataError::Entropy)?,
            started_at: super::now()?,
        })
    }
}

pub(super) fn begin(
    transaction: &Transaction<'_>,
    task: Task,
    reset: &Reset,
) -> Result<Progress, OperationError> {
    let (name, shard) = task.identity();

    let (generation, cursor, upper): (Vec<u8>, i64, i64) = transaction
        .query_row(
            "SELECT generation, cursor_row_id, upper_row_id FROM maintenance_sweeps
             WHERE task = ?1 AND shard = ?2",
            (name, shard),
            |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
        )
        .map_err(decode_error)?;

    let generation: [u8; 16] = generation
        .try_into()
        .map_err(|_| MetadataError::Corrupt("invalid sweep generation"))?;

    if cursor < 0 || upper < cursor {
        return Err(MetadataError::Corrupt("invalid sweep bounds").into());
    }

    let mut progress = Progress {
        task,
        expected: Position {
            generation,
            cursor,
            upper,
        },
        reset: None,
        cursor,
        upper,
        examined: cursor,
    };

    if cursor < upper {
        return Ok(progress);
    }

    let upper: i64 = match task {
        Task::Blobs(shard) => transaction.query_row(
            "SELECT COALESCE(MAX(row_id), 0) FROM blobs WHERE gc_pending = 1 AND shard = ?1",
            [i64::from(shard)],
            |row| row.get(0),
        )?,
        Task::Receipts => transaction.query_row(
            "SELECT COALESCE(MAX(row_id), 0) FROM multipart_receipts",
            [],
            |row| row.get(0),
        )?,
        Task::ExpireObjects => {
            transaction.query_row("SELECT COALESCE(MAX(row_id), 0) FROM objects", [], |row| {
                row.get(0)
            })?
        }
        Task::AbortUploads => transaction.query_row(
            "SELECT COALESCE(MAX(row_id), 0) FROM multipart_uploads",
            [],
            |row| row.get(0),
        )?,
    };

    // Capture bounds in the page's read snapshot. Persist this proposed reset
    // together with advancement, fenced by the state that snapshot observed.
    // Interruption before then only repeats bounded, revalidated work.
    progress.reset = Some(*reset);
    progress.cursor = 0;
    progress.upper = upper;
    progress.examined = 0;

    Ok(progress)
}

impl MetadataDatabase {
    pub(crate) fn advance_sweep(&mut self, progress: Progress) -> Result<(), OperationError> {
        let (name, shard) = progress.task.identity();

        let generation = progress
            .reset
            .map_or(progress.expected.generation, |reset| {
                *reset.generation.as_bytes()
            });

        #[cfg(test)]
        let _protocol = crate::protocol_fault::Scope::enter("sweep.advance");

        self.write(|transaction| {
            // Another executor's newer cursor/generation wins. A stale advance
            // is deliberately a no-op; the page cannot authorize any deletion.
            transaction.execute(
                "UPDATE maintenance_sweeps SET cursor_row_id = ?3, upper_row_id = ?4,
                 generation = ?5, started_at_ms = COALESCE(?6, started_at_ms)
                 WHERE task = ?1 AND shard = ?2 AND generation = ?7
                 AND cursor_row_id = ?8 AND upper_row_id = ?9",
                params![
                    name,
                    shard,
                    progress.examined,
                    progress.upper,
                    generation.as_slice(),
                    progress.reset.map(|reset| reset.started_at.get()),
                    progress.expected.generation.as_slice(),
                    progress.expected.cursor,
                    progress.expected.upper
                ],
            )?;

            Ok(())
        })
    }
}
