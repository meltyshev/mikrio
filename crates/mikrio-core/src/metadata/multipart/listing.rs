//! Indexed committed-part pages; no payload access or transaction across pages.

use rusqlite::params;

use super::super::MetadataDatabase;
use crate::OperationError;
use crate::multipart::{ListPartsRequest, PartPage};
use crate::object::Authority;

impl MetadataDatabase {
    pub(crate) fn list_parts(
        &mut self,
        authority: &Authority,
        request: &ListPartsRequest,
    ) -> Result<PartPage, OperationError> {
        self.read(|transaction| {
            let (_, selected) = super::access::active(
                transaction,
                authority,
                &request.upload,
                "s3:ListMultipartUploadParts",
            )?;

            let mut page = PartPage {
                parts: Vec::new(),
                next_part_number_marker: None,
                checksum: selected.checksum,
                initiator: selected.initiator,
                owner: super::super::admin::owner_info(transaction)?,
            };

            if request.max_parts.get() == 0 {
                return Ok(page);
            }

            let mut statement = transaction.prepare_cached(
                "SELECT p.generation, p.blob_id, b.blob_id, b.shard, b.state, b.byte_length,
                 b.etag, b.md5_digest, b.part_count, b.checksum_type, b.checksums_json,
                 p.last_modified_at_ms, p.part_number FROM multipart_parts AS p
                 LEFT JOIN blobs AS b ON b.blob_id = p.blob_id
                 WHERE p.upload_id = ?1 AND p.part_number > ?2 ORDER BY p.part_number LIMIT ?3",
            )?;

            let mut rows = statement.query(params![
                request.upload.upload_id.to_string(),
                i64::from(request.part_number_marker),
                i64::from(request.max_parts.get()) + 1,
            ])?;

            let mut remaining = request.max_parts.get();

            while let Some(row) = rows.next()? {
                let part = super::parts::decode_part(row, selected.checksum.algorithm())?;

                if remaining == 0 {
                    page.next_part_number_marker =
                        page.parts.last().map(|part| part.part_number.get());

                    break;
                }

                page.parts.push(part.listed);
                remaining -= 1;
            }

            Ok(page)
        })
    }
}
