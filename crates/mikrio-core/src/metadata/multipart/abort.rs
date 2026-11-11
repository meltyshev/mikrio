//! One atomic upload retirement shared by explicit abort and namespace removal.

use rusqlite::{Transaction, params};

use super::super::{MetadataDatabase, MetadataError};
use crate::multipart::UploadRequest;
use crate::object::Authority;
use crate::{EpochMillis, OperationError};

impl MetadataDatabase {
    pub(crate) fn abort_multipart_upload(
        &mut self,
        authority: &Authority,
        request: &UploadRequest,
    ) -> Result<(), OperationError> {
        let now = super::super::now()?;

        #[cfg(test)]
        let _protocol = crate::protocol_fault::Scope::enter("multipart.abort");

        self.write(|transaction| {
            let (bucket_id, _) =
                super::access::active(transaction, authority, request, "s3:AbortMultipartUpload")?;

            retire_upload(transaction, &request.upload_id.to_string(), bucket_id, now)
        })
    }
}

pub(in crate::metadata) fn retire_upload(
    transaction: &Transaction<'_>,
    upload_id: &str,
    bucket_id: [u8; 16],
    now: EpochMillis,
) -> Result<(), OperationError> {
    if transaction
        .prepare_cached(
            "SELECT 1 FROM multipart_uploads AS u
             LEFT JOIN blobs AS b ON b.blob_id = u.completion_blob_id
             WHERE u.upload_id = ?1 AND u.completion_blob_id IS NOT NULL AND
             (b.state IS NULL OR b.state <> 'reserved'
              OR b.write_kind <> 'complete_multipart_upload'
              OR b.target_bucket_id IS NOT u.bucket_id OR b.target_key IS NOT u.key
              OR b.target_upload_id IS NOT u.upload_id) LIMIT 1",
        )?
        .exists([upload_id])?
    {
        return Err(MetadataError::Corrupt("invalid retiring completion claim").into());
    }

    retire_parts(transaction, upload_id)?;

    transaction.execute(
        "UPDATE blobs SET gc_pending = 1 WHERE blob_id IN (SELECT completion_blob_id FROM multipart_uploads
             WHERE upload_id = ?1 AND completion_blob_id IS NOT NULL)",
        [upload_id],
    )?;

    transaction.execute(
        "INSERT INTO multipart_receipts (upload_id, bucket_id, key, outcome,
            completion_claim_id, completion_blob_id, completion_parts_digest,
            result_json, finished_at_ms)
         SELECT upload_id, bucket_id, key, 'aborted', completion_claim_id,
            completion_blob_id, completion_parts_digest, NULL, ?2
         FROM multipart_uploads WHERE upload_id = ?1",
        params![upload_id, now.get()],
    )?;

    let changed = transaction.execute(
        "DELETE FROM multipart_uploads WHERE upload_id = ?1 AND bucket_id = ?2",
        params![upload_id, bucket_id.as_slice()],
    )?;

    if changed != 1 {
        return Err(MetadataError::Corrupt("retiring upload fence failed").into());
    }

    Ok(())
}

pub(super) fn retire_parts(
    transaction: &Transaction<'_>,
    upload_id: &str,
) -> Result<(), OperationError> {
    let count: i64 = transaction.query_row(
        "SELECT count(*) FROM
         (SELECT 1 FROM multipart_parts WHERE upload_id = ?1 LIMIT 10001)",
        [upload_id],
        |row| row.get(0),
    )?;

    if count > 10_000 {
        return Err(MetadataError::Corrupt("upload exceeds part limit").into());
    }

    if transaction
        .prepare_cached(
            "SELECT 1 FROM multipart_parts AS p LEFT JOIN blobs AS b ON b.blob_id = p.blob_id
             WHERE p.upload_id = ?1 AND
             (b.state IS NULL OR b.state <> 'ready' OR p.part_number > 10000) LIMIT 1",
        )?
        .exists([upload_id])?
    {
        return Err(MetadataError::Corrupt("invalid retiring part reference").into());
    }

    transaction.execute(
        "UPDATE blobs SET gc_pending = 1 WHERE blob_id IN
         (SELECT blob_id FROM multipart_parts WHERE upload_id = ?1)",
        [upload_id],
    )?;

    #[cfg(test)]
    crate::protocol_fault::metadata("parts.marked")?;

    transaction.execute(
        "DELETE FROM multipart_parts WHERE upload_id = ?1",
        [upload_id],
    )?;

    Ok(())
}
