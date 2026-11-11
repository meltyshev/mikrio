//! Ledger-driven reclamation; only the caller's exclusive shard authorizes it.

use rusqlite::{OptionalExtension, Row, params};

use super::sweep::{self, Page, Task};
use super::{MetadataDatabase, MetadataError, decode_error};
use crate::{BlobId, OperationError};

pub(crate) type GcPage = Page<Result<BlobId, OperationError>>;

/// Read-only diagnostic page. These rows provide no reclamation authority.
pub(crate) struct CleanupPage {
    pub(crate) blobs: Vec<BlobId>,
    pub(crate) oldest_created_at_ms: Option<i64>,
    pub(crate) after: Option<(u8, i64)>,
    pub(crate) more_pending: bool,
}

impl MetadataDatabase {
    pub(crate) fn cleanup_sample(
        &mut self,
        after: Option<(u8, i64)>,
    ) -> Result<CleanupPage, OperationError> {
        self.read(|transaction| {
            let mut statement = transaction.prepare_cached(
                "SELECT shard, row_id, blob_id, created_at_ms FROM blobs WHERE gc_pending = 1
                 AND (shard, row_id) > (?1, ?2) ORDER BY shard, row_id LIMIT 17",
            )?;

            let (shard, row_id) =
                after.map_or((-1, 0), |(shard, row_id)| (i64::from(shard), row_id));

            let mut rows = statement.query((shard, row_id))?;

            let mut page = CleanupPage {
                blobs: Vec::with_capacity(16),
                oldest_created_at_ms: None,
                after: None,
                more_pending: false,
            };

            while let Some(row) = rows.next()? {
                if page.blobs.len() == 16 {
                    page.more_pending = true;
                    break;
                }

                let shard: i64 = row.get(0).map_err(decode_error)?;
                let row_id: i64 = row.get(1).map_err(decode_error)?;
                let bytes: Vec<u8> = row.get(2).map_err(decode_error)?;

                let blob_id = BlobId::from_bytes(
                    bytes
                        .try_into()
                        .map_err(|_| MetadataError::Corrupt("invalid sampled blob identity"))?,
                );

                if shard != i64::from(blob_id.shard()) || row_id <= 0 {
                    return Err(MetadataError::Corrupt("invalid sampled blob position").into());
                }

                let created = row.get(3).map_err(decode_error)?;

                crate::EpochMillis::new(created)
                    .map_err(|_| MetadataError::Corrupt("invalid sampled blob timestamp"))?;

                page.oldest_created_at_ms = Some(
                    page.oldest_created_at_ms
                        .map_or(created, |oldest| oldest.min(created)),
                );

                page.blobs.push(blob_id);
                page.after = Some((blob_id.shard(), row_id));
            }

            if !page.more_pending {
                page.after = None;
            }

            Ok(page)
        })
    }

    pub(crate) fn gc_next_shard(&mut self, start: u8) -> Result<Option<u8>, OperationError> {
        self.read(|transaction| {
            let mut statement = transaction.prepare_cached(
                "SELECT shard FROM blobs WHERE gc_pending = 1 AND shard >= ?1
                 ORDER BY shard LIMIT 1",
            )?;

            let mut shard: Option<i64> = statement
                .query_row([i64::from(start)], |row| row.get(0))
                .optional()
                .map_err(decode_error)?;

            if shard.is_none() && start != 0 {
                shard = statement
                    .query_row([0], |row| row.get(0))
                    .optional()
                    .map_err(decode_error)?;
            }

            shard
                .map(|shard| {
                    u8::try_from(shard)
                        .map_err(|_| MetadataError::Corrupt("invalid pending GC shard").into())
                })
                .transpose()
        })
    }

