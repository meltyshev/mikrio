//! Fenced claim creation and atomic final publication with a protected receipt witness.

use std::collections::BTreeMap;

use rusqlite::{OptionalExtension, Transaction, params};
use serde::{Deserialize, Serialize};

use crate::integrity::{ChecksumType, ObjectChecksum, PayloadFacts};
use crate::metadata::{MetadataDatabase, MetadataError, decode_error, now, objects};
use crate::multipart::{CompleteRequest, CompletedUpload, CompletionClaim, CompletionSelection};
use crate::object::{Authority, ByteLength};
use crate::{MutationOutcome, OperationError, OperationFailure, StoreId};

mod selection;

#[derive(Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
struct ReceiptResult {
    object_generation: String,
    byte_length: u64,
    etag: String,
    checksum_type: String,
    checksums: BTreeMap<String, String>,
}

impl MetadataDatabase {
    pub(crate) fn select_completion(
        &mut self,
        authority: &Authority,
        request: &CompleteRequest,
    ) -> Result<CompletionSelection, OperationError> {
        self.read(|transaction| selection::select(transaction, authority, request))
    }

    /// All unique input/output shards are already held, with no earlier snapshot open.
    pub(crate) fn claim_completion(
        &mut self,
        authority: &Authority,
        request: &CompleteRequest,
        selected: &CompletionSelection,
        claim: CompletionClaim,
    ) -> Result<bool, OperationError> {
        let now = now()?;

        #[cfg(test)]
        let _protocol = crate::protocol_fault::Scope::enter("multipart.claim");

        self.write(|transaction| {
            let current = selection::select(transaction, authority, request)?;

            if &current != selected {
                return Err(OperationError::new(OperationFailure::InvalidPart));
            }

            if transaction
                .prepare_cached("SELECT 1 FROM blobs WHERE blob_id = ?1")?
                .exists([claim.blob_id.as_bytes().as_slice()])?
            {
                return Ok(false);
            }

            transaction.execute(
                "INSERT INTO blobs (blob_id, shard, state, write_kind, target_bucket_id,
                 target_key, target_upload_id, created_at_ms, gc_pending, gc_attempt_count)
                 VALUES (?1, ?2, 'reserved', 'complete_multipart_upload', ?3, ?4, ?5, ?6, 1, 0)",
                params![
                    claim.blob_id.as_bytes().as_slice(),
                    i64::from(claim.blob_id.shard()),
                    selected.bucket_id.as_slice(),
                    request.upload.key.as_bytes(),
                    request.upload.upload_id.to_string(),
                    now.get()
                ],
            )?;

            let changed = transaction.execute(
                "UPDATE multipart_uploads SET state = 'completing', completion_claim_id = ?2,
                 completion_blob_id = ?3, completion_parts_digest = ?4
                 WHERE upload_id = ?1 AND state = 'open' AND bucket_id = ?5 AND key = ?6",
                params![
                    request.upload.upload_id.to_string(),
                    claim.claim_id.as_slice(),
                    claim.blob_id.as_bytes().as_slice(),
                    selected.fingerprint.as_slice(),
                    selected.bucket_id.as_slice(),
                    request.upload.key.as_bytes()
                ],
            )?;

            if changed != 1 {
                return Err(MetadataError::Corrupt("completion claim fence failed").into());
            }

            Ok(true)
        })
    }

    pub(crate) fn publish_completion(
        &mut self,
        authority: &Authority,
        request: &CompleteRequest,
        selected: &CompletionSelection,
        claim: CompletionClaim,
        facts: &PayloadFacts,
    ) -> Result<CompletedUpload, OperationError> {
        if facts.length() != selected.layout.content_length()
            || facts.checksum().algorithm() != selected.checksum.algorithm()
        {
            return Err(
                MetadataError::Corrupt("assembled payload disagrees with selection").into(),
            );
        }

        let checksum = match selected.checksum.checksum_type() {
            ChecksumType::FullObject => ObjectChecksum::full(facts.checksum()),
            ChecksumType::Composite => ObjectChecksum::composite(
                selected.checksum.algorithm(),
                selected.parts.iter().map(|part| part.checksum),
            )?,
        };

        if request
            .checksum
            .is_some_and(|expected| expected != checksum)
        {
            return Err(OperationError::new(OperationFailure::BadDigest));
        }

        let result = CompletedUpload {
            length: ByteLength::new(facts.length())
                .map_err(|error| OperationError::caused(OperationFailure::StoreCorrupt, error))?,
            etag: selected.etag(),
            checksum,
        };

        let generation = StoreId::generate().map_err(MetadataError::Entropy)?;
        let finished = now()?;

        let mode = match checksum.checksum_type() {
            ChecksumType::FullObject => "FULL_OBJECT",
            ChecksumType::Composite => "COMPOSITE",
        };

        let receipt = ReceiptResult {
            object_generation: generation.to_string(),
            byte_length: facts.length(),
            etag: result.etag.clone(),
            checksum_type: mode.into(),
            checksums: BTreeMap::from([(
                checksum.digest().algorithm().as_str().into(),
                checksum.encode(),
            )]),
        };

        let encoded = serde_json::to_string(&receipt).map_err(io_error)?;
        let checksums = serde_json::to_string(&receipt.checksums).map_err(io_error)?;

        let part_checksums = selected
            .parts
            .iter()
            .map(|part| {
                serde_json::to_string(&BTreeMap::from([(
                    part.checksum.algorithm().as_str(),
                    part.checksum.encode(),
                )]))
                .map_err(io_error)
            })
            .collect::<Result<Vec<_>, _>>()?;

        #[cfg(test)]
        let _protocol = crate::protocol_fault::Scope::enter("multipart.complete");

        let publication = self.write(|transaction| {
            let (bucket_id, _) = selection::authorize(transaction, authority, request)?;

            if bucket_id != selected.bucket_id {
                return Err(OperationError::new(OperationFailure::OperationAborted));
            }

            if !matches_claim(transaction, request, selected, claim)? {
                return Err(OperationError::new(OperationFailure::OperationAborted));
            }

            let previous = objects::write_destination(
                transaction,
                bucket_id,
                request.upload.key.as_bytes(),
                &request.conditions,
            )?;

            let changed = transaction.execute(
                "UPDATE blobs SET state = 'ready', target_bucket_id = NULL, target_key = NULL,
                 target_upload_id = NULL, byte_length = ?2, part_count = ?3, etag = ?4,
                 md5_digest = ?5, checksum_type = ?6, checksums_json = ?7, gc_pending = 0,
                 gc_retry_at_ms = NULL, gc_attempt_count = 0, gc_error_code = NULL
                 WHERE blob_id = ?1 AND state = 'reserved' AND write_kind = 'complete_multipart_upload'
                 AND target_bucket_id = ?8 AND target_key = ?9 AND target_upload_id = ?10
                 AND target_part_number IS NULL",
                params![
                    claim.blob_id.as_bytes().as_slice(),
                    result.length.as_stored(),
                    i64::from(selected.layout.part_count()),
                    result.etag,
                    facts.md5().as_slice(),
                    mode,
                    checksums,
                    bucket_id.as_slice(),
                    request.upload.key.as_bytes(),
                    request.upload.upload_id.to_string()
                ],
            )?;

            if changed != 1 {
                return Err(MetadataError::Corrupt("completion lost its reservation").into());
            }

            #[cfg(test)]
            crate::protocol_fault::metadata("blob.ready")?;

            let mut insert = transaction.prepare_cached(
                "INSERT INTO blob_parts (blob_id, part_number, byte_offset, byte_length, checksums_json, part_index)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
            )?;

            for (index, (part, checksums)) in selected
                .layout
                .parts()
                .iter()
                .zip(&part_checksums)
                .enumerate()
            {
                insert.execute(params![
                    claim.blob_id.as_bytes().as_slice(),
                    i64::from(part.part_number),
                    i64::try_from(part.byte_offset).map_err(io_error)?,
                    i64::try_from(part.byte_length).map_err(io_error)?,
                    checksums,
                    i64::try_from(index + 1).map_err(io_error)?
                ])?;

                #[cfg(test)]
                crate::protocol_fault::hit("multipart.complete", "layout.part")?;
            }

            drop(insert);

            transaction.execute(
                "INSERT INTO objects (bucket_id, key, generation, blob_id, content_type,
                 content_encoding, content_disposition, content_language, cache_control,
                 expires_at_ms, user_metadata_json, tags_json, last_modified_at_ms, lifecycle_anchor_at_ms)
                 SELECT bucket_id, key, ?2, ?3, content_type, content_encoding, content_disposition,
                 content_language, cache_control, expires_at_ms, user_metadata_json, tags_json,
                 initiated_at_ms, initiated_at_ms FROM multipart_uploads WHERE upload_id = ?1
                 ON CONFLICT (bucket_id, key) DO UPDATE SET generation = excluded.generation,
                 blob_id = excluded.blob_id, content_type = excluded.content_type,
                 content_encoding = excluded.content_encoding, content_disposition = excluded.content_disposition,
                 content_language = excluded.content_language, cache_control = excluded.cache_control,
                 expires_at_ms = excluded.expires_at_ms, user_metadata_json = excluded.user_metadata_json,
                 tags_json = excluded.tags_json, last_modified_at_ms = excluded.last_modified_at_ms,
                 lifecycle_anchor_at_ms = excluded.lifecycle_anchor_at_ms",
                params![
                    request.upload.upload_id.to_string(),
                    generation.as_bytes().as_slice(),
                    claim.blob_id.as_bytes().as_slice()
                ],
            )?;

            #[cfg(test)]
            crate::protocol_fault::hit("multipart.complete", "object")?;

            super::abort::retire_parts(transaction, &request.upload.upload_id.to_string())?;

            #[cfg(test)]
            crate::protocol_fault::hit("multipart.complete", "parts-retired")?;

            transaction.execute(
                "INSERT INTO multipart_receipts (upload_id, bucket_id, key, outcome,
                 completion_claim_id, completion_blob_id, completion_parts_digest, result_json, finished_at_ms)
                 SELECT upload_id, bucket_id, key, 'completed', completion_claim_id, completion_blob_id,
                 completion_parts_digest, ?2, ?3 FROM multipart_uploads WHERE upload_id = ?1",
                params![request.upload.upload_id.to_string(), encoded, finished.get()],
            )?;

            #[cfg(test)]
            crate::protocol_fault::hit("multipart.complete", "receipt")?;

            transaction.execute(
                "DELETE FROM multipart_uploads WHERE upload_id = ?1",
                [request.upload.upload_id.to_string()],
            )?;

            #[cfg(test)]
            crate::protocol_fault::metadata("upload.removed")?;

            if let Some(previous) = previous {
                objects::retire(transaction, previous.blob_id)?;

                #[cfg(test)]
                crate::protocol_fault::metadata("predecessor.retired")?;
            }

            Ok(())
        });

        if let Err(error) = publication {
            if error.outcome() != Some(MutationOutcome::Unknown) {
                return Err(error);
            }

            // The output guard excludes receipt pruning, even after destination replacement.
            let witness = self.read(|transaction| {
                #[cfg(test)]
                crate::protocol_fault::hit("multipart.complete", "witness.before")?;

                let stored: Option<String> = transaction
                    .query_row(
                        "SELECT result_json FROM multipart_receipts WHERE upload_id = ?1 AND bucket_id = ?2
                         AND key = ?3 AND outcome = 'completed' AND completion_claim_id = ?4
                         AND completion_blob_id = ?5 AND completion_parts_digest = ?6",
                        params![
                            request.upload.upload_id.to_string(),
                            selected.bucket_id.as_slice(),
                            request.upload.key.as_bytes(),
                            claim.claim_id.as_slice(),
                            claim.blob_id.as_bytes().as_slice(),
                            selected.fingerprint.as_slice()
                        ],
                        |row| row.get(0),
                    )
                    .optional()
                    .map_err(decode_error)?;

                let Some(stored) = stored else {
                    return Ok(false);
                };

                if stored.len() > 16_384 {
                    return Err(MetadataError::Corrupt("oversized completion receipt").into());
                }

                let decoded: ReceiptResult = serde_json::from_str(&stored)
                    .map_err(|_| MetadataError::Corrupt("invalid completion receipt result"))?;

                #[cfg(test)]
                crate::protocol_fault::hit("multipart.complete", "witness.after")?;

                Ok(decoded == receipt && stored == encoded)
            });

            if !witness.is_ok_and(|confirmed| confirmed) {
                return Err(error);
            }
        }

        Ok(result)
    }
}

fn matches_claim(
    transaction: &Transaction<'_>,
    request: &CompleteRequest,
    selected: &CompletionSelection,
    claim: CompletionClaim,
) -> Result<bool, OperationError> {
    Ok(transaction
        .prepare_cached(
            "SELECT 1 FROM multipart_uploads WHERE upload_id = ?1 AND bucket_id = ?2 AND key = ?3
         AND state = 'completing' AND completion_claim_id = ?4 AND completion_blob_id = ?5
         AND completion_parts_digest = ?6",
        )?
        .exists(params![
            request.upload.upload_id.to_string(),
            selected.bucket_id.as_slice(),
            request.upload.key.as_bytes(),
            claim.claim_id.as_slice(),
            claim.blob_id.as_bytes().as_slice(),
            selected.fingerprint.as_slice()
        ])?)
}

fn io_error(error: impl std::error::Error + Send + Sync + 'static) -> OperationError {
    OperationError::caused(OperationFailure::IoError, error)
}
