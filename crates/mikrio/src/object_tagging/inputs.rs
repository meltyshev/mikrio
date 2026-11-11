//! Validated object identity and whole tag sets; construction performs no I/O.

use mikrio_core::object::{ObjectTags, ReadConditions, ReadRequest, Representation};

use crate::{ObjectRef, StorageError, Tag};

fn build_request(
    bucket: &str,
    key: &str,
    expected_owner: Option<String>,
) -> Result<ReadRequest, StorageError> {
    let (bucket, key) = ObjectRef::new(bucket, key)?.into_core();

    if let Some(owner) = &expected_owner {
        mikrio_core::access::validate_account_id(owner).map_err(StorageError::from_identity)?;
    }

    Ok(ReadRequest {
        bucket,
        key,
        expected_owner,
        conditions: ReadConditions::default(),
        range: None,
        part_number: None,
        response: Representation::default(),
    })
}

/// Owned values for [`Store::get_object_tagging`](crate::Store::get_object_tagging).
#[derive(Clone)]
pub struct GetObjectTaggingInput {
    pub(super) request: ReadRequest,
}

impl GetObjectTaggingInput {
    /// Start a builder with all required values.
    #[must_use]
    pub fn builder(
        bucket: impl Into<String>,
        key: impl Into<String>,
    ) -> GetObjectTaggingInputBuilder {
        GetObjectTaggingInputBuilder {
            bucket: bucket.into(),
            key: key.into(),
            expected_bucket_owner: None,
        }
    }

    /// Exact validated bucket name.
    #[must_use]
    pub fn bucket(&self) -> &str {
        self.request.bucket.as_str()
    }

    /// Exact validated object key, without path normalization or URL encoding.
    #[must_use]
    pub fn key(&self) -> &str {
        self.request.key.as_str()
    }

    /// Optional twelve-digit account assertion; it grants no permission.
    #[must_use]
    pub fn expected_bucket_owner(&self) -> Option<&str> {
        self.request.expected_owner.as_deref()
    }
}

impl std::fmt::Debug for GetObjectTaggingInput {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("GetObjectTaggingInput")
            .field("bucket", &self.bucket())
            .field("key", &self.key())
            .field("expected_bucket_owner", &self.expected_bucket_owner())
            .finish_non_exhaustive()
    }
}

/// Builder for [`GetObjectTaggingInput`]. Validation occurs in [`Self::build`].
#[derive(Clone, Debug)]
pub struct GetObjectTaggingInputBuilder {
    bucket: String,
    key: String,
    expected_bucket_owner: Option<String>,
}

impl GetObjectTaggingInputBuilder {
    /// Require the bucket to belong to this twelve-digit account.
    #[must_use]
    pub fn expected_bucket_owner(mut self, account: impl Into<String>) -> Self {
        self.expected_bucket_owner = Some(account.into());
        self
    }

    /// Validate and own inputs without accessing a store.
    ///
    /// # Errors
    /// Returns `InvalidBucketName` for an invalid bucket or `InvalidArgument`
    /// for an invalid key or owner assertion.
    pub fn build(self) -> Result<GetObjectTaggingInput, StorageError> {
        let request = build_request(&self.bucket, &self.key, self.expected_bucket_owner)?;
        Ok(GetObjectTaggingInput { request })
    }
}

/// Owned values for [`Store::put_object_tagging`](crate::Store::put_object_tagging).
#[derive(Clone)]
pub struct PutObjectTaggingInput {
    pub(super) request: ReadRequest,
    pub(super) tags: ObjectTags,
}

impl PutObjectTaggingInput {
    /// Start a builder with all required values.
    #[must_use]
    pub fn builder(
        bucket: impl Into<String>,
        key: impl Into<String>,
        tags: Vec<Tag>,
    ) -> PutObjectTaggingInputBuilder {
        PutObjectTaggingInputBuilder {
            bucket: bucket.into(),
            key: key.into(),
            expected_bucket_owner: None,
            tags,
        }
    }

    /// Exact validated bucket name.
    #[must_use]
    pub fn bucket(&self) -> &str {
        self.request.bucket.as_str()
    }

    /// Exact validated object key, without path normalization or URL encoding.
    #[must_use]
    pub fn key(&self) -> &str {
        self.request.key.as_str()
    }

    /// Optional twelve-digit account assertion; it grants no permission.
    #[must_use]
    pub fn expected_bucket_owner(&self) -> Option<&str> {
        self.request.expected_owner.as_deref()
    }

