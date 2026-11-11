//! A part replacement atomically changes its generation, payload and retirement hint.

use std::collections::BTreeMap;

use rusqlite::{Row, Transaction, params};

use super::super::{MetadataDatabase, MetadataError, decode_error, now, objects};
use crate::integrity::{Checksum, ChecksumAlgorithm, ChecksumType, PayloadFacts};
use crate::multipart::{
    ListedPart, MultipartChecksum, PART_MAX_BYTES, PartNumber, PartRequest, PartReservation,
};
use crate::object::{Authority, ByteLength};
use crate::{BlobId, EpochMillis, OperationError, OperationFailure, StoreId};

pub(super) const SELECT_RANGE: &str =
    "SELECT p.generation, p.blob_id, b.blob_id, b.shard, b.state, b.byte_length,
     b.etag, b.md5_digest, b.part_count, b.checksum_type, b.checksums_json,
     p.last_modified_at_ms, p.part_number FROM multipart_parts AS p
     LEFT JOIN blobs AS b ON b.blob_id = p.blob_id
     WHERE p.upload_id = ?1 AND p.part_number BETWEEN ?2 AND ?3 ORDER BY p.part_number";

impl MetadataDatabase {
    pub(crate) fn reserve_part(
        &mut self,
        authority: &Authority,
        request: &PartRequest,
        blob_id: BlobId,
    ) -> Result<Option<PartReservation>, OperationError> {
        let created = now()?;

        #[cfg(test)]
        let _protocol = crate::protocol_fault::Scope::enter("part.reservation");

        self.write(|transaction| {
            let bucket_id = super::access::part(transaction, authority, request)?;
            let checksum = open_upload(transaction, request, bucket_id)?;
            request.verifier(checksum)?;

            if transaction
                .prepare_cached("SELECT 1 FROM blobs WHERE blob_id = ?1")?
                .exists([blob_id.as_bytes().as_slice()])?
            {
                return Ok(None);
            }

            transaction.execute(
                "INSERT INTO blobs (blob_id, shard, state, write_kind, target_bucket_id,
                 target_key, target_upload_id, target_part_number, created_at_ms,
                 gc_pending, gc_attempt_count)
                 VALUES (?1, ?2, 'reserved', 'upload_part', ?3, ?4, ?5, ?6, ?7, 1, 0)",
                params![
                    blob_id.as_bytes().as_slice(),
                    i64::from(blob_id.shard()),
                    bucket_id.as_slice(),
                    request.key.as_bytes(),
                    request.upload_id.to_string(),
                    i64::from(request.part_number.get()),
                    created.get(),
                ],
            )?;

            Ok(Some(PartReservation {
                bucket_id,
                checksum,
            }))
        })
    }

    pub(crate) fn publish_part(
        &mut self,
        authority: &Authority,
        request: &PartRequest,
        reservation: PartReservation,
        blob_id: BlobId,
        facts: &PayloadFacts,
    ) -> Result<EpochMillis, OperationError> {
        let generation = StoreId::generate().map_err(MetadataError::Entropy)?;
        let modified = now()?;
        let etag = facts.etag();
        let checksum = facts.checksum();

        if facts.length() > PART_MAX_BYTES {
            return Err(OperationError::new(OperationFailure::EntityTooLarge).uncommitted());
        }

        let length = ByteLength::new(facts.length())
            .map_err(|error| OperationError::caused(OperationFailure::StoreCorrupt, error))?;

        let checksums = serde_json::to_string(&BTreeMap::from([(
            checksum.algorithm().as_str(),
            checksum.encode(),
        )]))
        .map_err(|error| OperationError::caused(OperationFailure::IoError, error))?;

        #[cfg(test)]
        let _protocol = crate::protocol_fault::Scope::enter("part.publication");

        let result = self.write(|transaction| {
            let bucket_id = super::access::part(transaction, authority, request)?;

            if bucket_id != reservation.bucket_id {
                return Err(OperationError::new(OperationFailure::NoSuchUpload));
            }

            let selected = open_upload(transaction, request, bucket_id)?;

            if selected != reservation.checksum {
                return Err(MetadataError::Corrupt("upload checksum negotiation changed").into());
            }

            if checksum.algorithm() != selected.algorithm() {
                return Err(crate::integrity::IntegrityError::AlgorithmMismatch.into());
            }

            let previous = current_part(
                transaction,
                &request.upload_id,
                request.part_number,
                selected.algorithm(),
            )?;

            let changed = transaction.execute(
                "UPDATE blobs SET state = 'ready', target_bucket_id = NULL, target_key = NULL,
                 target_upload_id = NULL, target_part_number = NULL,
                 byte_length = ?2, etag = ?3, md5_digest = ?4, checksum_type = 'FULL_OBJECT',
                 checksums_json = ?5, gc_pending = 0, gc_retry_at_ms = NULL,
                 gc_attempt_count = 0, gc_error_code = NULL
                 WHERE blob_id = ?1 AND state = 'reserved' AND write_kind = 'upload_part'
                 AND target_bucket_id = ?6 AND target_key = ?7
                 AND target_upload_id = ?8 AND target_part_number = ?9",
                params![
                    blob_id.as_bytes().as_slice(),
                    length.as_stored(),
                    etag,
                    facts.md5().as_slice(),
                    checksums,
                    bucket_id.as_slice(),
                    request.key.as_bytes(),
                    request.upload_id.to_string(),
                    i64::from(request.part_number.get()),
                ],
            )?;

            if changed != 1 {
                return Err(MetadataError::Corrupt("part publication lost its reservation").into());
            }

            #[cfg(test)]
            crate::protocol_fault::metadata("blob.ready")?;

            transaction.execute(
                "INSERT INTO multipart_parts
                 (upload_id, part_number, generation, blob_id, last_modified_at_ms)
                 VALUES (?1, ?2, ?3, ?4, ?5)
                 ON CONFLICT (upload_id, part_number) DO UPDATE SET
                 generation = excluded.generation, blob_id = excluded.blob_id,
                 last_modified_at_ms = excluded.last_modified_at_ms",
                params![
                    request.upload_id.to_string(),
                    i64::from(request.part_number.get()),
                    generation.as_bytes().as_slice(),
                    blob_id.as_bytes().as_slice(),
                    modified.get(),
                ],
            )?;

            #[cfg(test)]
            crate::protocol_fault::metadata("part.replaced")?;

            if let Some(previous) = previous {
                objects::retire(transaction, previous.blob_id)?;

                #[cfg(test)]
                crate::protocol_fault::metadata("predecessor.retired")?;
            }

            Ok(())
        });

        self.resolve_blob_publication(result, blob_id)?;
        Ok(modified)
    }
}

