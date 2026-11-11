//! Owned object identities and deletion values.

use mikrio_core::object::{BucketName, ObjectKey};

use crate::StorageError;

/// A literal object identity, independent of local paths and HTTP encoding.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ObjectRef {
    bucket: BucketName,
    key: ObjectKey,
}

impl ObjectRef {
    /// Validate and own a bucket/key pair without accessing storage.
    ///
    /// # Errors
    /// Returns `InvalidBucketName` for an invalid bucket or `InvalidArgument`
    /// for an empty, overlong or unbalanced key.
    pub fn new(bucket: &str, key: &str) -> Result<Self, StorageError> {
        Ok(Self {
            bucket: BucketName::parse(bucket).map_err(StorageError::from_object)?,
            key: ObjectKey::parse(key).map_err(StorageError::from_object)?,
        })
    }

    /// Exact bucket name, without case conversion.
    #[must_use]
    pub fn bucket(&self) -> &str {
        self.bucket.as_str()
    }

    /// Exact application key, without percent encoding or path normalization.
    #[must_use]
    pub fn key(&self) -> &str {
        self.key.as_str()
    }

    pub(crate) fn into_core(self) -> (BucketName, ObjectKey) {
        (self.bucket, self.key)
    }
}

/// Validated identity, owner assertion and optional atomic delete condition.
/// Construction performs no I/O and establishes no mutation outcome.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DeleteObjectInput {
    object: ObjectRef,
    expected_bucket_owner: Option<String>,
    if_match: Option<String>,
}

impl DeleteObjectInput {
    /// Start a builder with the required literal identity.
    #[must_use]
    pub fn builder(bucket: impl Into<String>, key: impl Into<String>) -> DeleteObjectInputBuilder {
        DeleteObjectInputBuilder {
            bucket: bucket.into(),
            key: key.into(),
            expected_bucket_owner: None,
            if_match: None,
        }
    }

    /// Exact validated bucket name.
    #[must_use]
    pub fn bucket(&self) -> &str {
        self.object.bucket()
    }

    /// Exact validated object key.
    #[must_use]
    pub fn key(&self) -> &str {
        self.object.key()
    }

    /// Optional account assertion; it supplies no authorization.
    #[must_use]
    pub fn expected_bucket_owner(&self) -> Option<&str> {
        self.expected_bucket_owner.as_deref()
    }

    /// Quoted `ETag` or `*` for existence; absence is unconditional.
    #[must_use]
    pub fn if_match(&self) -> Option<&str> {
        self.if_match.as_deref()
    }
}

/// Builder for [`DeleteObjectInput`]. Validation occurs in [`Self::build`].
#[derive(Debug, Clone)]
pub struct DeleteObjectInputBuilder {
    bucket: String,
    key: String,
    expected_bucket_owner: Option<String>,
    if_match: Option<String>,
}

impl DeleteObjectInputBuilder {
    /// Assert ownership by this twelve-digit account, preserving leading zeroes.
    #[must_use]
    pub fn expected_bucket_owner(mut self, account: impl Into<String>) -> Self {
        self.expected_bucket_owner = Some(account.into());
        self
    }

    /// Require a matching quoted `ETag`, or use `*` to require an existing object.
    #[must_use]
    pub fn if_match(mut self, etag: impl Into<String>) -> Self {
        self.if_match = Some(etag.into());
        self
    }

    /// Validate the identity and optional assertions without opening a store.
    ///
    /// # Errors
    /// Returns `InvalidBucketName` for an invalid bucket, or `InvalidArgument`
    /// for an invalid key, account assertion or entity-tag condition.
    pub fn build(self) -> Result<DeleteObjectInput, StorageError> {
        let object = ObjectRef::new(&self.bucket, &self.key)?;

        if let Some(account) = &self.expected_bucket_owner {
            mikrio_core::access::validate_account_id(account)
                .map_err(StorageError::from_identity)?;
        }

        if let Some(condition) = &self.if_match {
            mikrio_core::object::validate_entity_tag_condition(condition)
                .map_err(StorageError::from_object)?;
        }

        Ok(DeleteObjectInput {
            object,
            expected_bucket_owner: self.expected_bucket_owner,
            if_match: self.if_match,
        })
    }
}

/// Successful deletion acknowledgment, with no additional value fields.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub struct DeleteObjectOutput;

impl crate::Store {
    /// Remove the current mapping while existing readers retain their generation.
    ///
    /// # Errors
    /// Reports missing buckets, failed conditions, owner assertions and storage
    /// failures. Unconditional absence succeeds; failures carry mutation outcome.
    pub async fn delete_object(
        &self,
        input: DeleteObjectInput,
    ) -> Result<DeleteObjectOutput, StorageError> {
        let (bucket, key) = input.object.into_core();

        let request = mikrio_core::object::ReadRequest {
            bucket,
            key,
            expected_owner: input.expected_bucket_owner,
            conditions: mikrio_core::object::ReadConditions::default(),
            range: None,
            part_number: None,
            response: mikrio_core::object::Representation::default(),
        };

        self.connection
            .delete_object(request, input.if_match)
            .await
            .map_err(StorageError::from_operation)?;

        Ok(DeleteObjectOutput)
    }
}
