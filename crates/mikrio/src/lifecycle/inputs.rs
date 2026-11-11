//! Owned configuration inputs validate names and assertions before dispatch.

use mikrio_core::access::BucketAccessRequest;
use mikrio_core::lifecycle::LifecycleConfiguration;
use mikrio_core::object::BucketName;

use super::LifecycleRule;
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

/// Owned values for [`Store::put_bucket_lifecycle_configuration`](crate::Store::put_bucket_lifecycle_configuration). Construction performs no I/O.
#[derive(Clone, Debug)]
pub struct PutBucketLifecycleConfigurationInput {
    pub(super) request: BucketAccessRequest,
    pub(super) configuration: LifecycleConfiguration,
}

impl PutBucketLifecycleConfigurationInput {
    /// Start a builder with all required values.
    #[must_use]
    pub fn builder(
        bucket: impl Into<String>,
        lifecycle_rules: Vec<LifecycleRule>,
    ) -> PutBucketLifecycleConfigurationInputBuilder {
        PutBucketLifecycleConfigurationInputBuilder {
            bucket: bucket.into(),
            expected_bucket_owner: None,
            lifecycle_rules,
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

    /// Complete validated lifecycle configuration in evaluation order.
    #[must_use]
    pub fn lifecycle_rules(&self) -> Vec<LifecycleRule> {
        self.configuration
            .rules()
            .iter()
            .cloned()
            .map(LifecycleRule::from_core)
            .collect()
    }
}

/// Builder for [`PutBucketLifecycleConfigurationInput`]. Validation occurs in [`Self::build`].
#[derive(Clone, Debug)]
pub struct PutBucketLifecycleConfigurationInputBuilder {
    bucket: String,
    expected_bucket_owner: Option<String>,
    lifecycle_rules: Vec<LifecycleRule>,
}

impl PutBucketLifecycleConfigurationInputBuilder {
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
    /// Every rule and the complete configuration rule bound are validated here.
    pub fn build(self) -> Result<PutBucketLifecycleConfigurationInput, StorageError> {
        let request = build_request(&self.bucket, self.expected_bucket_owner)?;

        let configuration = LifecycleConfiguration::new(
            self.lifecycle_rules
                .into_iter()
                .map(LifecycleRule::into_core)
                .collect(),
        )
        .map_err(super::invalid)?;

        Ok(PutBucketLifecycleConfigurationInput {
            request,
            configuration,
        })
    }
}

/// Owned values for [`Store::get_bucket_lifecycle_configuration`](crate::Store::get_bucket_lifecycle_configuration). Construction performs no I/O.
#[derive(Debug, Clone)]
pub struct GetBucketLifecycleConfigurationInput {
    pub(super) request: BucketAccessRequest,
}

impl GetBucketLifecycleConfigurationInput {
    /// Start a builder with all required values.
    #[must_use]
    pub fn builder(bucket: impl Into<String>) -> GetBucketLifecycleConfigurationInputBuilder {
        GetBucketLifecycleConfigurationInputBuilder {
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

/// Builder for [`GetBucketLifecycleConfigurationInput`]. Validation occurs in [`Self::build`].
#[derive(Debug, Clone)]
pub struct GetBucketLifecycleConfigurationInputBuilder {
    bucket: String,
    expected_bucket_owner: Option<String>,
}

impl GetBucketLifecycleConfigurationInputBuilder {
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
    pub fn build(self) -> Result<GetBucketLifecycleConfigurationInput, StorageError> {
        let request = build_request(&self.bucket, self.expected_bucket_owner)?;

        Ok(GetBucketLifecycleConfigurationInput { request })
    }
}

/// Owned values for [`Store::delete_bucket_lifecycle`](crate::Store::delete_bucket_lifecycle). Construction performs no I/O.
#[derive(Debug, Clone)]
pub struct DeleteBucketLifecycleInput {
    pub(super) request: BucketAccessRequest,
}

impl DeleteBucketLifecycleInput {
    /// Start a builder with all required values.
    #[must_use]
    pub fn builder(bucket: impl Into<String>) -> DeleteBucketLifecycleInputBuilder {
        DeleteBucketLifecycleInputBuilder {
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

/// Builder for [`DeleteBucketLifecycleInput`]. Validation occurs in [`Self::build`].
#[derive(Debug, Clone)]
pub struct DeleteBucketLifecycleInputBuilder {
    bucket: String,
    expected_bucket_owner: Option<String>,
}

impl DeleteBucketLifecycleInputBuilder {
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
    pub fn build(self) -> Result<DeleteBucketLifecycleInput, StorageError> {
        let request = build_request(&self.bucket, self.expected_bucket_owner)?;

        Ok(DeleteBucketLifecycleInput { request })
    }
}
