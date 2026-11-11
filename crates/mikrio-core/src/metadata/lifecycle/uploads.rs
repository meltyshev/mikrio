//! Bounded upload hints are revalidated with current rules in the retirement transaction.

use rusqlite::{Row, Transaction};

use super::configuration;
use crate::lifecycle::{LifecycleConfiguration, abort_at};
use crate::metadata::sweep::{self, Page, Task};
use crate::metadata::{MetadataDatabase, MetadataError, decode_error, multipart};
use crate::object::ObjectKey;
use crate::{EpochMillis, OperationError, OperationFailure, UploadId};

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct Candidate {
    row_id: i64,
    bucket_id: [u8; 16],
    key: ObjectKey,
    upload_id: UploadId,
    rule_generation: [u8; 16],
    anchor: EpochMillis,
}

pub(crate) type UploadPage = Page<Result<Option<Candidate>, OperationError>>;

const PROJECTION: &str = "SELECT u.row_id, u.bucket_id, u.key, u.upload_id,
    u.initiated_at_ms, b.lifecycle_generation, b.name IS NOT NULL,
    b.bucket_id IS NOT NULL FROM multipart_uploads AS u
    LEFT JOIN buckets AS b ON b.bucket_id = u.bucket_id";

const HAS_RULES: &str = "SELECT 1 FROM bucket_lifecycle_rules AS r JOIN buckets AS b
    ON b.bucket_id = r.bucket_id WHERE r.is_enabled = 1
    AND r.abort_incomplete_multipart_days IS NOT NULL AND b.name IS NOT NULL
    AND b.lifecycle_generation IS NOT NULL LIMIT 1";
const PAGE_BOUNDS: &str = " WHERE u.row_id > ?1 AND u.row_id <= ?2 ORDER BY u.row_id LIMIT 16";

impl MetadataDatabase {
    pub(crate) fn upload_expiration_page(
        &mut self,
        now: EpochMillis,
    ) -> Result<Option<UploadPage>, OperationError> {
        if !self.read(|transaction| Ok(transaction.prepare_cached(HAS_RULES)?.exists([])?))? {
            return Ok(None);
        }

        let reset = sweep::Reset::prepare()?;

        #[cfg(test)]
        let _protocol = crate::protocol_fault::Scope::enter("lifecycle.uploads.page");

        self.read(|transaction| {
            let mut progress = sweep::begin(transaction, Task::AbortUploads, &reset)?;

            let mut statement =
                transaction.prepare_cached(&format!("{PROJECTION}{PAGE_BOUNDS}"))?;

            let mut rows = statement.query((progress.cursor, progress.upper))?;
            let mut candidates = Vec::new();

            while let Some(row) = rows.next()? {
                progress.examined = row.get(0).map_err(decode_error)?;
                candidates.push((progress.examined, decode(row)));
            }

            drop(rows);
            drop(statement);

            if progress.examined == progress.cursor {
                progress.examined = progress.upper;
            }

            let mut page = Page {
                progress,
                candidates,
            };

            super::filter_candidates(
                transaction,
                &mut page,
                |selected| selected.bucket_id,
                |rules, selected| eligible(rules, selected, now),
            );

            Ok(Some(page))
        })
    }

    pub(crate) fn expire_upload(
        &mut self,
        selected: &Candidate,
        now: EpochMillis,
    ) -> Result<(), OperationError> {
        #[cfg(test)]
        let _protocol = crate::protocol_fault::Scope::enter("lifecycle.upload.expire");

        self.write(|transaction| {
            let Some(current) = current(transaction, selected.row_id)? else {
                return Ok(());
            };

            if &current != selected {
                return Ok(());
            }

            let Some(rules) = configuration(transaction, &current.bucket_id)? else {
                return Ok(());
            };

            if !eligible(&rules, &current, now)? {
                return Ok(());
            }

            // Reuse the public abort/namespace transition in this same write
            // transaction. Completion-first removes this candidate; abort-first
            // retires the claim and fences all late part/completion publication.
            multipart::abort::retire_upload(
                transaction,
                &current.upload_id.to_string(),
                current.bucket_id,
                now,
            )
        })
    }
}

fn current(
    transaction: &Transaction<'_>,
    row_id: i64,
) -> Result<Option<Candidate>, OperationError> {
    let mut statement = transaction.prepare_cached(&format!("{PROJECTION} WHERE u.row_id = ?1"))?;
    let mut rows = statement.query([row_id])?;

    rows.next()?.map_or(Ok(None), decode)
}

fn decode(row: &Row<'_>) -> Result<Option<Candidate>, OperationError> {
    let bucket_exists: bool = row.get(7).map_err(decode_error)?;

    if !bucket_exists {
        return Err(corrupt());
    }

    let live: bool = row.get(6).map_err(decode_error)?;
    let rule_generation: Option<Vec<u8>> = row.get(5).map_err(decode_error)?;

    if !live {
        return Ok(None);
    }

    let Some(rule_generation) = rule_generation else {
        return Ok(None);
    };

    let key: Vec<u8> = row.get(2).map_err(decode_error)?;
    let bucket_id: Vec<u8> = row.get(1).map_err(decode_error)?;
    let encoded: String = row.get(3).map_err(decode_error)?;
    let anchor = EpochMillis::new(row.get(4).map_err(decode_error)?).map_err(|_| corrupt())?;
    let upload_id = UploadId::parse(&encoded, anchor).map_err(|_| corrupt())?;

    Ok(Some(Candidate {
        row_id: row.get(0).map_err(decode_error)?,
        bucket_id: bucket_id.try_into().map_err(|_| corrupt())?,
        key: ObjectKey::from_bytes(&key).map_err(|_| corrupt())?,
        upload_id,
        rule_generation: rule_generation.try_into().map_err(|_| corrupt())?,
        anchor,
    }))
}

fn eligible(
    rules: &LifecycleConfiguration,
    selected: &Candidate,
    now: EpochMillis,
) -> Result<bool, OperationError> {
    let mut earliest: Option<EpochMillis> = None;

    for rule in rules.rules() {
        if !rule.is_enabled {
            continue;
        }

        if !selected
            .key
            .as_bytes()
            .starts_with(rule.filter.prefix().as_bytes())
        {
            continue;
        }

        let Some(days) = rule.abort_incomplete_multipart_days else {
            continue;
        };

        let deadline = abort_at(selected.anchor, days)
            .map_err(|error| OperationError::caused(OperationFailure::InvalidArgument, error))?;

        earliest = Some(earliest.map_or(deadline, |current| current.min(deadline)));
    }

    Ok(earliest.is_some_and(|deadline| now >= deadline))
}

fn corrupt() -> OperationError {
    MetadataError::Corrupt("invalid lifecycle upload candidate").into()
}

#[cfg(test)]
pub(in crate::metadata) fn indexed_work(
    connection: &rusqlite::Connection,
) -> rusqlite::Result<i32> {
    use rusqlite::StatementStatus;

    let mut steps = 0;

    for sql in [HAS_RULES.to_owned(), format!("{PROJECTION}{PAGE_BOUNDS}")] {
        let statement = connection.prepare_cached(&sql)?;
        assert_eq!(statement.get_status(StatementStatus::FullscanStep), 0);
        assert_eq!(statement.get_status(StatementStatus::Sort), 0);
        assert_eq!(statement.get_status(StatementStatus::AutoIndex), 0);
        steps += statement.get_status(StatementStatus::VmStep);
    }

    Ok(steps)
}
