//! Owned configuration inputs validate names and assertions before dispatch.

use mikrio_core::access::{BucketAccessRequest, Policy};
use mikrio_core::object::BucketName;

use super::{ObjectOwnership, PublicAccessBlockConfiguration};
use crate::StorageError;

fn build_request(
    bucket: &str,
    expected_owner: Option<String>,
) -> Result<BucketAccessRequest, StorageError> {
    let bucket = BucketName::parse(bucket).map_err(StorageError::from_object)?;

    if let Some(account) = &expected_owner {
        mikrio_core::access::validate_account_id(account).map_err(StorageError::from_identity)?;
    }

    Ok(BucketAccessRequest {
        bucket,
        expected_owner,
    })
}

/// Owned values for [`Store::put_bucket_policy`](crate::Store::put_bucket_policy). Construction performs no I/O.
#[derive(Clone)]
pub struct PutBucketPolicyInput {
    pub(super) request: BucketAccessRequest,
    pub(super) document: String,
}

impl PutBucketPolicyInput {
    /// Start a builder with all required values.
    #[must_use]
    pub fn builder(
        bucket: impl Into<String>,
        document: impl Into<String>,
    ) -> PutBucketPolicyInputBuilder {
        PutBucketPolicyInputBuilder {
            bucket: bucket.into(),
            expected_bucket_owner: None,
            document: document.into(),
        }
    }

    /// Exact validated bucket name.
    #[must_use]
    pub fn bucket(&self) -> &str {
        self.request.bucket.as_str()
    }

    /// Optional twelve-digit account assertion; it grants no permission.
    #[must_use]
    pub fn expected_bucket_owner(&self) -> Option<&str> {
        self.request.expected_owner.as_deref()
    }

    /// Exact UTF-8 policy document, without normalization.
    #[must_use]
    pub fn document(&self) -> &str {
        &self.document
    }
}

impl std::fmt::Debug for PutBucketPolicyInput {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("PutBucketPolicyInput")
            .field("bucket", &self.bucket())
            .finish_non_exhaustive()
    }
}

/// Builder for [`PutBucketPolicyInput`]. Validation occurs in [`Self::build`].
#[derive(Clone)]
pub struct PutBucketPolicyInputBuilder {
    bucket: String,
    expected_bucket_owner: Option<String>,
    document: String,
}

impl PutBucketPolicyInputBuilder {
    /// Require the bucket to belong to this twelve-digit account.
    #[must_use]
    pub fn expected_bucket_owner(mut self, account: impl Into<String>) -> Self {
        self.expected_bucket_owner = Some(account.into());
        self
    }

    /// Validate and own values without accessing a store.
    ///
    /// # Errors
    /// Returns `InvalidBucketName` for an invalid name or `InvalidArgument`
    /// for a malformed owner assertion.
    /// Policy envelope size, depth and duplicate members are checked here;
    /// account-dependent policy semantics are validated at execution.
    pub fn build(self) -> Result<PutBucketPolicyInput, StorageError> {
        let request = build_request(&self.bucket, self.expected_bucket_owner)?;

        Policy::validate_envelope(&self.document).map_err(StorageError::from_policy)?;

        Ok(PutBucketPolicyInput {
            request,
            document: self.document,
        })
    }
}

impl std::fmt::Debug for PutBucketPolicyInputBuilder {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("PutBucketPolicyInputBuilder")
            .field("bucket", &self.bucket)
            .finish_non_exhaustive()
    }
}

/// Owned values for [`Store::get_bucket_policy`](crate::Store::get_bucket_policy). Construction performs no I/O.
#[derive(Debug, Clone)]
pub struct GetBucketPolicyInput {
    pub(super) request: BucketAccessRequest,
}

impl GetBucketPolicyInput {
    /// Start a builder with all required values.
    #[must_use]
    pub fn builder(bucket: impl Into<String>) -> GetBucketPolicyInputBuilder {
        GetBucketPolicyInputBuilder {
            bucket: bucket.into(),
            expected_bucket_owner: None,
        }
    }

    /// Exact validated bucket name.
    #[must_use]
    pub fn bucket(&self) -> &str {
        self.request.bucket.as_str()
    }

    /// Optional twelve-digit account assertion; it grants no permission.
    #[must_use]
    pub fn expected_bucket_owner(&self) -> Option<&str> {
        self.request.expected_owner.as_deref()
    }
}

