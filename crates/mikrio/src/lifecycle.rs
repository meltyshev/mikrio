//! Owned bucket lifecycle values and operations over the shared core worker.

use crate::{StorageError, Store};

mod inputs;
mod rule;

pub use inputs::{
    DeleteBucketLifecycleInput, DeleteBucketLifecycleInputBuilder,
    GetBucketLifecycleConfigurationInput, GetBucketLifecycleConfigurationInputBuilder,
    PutBucketLifecycleConfigurationInput, PutBucketLifecycleConfigurationInputBuilder,
};
pub use rule::{
    AbortIncompleteMultipartUpload, LifecycleExpiration, LifecycleFilter, LifecycleRule,
    LifecycleRuleBuilder, LifecycleStatus,
};

/// Confirmation that the complete lifecycle configuration was replaced.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PutBucketLifecycleConfigurationOutput;

/// Detached ordered lifecycle rules, usable after the connection closes.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct GetBucketLifecycleConfigurationOutput {
    lifecycle_rules: Vec<LifecycleRule>,
}

impl GetBucketLifecycleConfigurationOutput {
    /// Current rules in their stored evaluation order.
    #[must_use]
    pub fn lifecycle_rules(&self) -> &[LifecycleRule] {
        &self.lifecycle_rules
    }
}

/// Confirmation that the lifecycle configuration is absent.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct DeleteBucketLifecycleOutput;

impl Store {
    /// Replace the complete bucket lifecycle configuration atomically.
    /// Dropping this future cancels work before its commit section.
    ///
    /// # Errors
    /// Preserves missing-bucket, owner, lifecycle and storage failures.
    /// Dispatched mutation failures retain their authoritative outcome.
    pub async fn put_bucket_lifecycle_configuration(
        &self,
        input: PutBucketLifecycleConfigurationInput,
    ) -> Result<PutBucketLifecycleConfigurationOutput, StorageError> {
        self.connection
            .put_bucket_lifecycle(input.request, input.configuration)
            .await
            .map_err(StorageError::from_operation)?;

        Ok(PutBucketLifecycleConfigurationOutput)
    }

    /// Read the complete current lifecycle configuration.
    ///
    /// # Errors
    /// An absent configuration returns `NoSuchLifecycleConfiguration`.
    /// Preserves missing-bucket, owner, corruption and lifecycle/storage failures.
    pub async fn get_bucket_lifecycle_configuration(
        &self,
        input: GetBucketLifecycleConfigurationInput,
    ) -> Result<GetBucketLifecycleConfigurationOutput, StorageError> {
        let configuration = self
            .connection
            .get_bucket_lifecycle(input.request)
            .await
            .map_err(StorageError::from_operation)?
            .ok_or_else(|| StorageError {
                code: crate::ErrorCode::NoSuchLifecycleConfiguration,
                cause: crate::ErrorCause::External(std::io::Error::new(
                    std::io::ErrorKind::NotFound,
                    "bucket has no lifecycle configuration",
                )),
                mutation_outcome: None,
                partial_result: None,
            })?;

        Ok(GetBucketLifecycleConfigurationOutput {
            lifecycle_rules: configuration
                .into_rules()
                .into_iter()
                .map(LifecycleRule::from_core)
                .collect(),
        })
    }

    /// Delete lifecycle configuration idempotently without changing objects or access.
    /// Dropping this future cancels work before its commit section.
    ///
    /// # Errors
    /// Preserves missing-bucket, owner, lifecycle and storage failures.
    /// Dispatched mutation failures retain their authoritative outcome.
    pub async fn delete_bucket_lifecycle(
        &self,
        input: DeleteBucketLifecycleInput,
    ) -> Result<DeleteBucketLifecycleOutput, StorageError> {
        self.connection
            .delete_bucket_lifecycle(input.request)
            .await
            .map_err(StorageError::from_operation)?;

        Ok(DeleteBucketLifecycleOutput)
    }
}

fn invalid(error: mikrio_core::lifecycle::LifecycleError) -> StorageError {
    StorageError {
        code: crate::ErrorCode::InvalidArgument,
        cause: crate::ErrorCause::Lifecycle(error),
        mutation_outcome: None,
        partial_result: None,
    }
}
