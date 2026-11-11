//! Owned bucket operation values with shared name and owner validation.

use mikrio_core::object::BucketName;

use crate::{StorageError, Store};

/// Validated values for creating a bucket. Construction performs no I/O.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CreateBucketInput {
    bucket: BucketName,
}

impl CreateBucketInput {
    /// Start an owned input builder with the required literal bucket name.
    #[must_use]
    pub fn builder(bucket: impl Into<String>) -> CreateBucketInputBuilder {
        CreateBucketInputBuilder {
            bucket: bucket.into(),
        }
    }

    /// Exact validated bucket name, without case conversion.
    #[must_use]
    pub fn bucket(&self) -> &str {
        self.bucket.as_str()
    }
}

/// Builder for [`CreateBucketInput`]. Validation occurs in [`Self::build`].
#[derive(Debug, Clone)]
pub struct CreateBucketInputBuilder {
    bucket: String,
}

impl CreateBucketInputBuilder {
    /// Validate and own the operation's values without opening a store.
    ///
    /// # Errors
    /// Returns `InvalidBucketName` for an invalid bucket name.
    pub fn build(self) -> Result<CreateBucketInput, StorageError> {
        let bucket = BucketName::parse(&self.bucket).map_err(StorageError::from_object)?;

        Ok(CreateBucketInput { bucket })
    }
}

/// Validated values for inspecting bucket existence and region.
/// Construction performs no I/O.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HeadBucketInput {
    bucket: BucketName,
    expected_bucket_owner: Option<String>,
}

impl HeadBucketInput {
    /// Start an owned input builder with the required literal bucket name.
    #[must_use]
    pub fn builder(bucket: impl Into<String>) -> HeadBucketInputBuilder {
        HeadBucketInputBuilder {
            bucket: bucket.into(),
            expected_bucket_owner: None,
        }
    }

    /// Exact validated bucket name, without case conversion.
    #[must_use]
    pub fn bucket(&self) -> &str {
        self.bucket.as_str()
    }

    /// Optional account assertion; it supplies no authorization.
    #[must_use]
    pub fn expected_bucket_owner(&self) -> Option<&str> {
        self.expected_bucket_owner.as_deref()
    }
}

/// Builder for [`HeadBucketInput`]. Validation occurs in [`Self::build`].
#[derive(Debug, Clone)]
pub struct HeadBucketInputBuilder {
    bucket: String,
    expected_bucket_owner: Option<String>,
}

impl HeadBucketInputBuilder {
    /// Require the operation's bucket to belong to this twelve-digit account.
    #[must_use]
    pub fn expected_bucket_owner(mut self, account: impl Into<String>) -> Self {
        self.expected_bucket_owner = Some(account.into());
        self
    }

    /// Validate and own the operation's values without opening a store.
    ///
    /// # Errors
    /// Returns `InvalidBucketName` for an invalid name, or `InvalidArgument`
    /// for a malformed owner assertion.
    pub fn build(self) -> Result<HeadBucketInput, StorageError> {
        let bucket = BucketName::parse(&self.bucket).map_err(StorageError::from_object)?;

        if let Some(account) = &self.expected_bucket_owner {
            mikrio_core::access::validate_account_id(account)
                .map_err(StorageError::from_identity)?;
        }

        Ok(HeadBucketInput {
            bucket,
            expected_bucket_owner: self.expected_bucket_owner,
        })
    }
}

/// Validated values for retiring an empty bucket namespace.
/// Construction performs no I/O.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DeleteBucketInput {
    bucket: BucketName,
    expected_bucket_owner: Option<String>,
}

impl DeleteBucketInput {
    /// Start an owned input builder with the required literal bucket name.
    #[must_use]
    pub fn builder(bucket: impl Into<String>) -> DeleteBucketInputBuilder {
        DeleteBucketInputBuilder {
            bucket: bucket.into(),
            expected_bucket_owner: None,
        }
    }

    /// Exact validated bucket name, without case conversion.
    #[must_use]
    pub fn bucket(&self) -> &str {
        self.bucket.as_str()
    }

    /// Optional account assertion; it supplies no authorization.
    #[must_use]
    pub fn expected_bucket_owner(&self) -> Option<&str> {
        self.expected_bucket_owner.as_deref()
    }
}

/// Builder for [`DeleteBucketInput`]. Validation occurs in [`Self::build`].
#[derive(Debug, Clone)]
pub struct DeleteBucketInputBuilder {
    bucket: String,
    expected_bucket_owner: Option<String>,
}

impl DeleteBucketInputBuilder {
    /// Require the operation's bucket to belong to this twelve-digit account.
    #[must_use]
    pub fn expected_bucket_owner(mut self, account: impl Into<String>) -> Self {
        self.expected_bucket_owner = Some(account.into());
        self
    }

    /// Validate and own the operation's values without opening a store.
    ///
    /// # Errors
    /// Returns `InvalidBucketName` for an invalid name, or `InvalidArgument`
    /// for a malformed owner assertion.
    pub fn build(self) -> Result<DeleteBucketInput, StorageError> {
        let bucket = BucketName::parse(&self.bucket).map_err(StorageError::from_object)?;

        if let Some(account) = &self.expected_bucket_owner {
            mikrio_core::access::validate_account_id(account)
                .map_err(StorageError::from_identity)?;
        }

        Ok(DeleteBucketInput {
            bucket,
            expected_bucket_owner: self.expected_bucket_owner,
        })
    }
}