/// Builder for [`GetBucketPolicyInput`]. Validation occurs in [`Self::build`].
#[derive(Debug, Clone)]
pub struct GetBucketPolicyInputBuilder {
    bucket: String,
    expected_bucket_owner: Option<String>,
}

impl GetBucketPolicyInputBuilder {
    /// Require the bucket to belong to this twelve-digit account.
    #[must_use]
    pub fn expected_bucket_owner(mut self, account: impl Into<String>) -> Self {
        self.expected_bucket_owner = Some(account.into());
        self
    }

    /// Validate and own values without accessing a store.
    ///
    /// # Errors
    /// Returns `InvalidBucketName` for an invalid name or `InvalidArgument`
    /// for a malformed owner assertion.
    pub fn build(self) -> Result<GetBucketPolicyInput, StorageError> {
        let request = build_request(&self.bucket, self.expected_bucket_owner)?;

        Ok(GetBucketPolicyInput { request })
    }
}

/// Owned values for [`Store::delete_bucket_policy`](crate::Store::delete_bucket_policy). Construction performs no I/O.
#[derive(Debug, Clone)]
pub struct DeleteBucketPolicyInput {
    pub(super) request: BucketAccessRequest,
}

impl DeleteBucketPolicyInput {
    /// Start a builder with all required values.
    #[must_use]
    pub fn builder(bucket: impl Into<String>) -> DeleteBucketPolicyInputBuilder {
        DeleteBucketPolicyInputBuilder {
            bucket: bucket.into(),
            expected_bucket_owner: None,
        }
    }

    /// Exact validated bucket name.
    #[must_use]
    pub fn bucket(&self) -> &str {
        self.request.bucket.as_str()
    }

    /// Optional twelve-digit account assertion; it grants no permission.
    #[must_use]
    pub fn expected_bucket_owner(&self) -> Option<&str> {
        self.request.expected_owner.as_deref()
    }
}

/// Builder for [`DeleteBucketPolicyInput`]. Validation occurs in [`Self::build`].
#[derive(Debug, Clone)]
pub struct DeleteBucketPolicyInputBuilder {
    bucket: String,
    expected_bucket_owner: Option<String>,
}

impl DeleteBucketPolicyInputBuilder {
    /// Require the bucket to belong to this twelve-digit account.
    #[must_use]
    pub fn expected_bucket_owner(mut self, account: impl Into<String>) -> Self {
        self.expected_bucket_owner = Some(account.into());
        self
    }

    /// Validate and own values without accessing a store.
    ///
    /// # Errors
    /// Returns `InvalidBucketName` for an invalid name or `InvalidArgument`
    /// for a malformed owner assertion.
    pub fn build(self) -> Result<DeleteBucketPolicyInput, StorageError> {
        let request = build_request(&self.bucket, self.expected_bucket_owner)?;

        Ok(DeleteBucketPolicyInput { request })
    }
}

/// Owned values for [`Store::get_bucket_policy_status`](crate::Store::get_bucket_policy_status). Construction performs no I/O.
#[derive(Debug, Clone)]
pub struct GetBucketPolicyStatusInput {
    pub(super) request: BucketAccessRequest,
}

impl GetBucketPolicyStatusInput {
    /// Start a builder with all required values.
    #[must_use]
    pub fn builder(bucket: impl Into<String>) -> GetBucketPolicyStatusInputBuilder {
        GetBucketPolicyStatusInputBuilder {
            bucket: bucket.into(),
            expected_bucket_owner: None,
        }
    }

    /// Exact validated bucket name.
    #[must_use]
    pub fn bucket(&self) -> &str {
        self.request.bucket.as_str()
    }

    /// Optional twelve-digit account assertion; it grants no permission.
    #[must_use]
    pub fn expected_bucket_owner(&self) -> Option<&str> {
        self.request.expected_owner.as_deref()
    }
}

/// Builder for [`GetBucketPolicyStatusInput`]. Validation occurs in [`Self::build`].
#[derive(Debug, Clone)]
pub struct GetBucketPolicyStatusInputBuilder {
    bucket: String,
    expected_bucket_owner: Option<String>,
}

impl GetBucketPolicyStatusInputBuilder {
    /// Require the bucket to belong to this twelve-digit account.
    #[must_use]
    pub fn expected_bucket_owner(mut self, account: impl Into<String>) -> Self {
        self.expected_bucket_owner = Some(account.into());
        self
    }

