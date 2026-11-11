//! Snapshot selection and a format-defined fingerprint over exact part generations.

use rusqlite::{Transaction, params};
use sha2::Digest as _;

use super::super::{parts, upload};
use crate::integrity::{ChecksumType, MultipartLayout, PartLayout};
use crate::metadata::{MetadataError, decode_error};
use crate::multipart::{CompleteRequest, CompletionSelection, InputPart};
use crate::object::{Authority, ObjectTags, Representation, RepresentationInput, UserMetadata};
use crate::{EpochMillis, OperationError, OperationFailure};

pub(super) fn select(
    transaction: &Transaction<'_>,
    authority: &Authority,
    request: &CompleteRequest,
) -> Result<CompletionSelection, OperationError> {
    let (bucket_id, upload) = authorize(transaction, authority, request)?;

    if !upload.open {
        return Err(OperationError::new(OperationFailure::OperationAborted));
    }

    if request
        .checksum_type
        .is_some_and(|mode| mode != upload.checksum.checksum_type())
    {
        return Err(crate::integrity::IntegrityError::AlgorithmMismatch.into());
    }

    if request.checksum.is_some_and(|checksum| {
        checksum.digest().algorithm() != upload.checksum.algorithm()
            || checksum.checksum_type() != upload.checksum.checksum_type()
    }) {
        return Err(crate::integrity::IntegrityError::AlgorithmMismatch.into());
    }

    let mut selected = Vec::with_capacity(request.parts.parts().len());
    let mut layout = Vec::with_capacity(request.parts.parts().len());
    let mut length = 0_u64;
    let upload_id = request.upload.upload_id.to_string();
    let mut statement = transaction.prepare_cached(parts::SELECT_RANGE)?;

    // Read only requested runs. Sparse selections never scan or decode omitted
    // parts, and the same transaction still revalidates each exact generation.
    for run in request
        .parts
        .parts()
        .chunk_by(|left, right| left.part_number.get() + 1 == right.part_number.get())
    {
        let first = run
            .first()
            .ok_or_else(|| OperationError::new(OperationFailure::InvalidPart))?;

        let last = run
            .last()
            .ok_or_else(|| OperationError::new(OperationFailure::InvalidPart))?;

        let mut rows = statement.query(params![
            upload_id,
            first.part_number.get(),
            last.part_number.get()
        ])?;

        for requested in run {
            let index = selected.len();

            if upload.checksum.checksum_type() == ChecksumType::Composite
                && usize::try_from(requested.part_number.get()).ok() != Some(index + 1)
            {
                return Err(OperationError::new(OperationFailure::InvalidPartOrder));
            }

            let row = rows
                .next()?
                .ok_or_else(|| OperationError::new(OperationFailure::InvalidPart))?;

            // A hole is InvalidPart even when the next stored part is corrupt.
            // Preserve the point lookup's error order before decoding its facts.
            if row.get::<_, u32>(12).map_err(decode_error)? != requested.part_number.get() {
                return Err(OperationError::new(OperationFailure::InvalidPart));
            }

            let current = parts::decode_part(row, upload.checksum.algorithm())?;

            if requested.etag != current.listed.etag {
                return Err(OperationError::new(OperationFailure::InvalidPart));
            }

            if requested
                .checksum
                .is_some_and(|checksum| checksum != current.listed.checksum)
            {
                return Err(OperationError::new(OperationFailure::InvalidPart));
            }

            if upload.checksum.checksum_type() == ChecksumType::Composite
                && requested.checksum.is_none()
            {
                return Err(OperationError::new(OperationFailure::InvalidPart));
            }

            if index + 1 < request.parts.parts().len()
                && current.listed.size.get() < 5 * 1024 * 1024
            {
                return Err(OperationError::new(OperationFailure::EntityTooSmall));
            }

            layout.push(PartLayout {
                part_number: requested.part_number.get(),
                byte_offset: length,
                byte_length: current.listed.size.get(),
                checksums: vec![current.listed.checksum],
            });

            length = length
                .checked_add(current.listed.size.get())
                .ok_or_else(|| OperationError::new(OperationFailure::EntityTooLarge))?;

            selected.push(InputPart {
                generation: current.generation,
                blob_id: current.blob_id,
                part_number: requested.part_number,
                length: current.listed.size,
                etag: current.listed.etag,
                md5: current.md5,
                checksum: current.listed.checksum,
            });
        }
    }

    if request
        .content_length
        .is_some_and(|expected| expected.get() != length)
    {
        return Err(OperationError::new(OperationFailure::InvalidRequest));
    }

    let layout = MultipartLayout::new(layout, length)
        .map_err(|error| OperationError::caused(OperationFailure::StoreCorrupt, error))?;

    let fingerprint = fingerprint(&selected)?;

    Ok(CompletionSelection {
        bucket_id,
        checksum: upload.checksum,
        parts: selected.into_boxed_slice(),
        layout,
        fingerprint,
    })
}

