//! One bounded namespace retirement unit, independent of physical reclamation.

use rusqlite::OptionalExtension;

use super::{MetadataDatabase, MetadataError, decode_error};
use crate::{EpochMillis, OperationError, UploadId};

impl MetadataDatabase {
    pub(crate) fn retirement_candidate(
        &mut self,
        after: Option<[u8; 16]>,
    ) -> Result<Option<[u8; 16]>, OperationError> {
        self.read(|transaction| {
            let candidate: Option<Vec<u8>> = transaction
                .query_row(
                    "SELECT bucket_id FROM buckets WHERE name IS NULL AND bucket_id > ?1
                     ORDER BY bucket_id LIMIT 1",
                    [after.as_ref().map_or(&[][..], <[u8; 16]>::as_slice)],
                    |row| row.get(0),
                )
                .optional()
                .map_err(decode_error)?;

            let Some(candidate) = candidate else {
                return Ok(None);
            };

            let bucket_id: [u8; 16] = candidate
                .try_into()
                .map_err(|_| MetadataError::Corrupt("invalid retiring bucket identity"))?;

            Ok(Some(bucket_id))
        })
    }

    pub(crate) fn retire_namespace(&mut self, bucket_id: [u8; 16]) -> Result<(), OperationError> {
        let now = super::now()?;

        #[cfg(test)]
        let _protocol = crate::protocol_fault::Scope::enter("retirement");

        self.write(|transaction| {
            if !transaction
                .prepare_cached("SELECT 1 FROM buckets WHERE bucket_id = ?1 AND name IS NULL")?
                .exists([bucket_id.as_slice()])?
            {
                return Ok(());
            }

            if transaction
                .prepare_cached("SELECT 1 FROM objects WHERE bucket_id = ?1 LIMIT 1")?
                .exists([bucket_id.as_slice()])?
            {
                return Err(MetadataError::Corrupt("retired bucket has a current object").into());
            }

            transaction.execute(
                "DELETE FROM bucket_lifecycle_rules WHERE bucket_id = ?1 AND rule_index IN
                 (SELECT rule_index FROM bucket_lifecycle_rules WHERE bucket_id = ?1
                  ORDER BY rule_index LIMIT 256)",
                [bucket_id.as_slice()],
            )?;

            let upload: Option<(String, i64)> = transaction
                .query_row(
                    "SELECT upload_id, initiated_at_ms FROM multipart_uploads WHERE bucket_id = ?1
                     ORDER BY key, upload_id LIMIT 1",
                    [bucket_id.as_slice()],
                    |row| Ok((row.get(0)?, row.get(1)?)),
                )
                .optional()
                .map_err(decode_error)?;

            if let Some((upload, initiated)) = upload {
                let initiated = EpochMillis::new(initiated)
                    .map_err(|_| MetadataError::Corrupt("invalid retiring upload timestamp"))?;

                UploadId::parse(&upload, initiated)
                    .map_err(|_| MetadataError::Corrupt("invalid retiring upload identity"))?;

                super::multipart::abort::retire_upload(transaction, &upload, bucket_id, now)?;
            }

            transaction.execute(
                "DELETE FROM buckets WHERE bucket_id = ?1 AND name IS NULL
                 AND NOT EXISTS (SELECT 1 FROM bucket_lifecycle_rules WHERE bucket_id = ?1)
                 AND NOT EXISTS (SELECT 1 FROM multipart_uploads WHERE bucket_id = ?1)
                 AND NOT EXISTS (SELECT 1 FROM objects WHERE bucket_id = ?1)",
                [bucket_id.as_slice()],
            )?;

            Ok(())
        })
    }
}