/// Validated values for reading a bucket region. Construction performs no I/O.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GetBucketLocationInput {
    bucket: BucketName,
    expected_bucket_owner: Option<String>,
}

impl GetBucketLocationInput {
    /// Start an owned input builder with the required literal bucket name.
    #[must_use]
    pub fn builder(bucket: impl Into<String>) -> GetBucketLocationInputBuilder {
        GetBucketLocationInputBuilder {
            bucket: bucket.into(),
            expected_bucket_owner: None,
        }
    }

    /// Exact validated bucket name, without case conversion.
    #[must_use]
    pub fn bucket(&self) -> &str {
        self.bucket.as_str()
    }

    /// Optional account assertion; it supplies no authorization.
    #[must_use]
    pub fn expected_bucket_owner(&self) -> Option<&str> {
        self.expected_bucket_owner.as_deref()
    }
}

/// Builder for [`GetBucketLocationInput`]. Validation occurs in [`Self::build`].
#[derive(Debug, Clone)]
pub struct GetBucketLocationInputBuilder {
    bucket: String,
    expected_bucket_owner: Option<String>,
}

impl GetBucketLocationInputBuilder {
    /// Require the operation's bucket to belong to this twelve-digit account.
    #[must_use]
    pub fn expected_bucket_owner(mut self, account: impl Into<String>) -> Self {
        self.expected_bucket_owner = Some(account.into());
        self
    }

    /// Validate and own the operation's values without opening a store.
    ///
    /// # Errors
    /// Returns `InvalidBucketName` for an invalid name, or `InvalidArgument`
    /// for a malformed owner assertion.
    pub fn build(self) -> Result<GetBucketLocationInput, StorageError> {
        let bucket = BucketName::parse(&self.bucket).map_err(StorageError::from_object)?;

        if let Some(account) = &self.expected_bucket_owner {
            mikrio_core::access::validate_account_id(account)
                .map_err(StorageError::from_identity)?;
        }

        Ok(GetBucketLocationInput {
            bucket,
            expected_bucket_owner: self.expected_bucket_owner,
        })
    }
}

/// Successful creation acknowledgment, with no additional value fields.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub struct CreateBucketOutput;

/// Successful namespace-retirement acknowledgment, with no additional fields.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub struct DeleteBucketOutput;

/// Detached bucket region returned by a successful existence check.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HeadBucketOutput {
    pub(crate) bucket_region: String,
}

impl HeadBucketOutput {
    /// Canonical signing region, including `us-east-1`.
    #[must_use]
    pub fn bucket_region(&self) -> &str {
        &self.bucket_region
    }
}

/// Detached bucket location with the S3 empty-region convention.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GetBucketLocationOutput {
    pub(crate) location_constraint: Option<String>,
}

impl GetBucketLocationOutput {
    /// Absence denotes `us-east-1`; other regions retain their exact value.
    #[must_use]
    pub fn location_constraint(&self) -> Option<&str> {
        self.location_constraint.as_deref()
    }
}

impl Store {
    /// Create a bucket. Repeating creation succeeds in `us-east-1` and preserves
    /// the existing identity and configuration; other regions report a conflict.
    ///
    /// # Errors
    /// Returns namespace, lifecycle or storage failures with a mutation outcome.
    /// Dropping this future cancels work that has not entered its commit section.
    pub async fn create_bucket(
        &self,
        input: CreateBucketInput,
    ) -> Result<CreateBucketOutput, StorageError> {
        self.connection
            .create_bucket(input.bucket)
            .await
            .map_err(StorageError::from_operation)?;

        Ok(CreateBucketOutput)
    }

    /// Check a live bucket and return its canonical region from one snapshot.
    ///
    /// # Errors
    /// Returns `NoSuchBucket`, `AccessDenied` for an owner mismatch, or a
    /// lifecycle/storage error. This operation creates no body or payload guard.
    pub async fn head_bucket(
        &self,
        input: HeadBucketInput,
    ) -> Result<HeadBucketOutput, StorageError> {
        let bucket_region = self
            .connection
            .head_bucket(input.bucket, input.expected_bucket_owner)
            .await
            .map_err(StorageError::from_operation)?;

        Ok(HeadBucketOutput { bucket_region })
    }

    /// Read the bucket location, with absence representing `us-east-1`.
    ///
    /// # Errors
    /// Returns the same existence, owner and storage failures as `head_bucket`.
    pub async fn get_bucket_location(
        &self,
        input: GetBucketLocationInput,
    ) -> Result<GetBucketLocationOutput, StorageError> {
        let region = self
            .connection
            .head_bucket(input.bucket, input.expected_bucket_owner)
            .await
            .map_err(StorageError::from_operation)?;

        let location_constraint = (region != "us-east-1").then_some(region);

        Ok(GetBucketLocationOutput {
            location_constraint,
        })
    }

    /// Atomically retire an empty bucket namespace. Recreating the name allocates
    /// a new identity. Unfinished uploads do not block namespace retirement.
    ///
    /// # Errors
    /// Returns `BucketNotEmpty`, `NoSuchBucket`, owner/lifecycle/storage failures
    /// and the authoritative mutation outcome. No payload is unlinked here.
    pub async fn delete_bucket(
        &self,
        input: DeleteBucketInput,
    ) -> Result<DeleteBucketOutput, StorageError> {
        self.connection
            .delete_bucket(input.bucket, input.expected_bucket_owner)
            .await
            .map_err(StorageError::from_operation)?;

        Ok(DeleteBucketOutput)
    }
}