pub(super) fn authorize(
    transaction: &Transaction<'_>,
    authority: &Authority,
    request: &CompleteRequest,
) -> Result<([u8; 16], upload::SelectedUpload), OperationError> {
    let bucket_id = super::super::super::objects::access::bucket(
        transaction,
        authority,
        &request.upload.bucket,
        request.upload.expected_owner.as_deref(),
    )?;

    super::super::access::completion(transaction, authority, request, bucket_id)?;

    let upload = upload::select(
        transaction,
        &request.upload.upload_id,
        &request.upload.key,
        bucket_id,
    )?
    .ok_or_else(|| OperationError::new(OperationFailure::NoSuchUpload))?;

    let owner = super::super::super::admin::owner_info(transaction)?;
    super::super::access::validate_initiator(&upload.initiator, &owner)?;
    validate_metadata(transaction, &request.upload.upload_id.to_string())?;

    Ok((bucket_id, upload))
}

fn validate_metadata(transaction: &Transaction<'_>, upload_id: &str) -> Result<(), OperationError> {
    let mut statement = transaction.prepare_cached(
        "SELECT content_type, content_encoding, content_disposition, content_language,
         cache_control, expires_at_ms, user_metadata_json, tags_json
         FROM multipart_uploads WHERE upload_id = ?1",
    )?;

    let mut rows = statement.query([upload_id])?;

    let row = rows
        .next()?
        .ok_or(MetadataError::Corrupt("missing upload metadata"))?;

    Representation::new(RepresentationInput {
        content_type: row.get(0).map_err(decode_error)?,
        content_encoding: row.get(1).map_err(decode_error)?,
        content_disposition: row.get(2).map_err(decode_error)?,
        content_language: row.get(3).map_err(decode_error)?,
        cache_control: row.get(4).map_err(decode_error)?,
        expires: row
            .get::<_, Option<i64>>(5)
            .map_err(decode_error)?
            .map(EpochMillis::new)
            .transpose()
            .map_err(|_| MetadataError::Corrupt("invalid upload expiry"))?,
    })
    .map_err(|_| MetadataError::Corrupt("invalid upload representation"))?;

    let metadata: String = row.get(6).map_err(decode_error)?;

    UserMetadata::from_json(&metadata)
        .map_err(|_| MetadataError::Corrupt("invalid upload metadata"))?;

    let tags: String = row.get(7).map_err(decode_error)?;

    ObjectTags::from_json(&tags).map_err(|_| MetadataError::Corrupt("invalid upload tags"))?;

    Ok(())
}

fn fingerprint(parts: &[InputPart]) -> Result<[u8; 32], OperationError> {
    let mut digest = sha2::Sha256::new();
    digest.update(b"mikrio.multipart.selection\0\x01");

    let count =
        u32::try_from(parts.len()).map_err(|_| MetadataError::Corrupt("invalid selected count"))?;

    digest.update(count.to_be_bytes());

    for part in parts {
        digest.update(part.part_number.get().to_be_bytes());

        for bytes in [
            part.generation.as_slice(),
            part.blob_id.as_bytes().as_slice(),
            part.etag.as_bytes(),
            part.checksum.algorithm().as_str().as_bytes(),
            part.checksum.as_bytes(),
        ] {
            let length = u32::try_from(bytes.len())
                .map_err(|_| MetadataError::Corrupt("invalid fingerprint field"))?;

            digest.update(length.to_be_bytes());
            digest.update(bytes);
        }
    }

    Ok(digest.finalize().into())
}