    /// Validate and own values without accessing a store.
    ///
    /// # Errors
    /// Returns `InvalidBucketName` for an invalid name or `InvalidArgument`
    /// for a malformed owner assertion.
    pub fn build(self) -> Result<GetBucketPolicyStatusInput, StorageError> {
        let request = build_request(&self.bucket, self.expected_bucket_owner)?;

        Ok(GetBucketPolicyStatusInput { request })
    }
}

/// Owned values for [`Store::put_public_access_block`](crate::Store::put_public_access_block). Construction performs no I/O.
#[derive(Debug, Clone)]
pub struct PutPublicAccessBlockInput {
    pub(super) request: BucketAccessRequest,
    pub(super) configuration: PublicAccessBlockConfiguration,
}

impl PutPublicAccessBlockInput {
    /// Start a builder with all required values.
    #[must_use]
    pub fn builder(
        bucket: impl Into<String>,
        configuration: PublicAccessBlockConfiguration,
    ) -> PutPublicAccessBlockInputBuilder {
        PutPublicAccessBlockInputBuilder {
            bucket: bucket.into(),
            expected_bucket_owner: None,
            configuration,
        }
    }

    /// Exact validated bucket name.
    #[must_use]
    pub fn bucket(&self) -> &str {
        self.request.bucket.as_str()
    }

    /// Optional twelve-digit account assertion; it grants no permission.
    #[must_use]
    pub fn expected_bucket_owner(&self) -> Option<&str> {
        self.request.expected_owner.as_deref()
    }

    /// Complete replacement configuration, with all four flags explicit.
    #[must_use]
    pub fn configuration(&self) -> PublicAccessBlockConfiguration {
        self.configuration
    }
}

/// Builder for [`PutPublicAccessBlockInput`]. Validation occurs in [`Self::build`].
#[derive(Debug, Clone)]
pub struct PutPublicAccessBlockInputBuilder {
    bucket: String,
    expected_bucket_owner: Option<String>,
    configuration: PublicAccessBlockConfiguration,
}

impl PutPublicAccessBlockInputBuilder {
    /// Require the bucket to belong to this twelve-digit account.
    #[must_use]
    pub fn expected_bucket_owner(mut self, account: impl Into<String>) -> Self {
        self.expected_bucket_owner = Some(account.into());
        self
    }

    /// Validate and own values without accessing a store.
    ///
    /// # Errors
    /// Returns `InvalidBucketName` for an invalid name or `InvalidArgument`
    /// for a malformed owner assertion.
    pub fn build(self) -> Result<PutPublicAccessBlockInput, StorageError> {
        let request = build_request(&self.bucket, self.expected_bucket_owner)?;

        Ok(PutPublicAccessBlockInput {
            request,
            configuration: self.configuration,
        })
    }
}

/// Owned values for [`Store::get_public_access_block`](crate::Store::get_public_access_block). Construction performs no I/O.
#[derive(Debug, Clone)]
pub struct GetPublicAccessBlockInput {
    pub(super) request: BucketAccessRequest,
}

impl GetPublicAccessBlockInput {
    /// Start a builder with all required values.
    #[must_use]
    pub fn builder(bucket: impl Into<String>) -> GetPublicAccessBlockInputBuilder {
        GetPublicAccessBlockInputBuilder {
            bucket: bucket.into(),
            expected_bucket_owner: None,
        }
    }

    /// Exact validated bucket name.
    #[must_use]
    pub fn bucket(&self) -> &str {
        self.request.bucket.as_str()
    }

    /// Optional twelve-digit account assertion; it grants no permission.
    #[must_use]
    pub fn expected_bucket_owner(&self) -> Option<&str> {
        self.request.expected_owner.as_deref()
    }
}

/// Builder for [`GetPublicAccessBlockInput`]. Validation occurs in [`Self::build`].
#[derive(Debug, Clone)]
pub struct GetPublicAccessBlockInputBuilder {
    bucket: String,
    expected_bucket_owner: Option<String>,
}

impl GetPublicAccessBlockInputBuilder {
    /// Require the bucket to belong to this twelve-digit account.
    #[must_use]
    pub fn expected_bucket_owner(mut self, account: impl Into<String>) -> Self {
        self.expected_bucket_owner = Some(account.into());
        self
    }

