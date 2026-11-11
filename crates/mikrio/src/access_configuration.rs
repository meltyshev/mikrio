//! Trusted native access configuration uses the same transactions as HTTP.

use crate::{StorageError, Store};

mod inputs;
mod object_acl;
mod values;

pub use inputs::{
    DeleteBucketPolicyInput, DeleteBucketPolicyInputBuilder, DeletePublicAccessBlockInput,
    DeletePublicAccessBlockInputBuilder, GetBucketAclInput, GetBucketAclInputBuilder,
    GetBucketOwnershipControlsInput, GetBucketOwnershipControlsInputBuilder, GetBucketPolicyInput,
    GetBucketPolicyInputBuilder, GetBucketPolicyStatusInput, GetBucketPolicyStatusInputBuilder,
    GetPublicAccessBlockInput, GetPublicAccessBlockInputBuilder, PutBucketOwnershipControlsInput,
    PutBucketOwnershipControlsInputBuilder, PutBucketPolicyInput, PutBucketPolicyInputBuilder,
    PutPublicAccessBlockInput, PutPublicAccessBlockInputBuilder,
};
pub use object_acl::{GetObjectAclInput, GetObjectAclInputBuilder};
pub use values::{
    AclGrant, AclOwner, AclPermission, DeleteBucketPolicyOutput, DeletePublicAccessBlockOutput,
    GetBucketAclOutput, GetBucketOwnershipControlsOutput, GetBucketPolicyOutput,
    GetBucketPolicyStatusOutput, GetObjectAclOutput, GetPublicAccessBlockOutput, ObjectOwnership,
    PublicAccessBlockConfiguration, PutBucketOwnershipControlsOutput, PutBucketPolicyOutput,
    PutPublicAccessBlockOutput,
};

impl Store {
    /// Replace the complete bucket policy.
    /// Dropping this future cancels work that has not entered its commit section.
    ///
    /// # Errors
    /// Preserves missing-bucket, owner, lifecycle and storage failures.
    /// Once dispatched, mutation failures retain their authoritative outcome.
    pub async fn put_bucket_policy(
        &self,
        input: PutBucketPolicyInput,
    ) -> Result<PutBucketPolicyOutput, StorageError> {
        self.connection
            .put_bucket_policy(input.request, input.document)
            .await
            .map_err(StorageError::from_operation)?;

        Ok(PutBucketPolicyOutput)
    }

    /// Read the exact stored bucket policy.
    /// The owned result remains usable after this connection closes.
    ///
    /// # Errors
    /// An absent policy returns `NoSuchBucketPolicy`.
    /// Preserves missing-bucket, owner, lifecycle and storage failures.
    pub async fn get_bucket_policy(
        &self,
        input: GetBucketPolicyInput,
    ) -> Result<GetBucketPolicyOutput, StorageError> {
        let value = self
            .connection
            .get_bucket_policy(input.request)
            .await
            .map_err(StorageError::from_operation)?;

        Ok(GetBucketPolicyOutput {
            document: value.document,
        })
    }

    /// Delete the bucket policy idempotently.
    /// Dropping this future cancels work that has not entered its commit section.
    ///
    /// # Errors
    /// Preserves missing-bucket, owner, lifecycle and storage failures.
    /// Once dispatched, mutation failures retain their authoritative outcome.
    pub async fn delete_bucket_policy(
        &self,
        input: DeleteBucketPolicyInput,
    ) -> Result<DeleteBucketPolicyOutput, StorageError> {
        self.connection
            .delete_bucket_policy(input.request)
            .await
            .map_err(StorageError::from_operation)?;

        Ok(DeleteBucketPolicyOutput)
    }

    /// Read public-policy classification independently of access enforcement.
    /// The owned result remains usable after this connection closes.
    ///
    /// # Errors
    /// An absent policy returns `NoSuchBucketPolicy`.
    /// Preserves missing-bucket, owner, lifecycle and storage failures.
    pub async fn get_bucket_policy_status(
        &self,
        input: GetBucketPolicyStatusInput,
    ) -> Result<GetBucketPolicyStatusOutput, StorageError> {
        let is_public = self
            .connection
            .get_bucket_policy_status(input.request)
            .await
            .map_err(StorageError::from_operation)?;

        Ok(GetBucketPolicyStatusOutput { is_public })
    }

