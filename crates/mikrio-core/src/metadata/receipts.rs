//! Guard-authorized terminal evidence pruning, without a wall-clock lifetime.

use rusqlite::{OptionalExtension, Row, params};

use super::sweep::{self, Page, Task};
use super::{MetadataDatabase, MetadataError, decode_error};
use crate::{BlobId, EpochMillis, OperationError, UploadId};

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct Receipt {
    row_id: i64,
    upload_id: String,
    claim: Option<[u8; 16]>,
    pub(crate) output: Option<BlobId>,
    digest: Option<[u8; 32]>,
    outcome: String,
    finished: i64,
}

pub(crate) type ReceiptPage = Page<Result<Receipt, OperationError>>;

// The result body is not pruning authority. Read only its required presence;
// completion result decoding belongs to the operation resolving that witness.
const PROJECTION: &str = "SELECT row_id, upload_id, completion_claim_id, completion_blob_id,
    completion_parts_digest, outcome, finished_at_ms, result_json IS NULL FROM multipart_receipts";

fn decode(row: &Row<'_>) -> Result<Receipt, OperationError> {
    let row_id: i64 = row.get(0).map_err(decode_error)?;
    let upload_id: String = row.get(1).map_err(decode_error)?;

    UploadId::decode(&upload_id)
        .map_err(|_| MetadataError::Corrupt("invalid terminal upload identity"))?;

    let claim = identity(row.get(2).map_err(decode_error)?)?;
    let output = identity(row.get(3).map_err(decode_error)?)?.map(BlobId::from_bytes);
    let digest = identity(row.get(4).map_err(decode_error)?)?;
    let outcome: String = row.get(5).map_err(decode_error)?;
    let finished = row.get(6).map_err(decode_error)?;
    let no_result: bool = row.get(7).map_err(decode_error)?;
    EpochMillis::new(finished).map_err(|_| MetadataError::Corrupt("invalid receipt timestamp"))?;

    let valid = match outcome.as_str() {
        "completed" => output.is_some() && !no_result,
        "aborted" => no_result,
        _ => false,
    };

    if row_id <= 0
        || !valid
        || claim.is_some() != output.is_some()
        || digest.is_some() != output.is_some()
    {
        return Err(MetadataError::Corrupt("invalid completion receipt").into());
    }

    Ok(Receipt {
        row_id,
        upload_id,
        claim,
        output,
        digest,
        outcome,
        finished,
    })
}

fn identity<const N: usize>(value: Option<Vec<u8>>) -> Result<Option<[u8; N]>, MetadataError> {
    value
        .map(|value| {
            value
                .try_into()
                .map_err(|_| MetadataError::Corrupt("invalid receipt identity"))
        })
        .transpose()
}

impl MetadataDatabase {
    pub(crate) fn receipt_page(&mut self) -> Result<Option<ReceiptPage>, OperationError> {
        if !self.read(|transaction| {
            Ok(transaction
                .prepare_cached("SELECT 1 FROM multipart_receipts LIMIT 1")?
                .exists([])?)
        })? {
            return Ok(None);
        }

        let reset = sweep::Reset::prepare()?;

        #[cfg(test)]
        let _protocol = crate::protocol_fault::Scope::enter("receipt.page");

        self.read(|transaction| {
            let mut progress = sweep::begin(transaction, Task::Receipts, &reset)?;

            let mut statement = transaction.prepare_cached(&format!(
                "{PROJECTION} WHERE row_id > ?1 AND row_id <= ?2 ORDER BY row_id LIMIT 16"
            ))?;

            let mut rows = statement.query((progress.cursor, progress.upper))?;
            let mut candidates = Vec::new();

            while let Some(row) = rows.next()? {
                progress.examined = row.get(0).map_err(decode_error)?;
                candidates.push((progress.examined, decode(row)));
            }

            if progress.examined == progress.cursor {
                progress.examined = progress.upper;
            }

            Ok(Some(Page {
                progress,
                candidates,
            }))
        })
    }

    /// The caller retains exclusive output-shard protection when output exists.
    pub(crate) fn prune_receipt(&mut self, selected: &Receipt) -> Result<(), OperationError> {
        #[cfg(test)]
        let _protocol = crate::protocol_fault::Scope::enter("receipt.prune");

        self.write(|transaction| {
            let mut statement =
                transaction.prepare_cached(&format!("{PROJECTION} WHERE row_id = ?1"))?;

            let mut rows = statement.query([selected.row_id])?;

            let Some(row) = rows.next()? else {
                return Ok(());
            };

            let current = decode(row)?;
            drop(rows);
            drop(statement);

            if &current != selected {
                return Err(MetadataError::Corrupt("terminal receipt identity changed").into());
            }

            let active: Option<i64> = transaction
                .query_row(
                    "SELECT 1 FROM multipart_uploads WHERE upload_id = ?1",
                    [&selected.upload_id],
                    |row| row.get(0),
                )
                .optional()?;

            if active.is_some() {
                return Err(MetadataError::Corrupt("terminal receipt has an active upload").into());
            }

            if transaction.execute(
                "DELETE FROM multipart_receipts WHERE row_id = ?1 AND upload_id = ?2
                AND completion_claim_id IS ?3 AND completion_blob_id IS ?4",
                params![
                    selected.row_id,
                    selected.upload_id,
                    selected.claim.as_ref().map(<[u8; 16]>::as_slice),
                    selected
                        .output
                        .as_ref()
                        .map(|blob| blob.as_bytes().as_slice())
                ],
            )? != 1
            {
                return Err(MetadataError::Corrupt("receipt pruning fence failed").into());
            }

            Ok(())
        })
    }
}