    /// Complete replacement set in ascending UTF-8 key order.
    #[must_use]
    pub fn tags(&self) -> Vec<Tag> {
        self.tags
            .entries()
            .iter()
            .map(|(key, value)| Tag {
                key: key.clone(),
                value: value.clone(),
            })
            .collect()
    }
}

impl std::fmt::Debug for PutObjectTaggingInput {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("PutObjectTaggingInput")
            .field("bucket", &self.bucket())
            .field("key", &self.key())
            .field("expected_bucket_owner", &self.expected_bucket_owner())
            .field("tags", &self.tags)
            .finish_non_exhaustive()
    }
}

/// Builder for [`PutObjectTaggingInput`]. Validation occurs in [`Self::build`].
#[derive(Clone, Debug)]
pub struct PutObjectTaggingInputBuilder {
    bucket: String,
    key: String,
    expected_bucket_owner: Option<String>,
    tags: Vec<Tag>,
}

impl PutObjectTaggingInputBuilder {
    /// Require the bucket to belong to this twelve-digit account.
    #[must_use]
    pub fn expected_bucket_owner(mut self, account: impl Into<String>) -> Self {
        self.expected_bucket_owner = Some(account.into());
        self
    }

    /// Validate and own inputs without accessing a store.
    ///
    /// # Errors
    /// Returns `InvalidBucketName` for an invalid bucket or `InvalidArgument`
    /// for an invalid key, owner assertion or complete tag set.
    pub fn build(self) -> Result<PutObjectTaggingInput, StorageError> {
        let request = build_request(&self.bucket, &self.key, self.expected_bucket_owner)?;

        let tags = ObjectTags::new(self.tags.into_iter().map(|tag| (tag.key, tag.value)))
            .map_err(StorageError::from_object)?;

        Ok(PutObjectTaggingInput { request, tags })
    }
}

/// Owned values for [`Store::delete_object_tagging`](crate::Store::delete_object_tagging).
#[derive(Clone)]
pub struct DeleteObjectTaggingInput {
    pub(super) request: ReadRequest,
}

impl DeleteObjectTaggingInput {
    /// Start a builder with all required values.
    #[must_use]
    pub fn builder(
        bucket: impl Into<String>,
        key: impl Into<String>,
    ) -> DeleteObjectTaggingInputBuilder {
        DeleteObjectTaggingInputBuilder {
            bucket: bucket.into(),
            key: key.into(),
            expected_bucket_owner: None,
        }
    }

    /// Exact validated bucket name.
    #[must_use]
    pub fn bucket(&self) -> &str {
        self.request.bucket.as_str()
    }

    /// Exact validated object key, without path normalization or URL encoding.
    #[must_use]
    pub fn key(&self) -> &str {
        self.request.key.as_str()
    }

    /// Optional twelve-digit account assertion; it grants no permission.
    #[must_use]
    pub fn expected_bucket_owner(&self) -> Option<&str> {
        self.request.expected_owner.as_deref()
    }
}

impl std::fmt::Debug for DeleteObjectTaggingInput {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("DeleteObjectTaggingInput")
            .field("bucket", &self.bucket())
            .field("key", &self.key())
            .field("expected_bucket_owner", &self.expected_bucket_owner())
            .finish_non_exhaustive()
    }
}

/// Builder for [`DeleteObjectTaggingInput`]. Validation occurs in [`Self::build`].
#[derive(Clone, Debug)]
pub struct DeleteObjectTaggingInputBuilder {
    bucket: String,
    key: String,
    expected_bucket_owner: Option<String>,
}

impl DeleteObjectTaggingInputBuilder {
    /// Require the bucket to belong to this twelve-digit account.
    #[must_use]
    pub fn expected_bucket_owner(mut self, account: impl Into<String>) -> Self {
        self.expected_bucket_owner = Some(account.into());
        self
    }

    /// Validate and own inputs without accessing a store.
    ///
    /// # Errors
    /// Returns `InvalidBucketName` for an invalid bucket or `InvalidArgument`
    /// for an invalid key or owner assertion.
    pub fn build(self) -> Result<DeleteObjectTaggingInput, StorageError> {
        let request = build_request(&self.bucket, &self.key, self.expected_bucket_owner)?;
        Ok(DeleteObjectTaggingInput { request })
    }
}
