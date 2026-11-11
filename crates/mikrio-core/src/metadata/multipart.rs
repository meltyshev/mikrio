//! Durable multipart metadata; payload work stays outside these transactions.

use rusqlite::params;

use super::{MetadataDatabase, MetadataError};
use crate::integrity::ChecksumType;
use crate::multipart::{CreateRequest, CreatedUpload};
use crate::object::Authority;
use crate::{EpochMillis, OperationError, OperationFailure, StoreId, UploadId};

pub(super) mod abort;
mod access;
mod completion;
mod listing;
mod parts;
#[cfg(test)]
pub(super) use parts::indexed_work as part_selection_work;
mod upload;
pub(super) mod uploads;

impl MetadataDatabase {
    pub(crate) fn create_multipart_upload(
        &mut self,
        authority: &Authority,
        request: &CreateRequest,
    ) -> Result<CreatedUpload, OperationError> {
        let initiated = super::now()?;

        let entropy = StoreId::generate().map_err(MetadataError::Entropy)?;
        let upload_id = UploadId::new(initiated, *entropy.as_bytes());
        self.initiate_multipart(authority, request, upload_id)
    }

    pub(super) fn initiate_multipart(
        &mut self,
        authority: &Authority,
        request: &CreateRequest,
        upload_id: UploadId,
    ) -> Result<CreatedUpload, OperationError> {
        let metadata = request.metadata.to_json().map_err(invalid)?;

        let tags = request
            .tags
            .clone()
            .unwrap_or_default()
            .to_json()
            .map_err(invalid)?;

        let encoded_id = upload_id.to_string();

        let checksum_type = match request.checksum.checksum_type() {
            ChecksumType::FullObject => "FULL_OBJECT",
            ChecksumType::Composite => "COMPOSITE",
        };

        #[cfg(test)]
        let _protocol = crate::protocol_fault::Scope::enter("multipart.initiate");

        self.write(|transaction| {
            let (bucket_id, initiator) = access::initiation(transaction, authority, request)?;

            // Active and terminal identities share one namespace even though
            // receipts deliberately have no live-upload foreign key.
            if transaction
                .prepare_cached(
                    "SELECT 1 FROM multipart_uploads WHERE upload_id = ?1
                 UNION ALL SELECT 1 FROM multipart_receipts WHERE upload_id = ?1 LIMIT 1",
                )?
                .exists([&encoded_id])?
            {
                return Err(OperationError::caused(
                    OperationFailure::IoError,
                    std::io::Error::other("generated multipart identity already exists"),
                ));
            }

            let fields = request.representation.fields();

            transaction.execute(
                "INSERT INTO multipart_uploads
                 (upload_id, bucket_id, key, initiator_id, state, checksum_algorithm,
                  checksum_type, content_type, content_encoding, content_disposition,
                  content_language, cache_control, expires_at_ms, user_metadata_json,
                  tags_json, initiated_at_ms)
                 VALUES (?1, ?2, ?3, ?4, 'open', ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12,
                         ?13, ?14, ?15)",
                params![
                    encoded_id,
                    bucket_id.as_slice(),
                    request.key.as_bytes(),
                    initiator,
                    request
                        .checksum
                        .is_explicit()
                        .then_some(request.checksum.algorithm().as_str()),
                    request.checksum.is_explicit().then_some(checksum_type),
                    fields.content_type,
                    fields.content_encoding,
                    fields.content_disposition,
                    fields.content_language,
                    fields.cache_control,
                    fields.expires.map(EpochMillis::get),
                    metadata,
                    tags,
                    upload_id.initiated_at().get(),
                ],
            )?;

            Ok(CreatedUpload {
                upload_id,
                checksum: request.checksum,
            })
        })
    }
}

fn invalid(error: crate::object::ObjectValueError) -> OperationError {
    OperationError::caused(OperationFailure::InvalidArgument, error).uncommitted()
}