fn open_upload(
    transaction: &Transaction<'_>,
    request: &PartRequest,
    bucket_id: [u8; 16],
) -> Result<MultipartChecksum, OperationError> {
    let selected = super::upload::select(transaction, &request.upload_id, &request.key, bucket_id)?
        .ok_or_else(|| OperationError::new(OperationFailure::NoSuchUpload))?;

    if !selected.open {
        return Err(OperationError::new(OperationFailure::OperationAborted));
    }

    Ok(selected.checksum)
}

pub(super) fn current_part(
    transaction: &Transaction<'_>,
    upload_id: &crate::UploadId,
    part_number: PartNumber,
    algorithm: ChecksumAlgorithm,
) -> Result<Option<CommittedPart>, OperationError> {
    let mut statement = transaction.prepare_cached(SELECT_RANGE)?;

    let mut rows = statement.query(params![
        upload_id.to_string(),
        part_number.get(),
        part_number.get()
    ])?;

    rows.next()?
        .map(|row| decode_part(row, algorithm))
        .transpose()
        .map_err(OperationError::from)
}

pub(super) struct CommittedPart {
    pub generation: [u8; 16],
    pub blob_id: BlobId,
    pub md5: [u8; 16],
    pub listed: ListedPart,
}

pub(super) fn decode_part(
    row: &Row<'_>,
    algorithm: ChecksumAlgorithm,
) -> Result<CommittedPart, MetadataError> {
    let generation: [u8; 16] = row
        .get::<_, Vec<u8>>(0)
        .map_err(decode_error)?
        .try_into()
        .map_err(|_| MetadataError::Corrupt("invalid part generation"))?;

    let blob_id: [u8; 16] = row
        .get::<_, Vec<u8>>(1)
        .map_err(decode_error)?
        .try_into()
        .map_err(|_| MetadataError::Corrupt("invalid part blob identity"))?;

    let stored_id: Option<Vec<u8>> = row.get(2).map_err(decode_error)?;
    let state: Option<String> = row.get(4).map_err(decode_error)?;

    if stored_id.as_deref() != Some(blob_id.as_slice()) || state.as_deref() != Some("ready") {
        return Err(MetadataError::Corrupt("missing or non-ready part blob"));
    }

    let blob_id = BlobId::from_bytes(blob_id);
    let shard: i64 = row.get(3).map_err(decode_error)?;

    if shard != i64::from(blob_id.shard()) {
        return Err(MetadataError::Corrupt("part blob shard mismatch"));
    }

    let length = ByteLength::from_stored(row.get(5).map_err(decode_error)?)
        .map_err(|_| MetadataError::Corrupt("invalid part length"))?;

    let count: Option<i64> = row.get(8).map_err(decode_error)?;
    let mode: Option<String> = row.get(9).map_err(decode_error)?;

    if length.get() > PART_MAX_BYTES || count.is_some() || mode.as_deref() != Some("FULL_OBJECT") {
        return Err(MetadataError::Corrupt("invalid part payload facts"));
    }

    let etag: String = row.get(6).map_err(decode_error)?;
    let md5: Vec<u8> = row.get(7).map_err(decode_error)?;
    objects::validate_etag(&etag, &md5, None)?;

    let md5 = md5
        .try_into()
        .map_err(|_| MetadataError::Corrupt("invalid part digest"))?;

    let encoded: String = row.get(10).map_err(decode_error)?;
    let checksums = objects::decode_checksums(&encoded, Some(ChecksumType::FullObject), None)?;

    if checksums.len() != 1 || !checksums.contains_key(algorithm.as_str()) {
        return Err(MetadataError::Corrupt(
            "part checksum does not match upload",
        ));
    }

    let last_modified = EpochMillis::new(row.get(11).map_err(decode_error)?)
        .map_err(|_| MetadataError::Corrupt("invalid part modification time"))?;

    let part_number = PartNumber::new(row.get(12).map_err(decode_error)?)
        .map_err(|_| MetadataError::Corrupt("invalid stored part number"))?;

    let encoded = checksums
        .get(algorithm.as_str())
        .ok_or(MetadataError::Corrupt("missing part checksum"))?;

    let checksum = Checksum::parse(algorithm, encoded)
        .map_err(|_| MetadataError::Corrupt("invalid part checksum"))?;

    Ok(CommittedPart {
        generation,
        blob_id,
        md5,
        listed: ListedPart {
            part_number,
            size: length,
            etag,
            last_modified,
            checksum,
        },
    })
}

#[cfg(test)]
pub(in crate::metadata) fn indexed_work(
    connection: &rusqlite::Connection,
) -> rusqlite::Result<i32> {
    use rusqlite::StatementStatus;

    let statement = connection.prepare_cached(SELECT_RANGE)?;
    assert_eq!(statement.get_status(StatementStatus::FullscanStep), 0);
    assert_eq!(statement.get_status(StatementStatus::Sort), 0);
    assert_eq!(statement.get_status(StatementStatus::AutoIndex), 0);
    Ok(statement.get_status(StatementStatus::Run))
}