    /// Validate and own values without accessing a store.
    ///
    /// # Errors
    /// Returns `InvalidBucketName` for an invalid name or `InvalidArgument`
    /// for a malformed owner assertion.
    pub fn build(self) -> Result<GetPublicAccessBlockInput, StorageError> {
        let request = build_request(&self.bucket, self.expected_bucket_owner)?;

        Ok(GetPublicAccessBlockInput { request })
    }
}

/// Owned values for [`Store::delete_public_access_block`](crate::Store::delete_public_access_block). Construction performs no I/O.
#[derive(Debug, Clone)]
pub struct DeletePublicAccessBlockInput {
    pub(super) request: BucketAccessRequest,
}

impl DeletePublicAccessBlockInput {
    /// Start a builder with all required values.
    #[must_use]
    pub fn builder(bucket: impl Into<String>) -> DeletePublicAccessBlockInputBuilder {
        DeletePublicAccessBlockInputBuilder {
            bucket: bucket.into(),
            expected_bucket_owner: None,
        }
    }

    /// Exact validated bucket name.
    #[must_use]
    pub fn bucket(&self) -> &str {
        self.request.bucket.as_str()
    }

    /// Optional twelve-digit account assertion; it grants no permission.
    #[must_use]
    pub fn expected_bucket_owner(&self) -> Option<&str> {
        self.request.expected_owner.as_deref()
    }
}

/// Builder for [`DeletePublicAccessBlockInput`]. Validation occurs in [`Self::build`].
#[derive(Debug, Clone)]
pub struct DeletePublicAccessBlockInputBuilder {
    bucket: String,
    expected_bucket_owner: Option<String>,
}

impl DeletePublicAccessBlockInputBuilder {
    /// Require the bucket to belong to this twelve-digit account.
    #[must_use]
    pub fn expected_bucket_owner(mut self, account: impl Into<String>) -> Self {
        self.expected_bucket_owner = Some(account.into());
        self
    }

    /// Validate and own values without accessing a store.
    ///
    /// # Errors
    /// Returns `InvalidBucketName` for an invalid name or `InvalidArgument`
    /// for a malformed owner assertion.
    pub fn build(self) -> Result<DeletePublicAccessBlockInput, StorageError> {
        let request = build_request(&self.bucket, self.expected_bucket_owner)?;

        Ok(DeletePublicAccessBlockInput { request })
    }
}

/// Owned values for [`Store::put_bucket_ownership_controls`](crate::Store::put_bucket_ownership_controls). Construction performs no I/O.
#[derive(Debug, Clone)]
pub struct PutBucketOwnershipControlsInput {
    pub(super) request: BucketAccessRequest,
    pub(super) object_ownership: ObjectOwnership,
}

impl PutBucketOwnershipControlsInput {
    /// Start a builder with all required values.
    #[must_use]
    pub fn builder(
        bucket: impl Into<String>,
        object_ownership: ObjectOwnership,
    ) -> PutBucketOwnershipControlsInputBuilder {
        PutBucketOwnershipControlsInputBuilder {
            bucket: bucket.into(),
            expected_bucket_owner: None,
            object_ownership,
        }
    }

    /// Exact validated bucket name.
    #[must_use]
    pub fn bucket(&self) -> &str {
        self.request.bucket.as_str()
    }

    /// Optional twelve-digit account assertion; it grants no permission.
    #[must_use]
    pub fn expected_bucket_owner(&self) -> Option<&str> {
        self.request.expected_owner.as_deref()
    }

    /// Required ownership model; ACL authorization remains disabled.
    #[must_use]
    pub fn object_ownership(&self) -> ObjectOwnership {
        self.object_ownership
    }
}

/// Builder for [`PutBucketOwnershipControlsInput`]. Validation occurs in [`Self::build`].
#[derive(Debug, Clone)]
pub struct PutBucketOwnershipControlsInputBuilder {
    bucket: String,
    expected_bucket_owner: Option<String>,
    object_ownership: ObjectOwnership,
}

impl PutBucketOwnershipControlsInputBuilder {
    /// Require the bucket to belong to this twelve-digit account.
    #[must_use]
    pub fn expected_bucket_owner(mut self, account: impl Into<String>) -> Self {
        self.expected_bucket_owner = Some(account.into());
        self
    }

