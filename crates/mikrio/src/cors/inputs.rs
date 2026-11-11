//! Owned configuration inputs validate names and assertions before dispatch.

use mikrio_core::access::BucketAccessRequest;
use mikrio_core::cors::CorsConfiguration;
use mikrio_core::object::BucketName;

use super::CorsRule;
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

/// Owned values for [`Store::put_bucket_cors`](crate::Store::put_bucket_cors). Construction performs no I/O.
#[derive(Clone, Debug)]
pub struct PutBucketCorsInput {
    pub(super) request: BucketAccessRequest,
    pub(super) configuration: CorsConfiguration,
}

impl PutBucketCorsInput {
    /// Start a builder with all required values.
    #[must_use]
    pub fn builder(
        bucket: impl Into<String>,
        cors_rules: Vec<CorsRule>,
    ) -> PutBucketCorsInputBuilder {
        PutBucketCorsInputBuilder {
            bucket: bucket.into(),
            expected_bucket_owner: None,
            cors_rules,
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

    /// Complete validated CORS configuration in evaluation order.
    #[must_use]
    pub fn cors_rules(&self) -> Vec<CorsRule> {
        self.configuration
            .rules()
            .iter()
            .cloned()
            .map(CorsRule::from_core)
            .collect()
    }
}

/// Builder for [`PutBucketCorsInput`]. Validation occurs in [`Self::build`].
#[derive(Clone, Debug)]
pub struct PutBucketCorsInputBuilder {
    bucket: String,
    expected_bucket_owner: Option<String>,
    cors_rules: Vec<CorsRule>,
}

impl PutBucketCorsInputBuilder {
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
    /// Every rule and the complete configuration byte budget are validated here.
    pub fn build(self) -> Result<PutBucketCorsInput, StorageError> {
        let request = build_request(&self.bucket, self.expected_bucket_owner)?;

        let configuration = CorsConfiguration::new(
            self.cors_rules
                .into_iter()
                .map(CorsRule::into_core)
                .collect(),
        )
        .map_err(super::invalid)?;

        Ok(PutBucketCorsInput {
            request,
            configuration,
        })
    }
}

/// Owned values for [`Store::get_bucket_cors`](crate::Store::get_bucket_cors). Construction performs no I/O.
#[derive(Debug, Clone)]
pub struct GetBucketCorsInput {
    pub(super) request: BucketAccessRequest,
}

impl GetBucketCorsInput {
    /// Start a builder with all required values.
    #[must_use]
    pub fn builder(bucket: impl Into<String>) -> GetBucketCorsInputBuilder {
        GetBucketCorsInputBuilder {
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

/// Builder for [`GetBucketCorsInput`]. Validation occurs in [`Self::build`].
#[derive(Debug, Clone)]
pub struct GetBucketCorsInputBuilder {
    bucket: String,
    expected_bucket_owner: Option<String>,
}

impl GetBucketCorsInputBuilder {
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
    pub fn build(self) -> Result<GetBucketCorsInput, StorageError> {
        let request = build_request(&self.bucket, self.expected_bucket_owner)?;

        Ok(GetBucketCorsInput { request })
    }
}

/// Owned values for [`Store::delete_bucket_cors`](crate::Store::delete_bucket_cors). Construction performs no I/O.
#[derive(Debug, Clone)]
pub struct DeleteBucketCorsInput {
    pub(super) request: BucketAccessRequest,
}

impl DeleteBucketCorsInput {
    /// Start a builder with all required values.
    #[must_use]
    pub fn builder(bucket: impl Into<String>) -> DeleteBucketCorsInputBuilder {
        DeleteBucketCorsInputBuilder {
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

/// Builder for [`DeleteBucketCorsInput`]. Validation occurs in [`Self::build`].
#[derive(Debug, Clone)]
pub struct DeleteBucketCorsInputBuilder {
    bucket: String,
    expected_bucket_owner: Option<String>,
}

impl DeleteBucketCorsInputBuilder {
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
    pub fn build(self) -> Result<DeleteBucketCorsInput, StorageError> {
        let request = build_request(&self.bucket, self.expected_bucket_owner)?;

        Ok(DeleteBucketCorsInput { request })
    }
}
