//! Bounded active-upload projection, shared by part writes and control requests.

use rusqlite::{Row, Transaction, params};

use super::super::{MetadataError, decode_error};
use crate::integrity::{ChecksumAlgorithm, ChecksumType};
use crate::multipart::MultipartChecksum;
use crate::object::ObjectKey;
use crate::{EpochMillis, OperationError, UploadId};

pub(super) struct SelectedUpload {
    pub checksum: MultipartChecksum,
    pub initiator: String,
    pub open: bool,
}

pub(super) fn select(
    transaction: &Transaction<'_>,
    upload_id: &UploadId,
    key: &ObjectKey,
    bucket_id: [u8; 16],
) -> Result<Option<SelectedUpload>, OperationError> {
    let mut statement = transaction.prepare_cached(
        "SELECT state, initiated_at_ms, checksum_algorithm, checksum_type,
         completion_claim_id, completion_blob_id, completion_parts_digest, initiator_id
         FROM multipart_uploads WHERE upload_id = ?1 AND bucket_id = ?2 AND key = ?3",
    )?;

    let mut rows = statement.query(params![
        upload_id.to_string(),
        bucket_id.as_slice(),
        key.as_bytes(),
    ])?;

    let Some(row) = rows.next()? else {
        return Ok(None);
    };

    decode(row, upload_id).map(Some)
}

pub(super) fn decode(
    row: &Row<'_>,
    upload_id: &UploadId,
) -> Result<SelectedUpload, OperationError> {
    let initiated = EpochMillis::new(row.get(1).map_err(decode_error)?)
        .map_err(|_| MetadataError::Corrupt("invalid upload initiation time"))?;

    UploadId::parse(&upload_id.to_string(), initiated)
        .map_err(|_| MetadataError::Corrupt("upload identity/time mismatch"))?;

    let algorithm: Option<String> = row.get(2).map_err(decode_error)?;
    let mode: Option<String> = row.get(3).map_err(decode_error)?;

    let checksum = match (algorithm.as_deref(), mode.as_deref()) {
        (None, None) => MultipartChecksum::resolve(None, None),
        (Some(algorithm), Some(mode)) => {
            let algorithm = ChecksumAlgorithm::parse(algorithm)
                .map_err(|_| MetadataError::Corrupt("invalid upload checksum algorithm"))?;

            let mode = match mode {
                "FULL_OBJECT" => ChecksumType::FullObject,
                "COMPOSITE" => ChecksumType::Composite,
                _ => return Err(MetadataError::Corrupt("invalid upload checksum type").into()),
            };

            MultipartChecksum::new(algorithm, mode)
        }

        _ => return Err(MetadataError::Corrupt("incomplete upload checksum negotiation").into()),
    }
    .map_err(|_| MetadataError::Corrupt("invalid upload checksum negotiation"))?;

    let state: String = row.get(0).map_err(decode_error)?;
    let claim: Option<Vec<u8>> = row.get(4).map_err(decode_error)?;
    let output: Option<Vec<u8>> = row.get(5).map_err(decode_error)?;
    let fingerprint: Option<Vec<u8>> = row.get(6).map_err(decode_error)?;

    let open = match (
        state.as_str(),
        claim.as_deref(),
        output.as_deref(),
        fingerprint.as_deref(),
    ) {
        ("open", None, None, None) => true,
        ("completing", Some(claim), Some(output), Some(fingerprint))
            if claim.len() == 16 && output.len() == 16 && fingerprint.len() == 32 =>
        {
            false
        }
        _ => return Err(MetadataError::Corrupt("invalid upload claim state").into()),
    };

    let initiator = row.get(7).map_err(decode_error)?;

    Ok(SelectedUpload {
        checksum,
        initiator,
        open,
    })
}