    /// Validate and own values without accessing a store.
    ///
    /// # Errors
    /// Returns `InvalidBucketName` for an invalid name or `InvalidArgument`
    /// for a malformed owner assertion.
    pub fn build(self) -> Result<PutBucketOwnershipControlsInput, StorageError> {
        let request = build_request(&self.bucket, self.expected_bucket_owner)?;

        Ok(PutBucketOwnershipControlsInput {
            request,
            object_ownership: self.object_ownership,
        })
    }
}

/// Owned values for [`Store::get_bucket_ownership_controls`](crate::Store::get_bucket_ownership_controls). Construction performs no I/O.
#[derive(Debug, Clone)]
pub struct GetBucketOwnershipControlsInput {
    pub(super) request: BucketAccessRequest,
}

impl GetBucketOwnershipControlsInput {
    /// Start a builder with all required values.
    #[must_use]
    pub fn builder(bucket: impl Into<String>) -> GetBucketOwnershipControlsInputBuilder {
        GetBucketOwnershipControlsInputBuilder {
            bucket: bucket.into(),
            expected_bucket_owner: None,
        }
    }

    /// Exact validated bucket name.
    #[must_use]
    pub fn bucket(&self) -> &str {
        self.request.bucket.as_str()
    }

    /// Optional twelve-digit account assertion; it grants no permission.
    #[must_use]
    pub fn expected_bucket_owner(&self) -> Option<&str> {
        self.request.expected_owner.as_deref()
    }
}

/// Builder for [`GetBucketOwnershipControlsInput`]. Validation occurs in [`Self::build`].
#[derive(Debug, Clone)]
pub struct GetBucketOwnershipControlsInputBuilder {
    bucket: String,
    expected_bucket_owner: Option<String>,
}

impl GetBucketOwnershipControlsInputBuilder {
    /// Require the bucket to belong to this twelve-digit account.
    #[must_use]
    pub fn expected_bucket_owner(mut self, account: impl Into<String>) -> Self {
        self.expected_bucket_owner = Some(account.into());
        self
    }

    /// Validate and own values without accessing a store.
    ///
    /// # Errors
    /// Returns `InvalidBucketName` for an invalid name or `InvalidArgument`
    /// for a malformed owner assertion.
    pub fn build(self) -> Result<GetBucketOwnershipControlsInput, StorageError> {
        let request = build_request(&self.bucket, self.expected_bucket_owner)?;

        Ok(GetBucketOwnershipControlsInput { request })
    }
}

/// Owned values for [`Store::get_bucket_acl`](crate::Store::get_bucket_acl). Construction performs no I/O.
#[derive(Debug, Clone)]
pub struct GetBucketAclInput {
    pub(super) request: BucketAccessRequest,
}

impl GetBucketAclInput {
    /// Start a builder with all required values.
    #[must_use]
    pub fn builder(bucket: impl Into<String>) -> GetBucketAclInputBuilder {
        GetBucketAclInputBuilder {
            bucket: bucket.into(),
            expected_bucket_owner: None,
        }
    }

    /// Exact validated bucket name.
    #[must_use]
    pub fn bucket(&self) -> &str {
        self.request.bucket.as_str()
    }

    /// Optional twelve-digit account assertion; it grants no permission.
    #[must_use]
    pub fn expected_bucket_owner(&self) -> Option<&str> {
        self.request.expected_owner.as_deref()
    }
}

/// Builder for [`GetBucketAclInput`]. Validation occurs in [`Self::build`].
#[derive(Debug, Clone)]
pub struct GetBucketAclInputBuilder {
    bucket: String,
    expected_bucket_owner: Option<String>,
}

impl GetBucketAclInputBuilder {
    /// Require the bucket to belong to this twelve-digit account.
    #[must_use]
    pub fn expected_bucket_owner(mut self, account: impl Into<String>) -> Self {
        self.expected_bucket_owner = Some(account.into());
        self
    }

    /// Validate and own values without accessing a store.
    ///
    /// # Errors
    /// Returns `InvalidBucketName` for an invalid name or `InvalidArgument`
    /// for a malformed owner assertion.
    pub fn build(self) -> Result<GetBucketAclInput, StorageError> {
        let request = build_request(&self.bucket, self.expected_bucket_owner)?;

        Ok(GetBucketAclInput { request })
    }
}
