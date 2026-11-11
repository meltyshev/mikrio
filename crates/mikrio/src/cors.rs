//! Owned bucket CORS values and operations over the shared core worker.

use crate::{StorageError, Store};

mod inputs;
mod rule;

pub use inputs::{
    DeleteBucketCorsInput, DeleteBucketCorsInputBuilder, GetBucketCorsInput,
    GetBucketCorsInputBuilder, PutBucketCorsInput, PutBucketCorsInputBuilder,
};
pub use rule::{CorsRule, CorsRuleBuilder};

/// Confirmation that the complete CORS configuration was replaced.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PutBucketCorsOutput;

/// Detached ordered CORS rules, usable after the connection closes.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct GetBucketCorsOutput {
    cors_rules: Vec<CorsRule>,
}

impl GetBucketCorsOutput {
    /// Current rules in their stored evaluation order.
    #[must_use]
    pub fn cors_rules(&self) -> &[CorsRule] {
        &self.cors_rules
    }
}

/// Confirmation that the CORS configuration is absent.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct DeleteBucketCorsOutput;

impl Store {
    /// Replace the complete bucket CORS configuration atomically.
    /// Dropping this future cancels work before its commit section.
    ///
    /// # Errors
    /// Preserves missing-bucket, owner, lifecycle and storage failures.
    /// Dispatched mutation failures retain their authoritative outcome.
    pub async fn put_bucket_cors(
        &self,
        input: PutBucketCorsInput,
    ) -> Result<PutBucketCorsOutput, StorageError> {
        self.connection
            .put_bucket_cors(input.request, input.configuration)
            .await
            .map_err(StorageError::from_operation)?;

        Ok(PutBucketCorsOutput)
    }

    /// Read the complete current CORS configuration.
    ///
    /// # Errors
    /// An absent configuration returns `NoSuchCORSConfiguration`.
    /// Preserves missing-bucket, owner, corruption and lifecycle/storage failures.
    pub async fn get_bucket_cors(
        &self,
        input: GetBucketCorsInput,
    ) -> Result<GetBucketCorsOutput, StorageError> {
        let configuration = self
            .connection
            .get_bucket_cors(input.request)
            .await
            .map_err(StorageError::from_operation)?;

        Ok(GetBucketCorsOutput {
            cors_rules: configuration
                .into_rules()
                .into_iter()
                .map(CorsRule::from_core)
                .collect(),
        })
    }

    /// Delete CORS configuration idempotently without changing objects or access.
    /// Dropping this future cancels work before its commit section.
    ///
    /// # Errors
    /// Preserves missing-bucket, owner, lifecycle and storage failures.
    /// Dispatched mutation failures retain their authoritative outcome.
    pub async fn delete_bucket_cors(
        &self,
        input: DeleteBucketCorsInput,
    ) -> Result<DeleteBucketCorsOutput, StorageError> {
        self.connection
            .delete_bucket_cors(input.request)
            .await
            .map_err(StorageError::from_operation)?;

        Ok(DeleteBucketCorsOutput)
    }
}

fn invalid(error: mikrio_core::cors::CorsError) -> StorageError {
    StorageError {
        code: crate::ErrorCode::InvalidArgument,
        cause: crate::ErrorCause::Cors(error),
        mutation_outcome: None,
        partial_result: None,
    }
}
