//! Current bucket CORS state shares the metadata transaction and outcome protocol.

use rusqlite::{Transaction, params};

use super::{MetadataDatabase, MetadataError, bucket_configuration, buckets, decode_error};
use crate::access::BucketAccessRequest;
use crate::cors::CorsConfiguration;
use crate::object::{Authority, BucketName};
use crate::{OperationError, OperationFailure};

impl MetadataDatabase {
    pub(crate) fn put_bucket_cors(
        &mut self,
        authority: &Authority,
        request: &BucketAccessRequest,
        configuration: &CorsConfiguration,
    ) -> Result<(), OperationError> {
        let encoded = configuration.encode().map_err(|error| {
            OperationError::caused(OperationFailure::InvalidArgument, error).uncommitted()
        })?;

        #[cfg(test)]
        let _protocol = crate::protocol_fault::Scope::enter("bucket.cors.put");

        self.write(|transaction| {
            let current =
                bucket_configuration::current(transaction, authority, request, "s3:PutBucketCORS")?;

            bucket_configuration::changed(transaction.execute(
                "UPDATE buckets SET cors_rules_json = ?2 WHERE bucket_id = ?1",
                params![current.bucket_id.as_slice(), encoded],
            )?)
        })
    }

    pub(crate) fn get_bucket_cors(
        &mut self,
        authority: &Authority,
        request: &BucketAccessRequest,
    ) -> Result<CorsConfiguration, OperationError> {
        self.read(|transaction| {
            let current =
                bucket_configuration::current(transaction, authority, request, "s3:GetBucketCORS")?;

            configuration(transaction, &current.bucket_id)?
                .ok_or_else(|| OperationError::new(OperationFailure::NoSuchCorsConfiguration))
        })
    }

    pub(crate) fn delete_bucket_cors(
        &mut self,
        authority: &Authority,
        request: &BucketAccessRequest,
    ) -> Result<(), OperationError> {
        #[cfg(test)]
        let _protocol = crate::protocol_fault::Scope::enter("bucket.cors.delete");

        self.write(|transaction| {
            let current =
                bucket_configuration::current(transaction, authority, request, "s3:PutBucketCORS")?;

            bucket_configuration::changed(transaction.execute(
                "UPDATE buckets SET cors_rules_json = NULL WHERE bucket_id = ?1",
                [current.bucket_id.as_slice()],
            )?)
        })
    }

    /// Public CORS matching is independent of data/configuration permissions.
    /// This narrow snapshot grants neither object access nor configuration writes.
    pub(crate) fn cors_for_response(
        &mut self,
        bucket: &BucketName,
    ) -> Result<Option<CorsConfiguration>, OperationError> {
        self.read(|transaction| {
            let bucket_id = buckets::require_bucket(transaction, bucket, None)?;
            configuration(transaction, &bucket_id)
        })
    }
}

fn configuration(
    transaction: &Transaction<'_>,
    bucket_id: &[u8; 16],
) -> Result<Option<CorsConfiguration>, OperationError> {
    let encoded: Option<String> = transaction
        .query_row(
            "SELECT cors_rules_json FROM buckets WHERE bucket_id = ?1",
            [bucket_id.as_slice()],
            |row| row.get(0),
        )
        .map_err(decode_error)?;

    encoded
        .map(|encoded| {
            CorsConfiguration::decode(&encoded)
                .map_err(|_| MetadataError::Corrupt("invalid stored CORS configuration").into())
        })
        .transpose()
}