    pub(crate) fn gc_page(&mut self, shard: u8) -> Result<Option<GcPage>, OperationError> {
        let has_work = self.read(|transaction| {
            Ok(transaction
                .prepare_cached("SELECT 1 FROM blobs WHERE gc_pending = 1 AND shard = ?1 LIMIT 1")?
                .exists([i64::from(shard)])?)
        })?;

        if !has_work {
            return Ok(None);
        }

        let now = super::now()?.get();

        let reset = sweep::Reset::prepare()?;

        #[cfg(test)]
        let _protocol = crate::protocol_fault::Scope::enter("gc.page");

        self.read(|transaction| {
            let mut progress = sweep::begin(transaction, Task::Blobs(shard), &reset)?;

            let mut statement = transaction.prepare_cached(
                "SELECT row_id, blob_id, gc_retry_at_ms FROM blobs WHERE gc_pending = 1 AND shard = ?1
                 AND row_id > ?2 AND row_id <= ?3 ORDER BY row_id LIMIT 16",
            )?;

            let mut rows = statement.query(params![
                i64::from(shard),
                progress.cursor,
                progress.upper
            ])?;

            let mut candidates = Vec::new();

            while let Some(row) = rows.next()? {
                progress.examined = row.get(0).map_err(decode_error)?;

                // Decode failures are candidate-local. Keep their positions so
                // the executor reports them and advances past them this sweep.
                if let Some(candidate) = decode_candidate(row, shard, now).transpose() {
                    candidates.push((progress.examined, candidate));
                }
            }

            drop(rows);
            drop(statement);

            if progress.examined == progress.cursor {
                progress.examined = progress.upper;
            }

            Ok(Some(Page {
                progress,
                candidates,
            }))
        })
    }

    pub(crate) fn gc_mark(&mut self, blob_id: BlobId) -> Result<bool, OperationError> {
        #[cfg(test)]
        let _protocol = crate::protocol_fault::Scope::enter("gc.mark");

        self.write(|transaction| {
            let row: Option<(String, i64)> = transaction
                .query_row(
                    "SELECT state, shard FROM blobs WHERE blob_id = ?1",
                    [blob_id.as_bytes().as_slice()],
                    |row| Ok((row.get(0)?, row.get(1)?)),
                )
                .optional()
                .map_err(decode_error)?;

            let Some((state, shard)) = row else {
                return Ok(false);
            };

            if shard != i64::from(blob_id.shard())
                || !matches!(state.as_str(), "reserved" | "ready" | "deleting")
            {
                return Err(MetadataError::Corrupt("invalid GC ledger state").into());
            }

            let referenced = transaction
                .prepare_cached(
                    "SELECT 1 FROM objects WHERE blob_id = ?1 UNION ALL
                     SELECT 1 FROM multipart_parts WHERE blob_id = ?1 LIMIT 1",
                )?
                .exists([blob_id.as_bytes().as_slice()])?;

            if referenced {
                if state != "ready" {
                    return Err(MetadataError::Corrupt("non-ready referenced GC blob").into());
                }

                transaction.execute(
                    "UPDATE blobs SET gc_pending = 0, gc_retry_at_ms = NULL, gc_attempt_count = 0,
                     gc_error_code = NULL WHERE blob_id = ?1",
                    [blob_id.as_bytes().as_slice()],
                )?;

                return Ok(false);
            }

            let claim: Option<(String, String)> = transaction
                .query_row(
                    "SELECT upload_id, state FROM multipart_uploads WHERE completion_blob_id = ?1",
                    [blob_id.as_bytes().as_slice()],
                    |row| Ok((row.get(0)?, row.get(1)?)),
                )
                .optional()
                .map_err(decode_error)?;

            if let Some((upload_id, upload_state)) = claim {
                if state != "reserved" || upload_state != "completing" {
                    return Err(MetadataError::Corrupt("invalid abandoned completion state").into());
                }

                let valid_claim = transaction
                    .prepare_cached(
                        "SELECT 1 FROM multipart_uploads AS u JOIN blobs AS b ON b.blob_id = u.completion_blob_id
                         WHERE u.upload_id = ?1 AND b.write_kind = 'complete_multipart_upload'
                         AND b.target_bucket_id = u.bucket_id AND b.target_key = u.key
                         AND b.target_upload_id = u.upload_id AND b.target_part_number IS NULL",
                    )?
                    .exists([&upload_id])?;

                if !valid_claim {
                    return Err(MetadataError::Corrupt("completion claim target mismatch").into());
                }

                let changed = transaction.execute(
                    "UPDATE multipart_uploads SET state = 'open', completion_claim_id = NULL,
                     completion_blob_id = NULL, completion_parts_digest = NULL
                     WHERE upload_id = ?1 AND completion_blob_id = ?2 AND state = 'completing'",
                    params![upload_id, blob_id.as_bytes().as_slice()],
                )?;

                if changed != 1 {
                    return Err(MetadataError::Corrupt("abandoned claim fence failed").into());
                }
            }

            transaction.execute(
                "UPDATE blobs SET state = 'deleting', gc_pending = 1 WHERE blob_id = ?1",
                [blob_id.as_bytes().as_slice()],
            )?;

            Ok(true)
        })
    }