    /// Replace all four public-access-block flags.
    /// Dropping this future cancels work that has not entered its commit section.
    ///
    /// # Errors
    /// Preserves missing-bucket, owner, lifecycle and storage failures.
    /// Once dispatched, mutation failures retain their authoritative outcome.
    pub async fn put_public_access_block(
        &self,
        input: PutPublicAccessBlockInput,
    ) -> Result<PutPublicAccessBlockOutput, StorageError> {
        self.connection
            .put_public_access_block(input.request, input.configuration.into_core())
            .await
            .map_err(StorageError::from_operation)?;

        Ok(PutPublicAccessBlockOutput)
    }

    /// Read the present public-access-block configuration.
    /// The owned result remains usable after this connection closes.
    ///
    /// # Errors
    /// Absence returns `NoSuchPublicAccessBlockConfiguration`.
    /// Preserves missing-bucket, owner, lifecycle and storage failures.
    pub async fn get_public_access_block(
        &self,
        input: GetPublicAccessBlockInput,
    ) -> Result<GetPublicAccessBlockOutput, StorageError> {
        let flags = self
            .connection
            .get_public_access_block(input.request)
            .await
            .map_err(StorageError::from_operation)?;

        Ok(GetPublicAccessBlockOutput {
            configuration: PublicAccessBlockConfiguration::from_core(flags),
        })
    }

    /// Delete the public-access-block configuration idempotently.
    /// Dropping this future cancels work that has not entered its commit section.
    ///
    /// # Errors
    /// Preserves missing-bucket, owner, lifecycle and storage failures.
    /// Once dispatched, mutation failures retain their authoritative outcome.
    pub async fn delete_public_access_block(
        &self,
        input: DeletePublicAccessBlockInput,
    ) -> Result<DeletePublicAccessBlockOutput, StorageError> {
        self.connection
            .delete_public_access_block(input.request)
            .await
            .map_err(StorageError::from_operation)?;

        Ok(DeletePublicAccessBlockOutput)
    }

    /// Assert the fixed bucket-owner-enforced ownership model.
    /// Dropping this future cancels work that has not entered its commit section.
    ///
    /// # Errors
    /// Preserves missing-bucket, owner, lifecycle and storage failures.
    /// Once dispatched, mutation failures retain their authoritative outcome.
    pub async fn put_bucket_ownership_controls(
        &self,
        input: PutBucketOwnershipControlsInput,
    ) -> Result<PutBucketOwnershipControlsOutput, StorageError> {
        self.connection
            .put_bucket_ownership_controls(input.request)
            .await
            .map_err(StorageError::from_operation)?;

        Ok(PutBucketOwnershipControlsOutput)
    }

    /// Read the fixed bucket-owner-enforced ownership model.
    /// The owned result remains usable after this connection closes.
    ///
    /// # Errors
    /// Preserves missing-bucket, owner, lifecycle and storage failures.
    pub async fn get_bucket_ownership_controls(
        &self,
        input: GetBucketOwnershipControlsInput,
    ) -> Result<GetBucketOwnershipControlsOutput, StorageError> {
        self.connection
            .get_bucket_ownership_controls(input.request)
            .await
            .map_err(StorageError::from_operation)?;

        Ok(GetBucketOwnershipControlsOutput {
            object_ownership: ObjectOwnership::BucketOwnerEnforced,
        })
    }

    /// Read the canonical owner and its full-control grant.
    /// The owned result remains usable after this connection closes.
    ///
    /// # Errors
    /// Preserves missing-bucket, owner, lifecycle and storage failures.
    pub async fn get_bucket_acl(
        &self,
        input: GetBucketAclInput,
    ) -> Result<GetBucketAclOutput, StorageError> {
        self.connection
            .get_bucket_acl(input.request)
            .await
            .map(|owner| GetBucketAclOutput::from_core(&owner))
            .map_err(StorageError::from_operation)
    }
}