    pub(crate) fn gc_finish(&mut self, blob_id: BlobId) -> Result<(), OperationError> {
        #[cfg(test)]
        let _protocol = crate::protocol_fault::Scope::enter("gc.finish");

        self.write(|transaction| {
            let state: Option<String> = transaction
                .query_row(
                    "SELECT state FROM blobs WHERE blob_id = ?1",
                    [blob_id.as_bytes().as_slice()],
                    |row| row.get(0),
                )
                .optional()
                .map_err(decode_error)?;

            let Some(state) = state else {
                return Ok(());
            };

            if state != "deleting" {
                return Err(MetadataError::Corrupt("GC finalization requires deleting state").into());
            }

            let referenced = transaction
                .prepare_cached(
                    "SELECT 1 FROM objects WHERE blob_id = ?1 UNION ALL
                     SELECT 1 FROM multipart_parts WHERE blob_id = ?1 UNION ALL
                     SELECT 1 FROM multipart_uploads WHERE completion_blob_id = ?1 LIMIT 1",
                )?
                .exists([blob_id.as_bytes().as_slice()])?;

            if referenced {
                return Err(MetadataError::Corrupt("GC finalization found a live reference").into());
            }

            transaction.execute(
                "DELETE FROM blob_parts WHERE blob_id = ?1 AND part_number IN
                 (SELECT part_number FROM blob_parts WHERE blob_id = ?1 ORDER BY part_number LIMIT 256)",
                [blob_id.as_bytes().as_slice()],
            )?;

            let has_parts = transaction
                .prepare_cached("SELECT 1 FROM blob_parts WHERE blob_id = ?1 LIMIT 1")?
                .exists([blob_id.as_bytes().as_slice()])?;

            if has_parts {
                return Ok(());
            }

            transaction.execute(
                "DELETE FROM blobs WHERE blob_id = ?1 AND state = 'deleting'",
                [blob_id.as_bytes().as_slice()],
            )?;

            Ok(())
        })
    }

    pub(crate) fn gc_failed(&mut self, blob_id: BlobId) -> Result<(), OperationError> {
        let now = super::now()?;

        #[cfg(test)]
        let _protocol = crate::protocol_fault::Scope::enter("gc.retry");

        self.write(|transaction| {
            let count: Option<i64> = transaction
                .query_row(
                    "SELECT gc_attempt_count FROM blobs WHERE blob_id = ?1 AND gc_pending = 1",
                    [blob_id.as_bytes().as_slice()],
                    |row| row.get(0),
                )
                .optional()
                .map_err(decode_error)?;

            let Some(count) = count else {
                return Ok(());
            };

            if count < 0 {
                return Err(MetadataError::Corrupt("negative GC retry count").into());
            }

            let shift = u32::try_from(count.min(5))
                .map_err(|_| MetadataError::Corrupt("invalid GC retry count"))?;

            let retry = now.get().saturating_add(1000_i64 << shift);

            transaction.execute(
                "UPDATE blobs SET gc_attempt_count = ?2, gc_retry_at_ms = ?3, gc_error_code = 'CleanupFailed'
                 WHERE blob_id = ?1 AND gc_pending = 1",
                params![blob_id.as_bytes().as_slice(), count.saturating_add(1), retry],
            )?;

            Ok(())
        })
    }
}

fn decode_candidate(row: &Row<'_>, shard: u8, now: i64) -> Result<Option<BlobId>, OperationError> {
    let bytes: Vec<u8> = row.get(1).map_err(decode_error)?;

    let blob_id = BlobId::from_bytes(
        bytes
            .try_into()
            .map_err(|_| MetadataError::Corrupt("invalid GC blob identity"))?,
    );

    if blob_id.shard() != shard {
        return Err(MetadataError::Corrupt("GC shard mismatch").into());
    }

    let retry: Option<i64> = row.get(2).map_err(decode_error)?;

    // Wall time is only a hint. A backward jump beyond the capped
    // delay cannot suppress work indefinitely; each sweep is paced.
    let due = retry.is_none_or(|retry| retry <= now || retry > now.saturating_add(32_000));
    Ok(due.then_some(blob_id))
}
