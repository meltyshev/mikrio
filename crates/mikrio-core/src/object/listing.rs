use super::{BucketName, ObjectValueError};
use crate::EpochMillis;

/// Owned request for one exact-byte object namespace page.
#[derive(Debug, Clone)]
pub struct ListObjectsRequest {
    pub bucket: BucketName,
    pub expected_owner: Option<String>,
    pub prefix: Option<String>,
    pub delimiter: Option<String>,
    pub start_after: Option<String>,
    pub continuation_token: Option<String>,
    pub max_keys: ObjectPageSize,
}

impl ListObjectsRequest {
    /// Validate the token envelope bound and optional owner assertion.
    /// Literal filters are not object keys and may exceed the key byte limit.
    ///
    /// # Errors
    /// Rejects an oversized token or a malformed owner assertion.
    pub fn validate(&self) -> Result<(), crate::OperationError> {
        if self
            .continuation_token
            .as_ref()
            .is_some_and(|token| token.len() > 2048)
        {
            return Err(crate::OperationError::new(
                crate::OperationFailure::InvalidArgument,
            ));
        }

        if self
            .expected_owner
            .as_ref()
            .is_some_and(|owner| crate::access::validate_account_id(owner).is_err())
        {
            return Err(crate::OperationError::new(
                crate::OperationFailure::InvalidArgument,
            ));
        }

        Ok(())
    }
}

/// Detached fields selected with the logical object and its ready payload.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ListedObject {
    pub key: String,
    pub size: u64,
    pub etag: String,
    pub last_modified: EpochMillis,
}

/// One consistent page; subsequent requests observe a new live snapshot.
#[derive(Debug, Default)]
pub struct ObjectPage {
    pub contents: Vec<ListedObject>,
    pub common_prefixes: Vec<String>,
    pub next_continuation_token: Option<String>,
}

/// A native object-list page bound, including an explicit zero-sized request.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ObjectPageSize(u32);

impl ObjectPageSize {
    /// # Errors
    /// Rejects values above 1,000 rather than silently clamping them.
    pub fn new(value: u32) -> Result<Self, ObjectValueError> {
        if value > 1000 {
            return Err(ObjectValueError::PageSize);
        }

        Ok(Self(value))
    }

    #[must_use]
    pub const fn get(self) -> u32 {
        self.0
    }
}

impl Default for ObjectPageSize {
    fn default() -> Self {
        Self(1000)
    }
}

/// One account-wide bucket request. Only an option-free HTTP request may
/// select the unpaginated form; native facade calls always provide a page bound.
#[derive(Debug, Clone)]
pub struct ListBucketsRequest {
    pub prefix: Option<String>,
    pub continuation_token: Option<String>,
    pub max_buckets: Option<BucketPageSize>,
    pub bucket_region: Option<String>,
}

impl ListBucketsRequest {
    /// Validate the bounded token envelope and the unpaginated request form.
    ///
    /// # Errors
    /// Returns invalid argument for oversized tokens or inconsistent pagination.
    pub fn validate(&self) -> Result<(), crate::OperationError> {
        if self
            .continuation_token
            .as_ref()
            .is_some_and(|token| token.len() > 1024)
        {
            return Err(crate::OperationError::new(
                crate::OperationFailure::InvalidArgument,
            ));
        }

        if self.max_buckets.is_none()
            && (self.prefix.is_some()
                || self.continuation_token.is_some()
                || self.bucket_region.is_some())
        {
            return Err(crate::OperationError::new(
                crate::OperationFailure::InvalidArgument,
            ));
        }

        Ok(())
    }
}

/// Detached live bucket facts selected in one metadata snapshot.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ListedBucket {
    pub name: String,
    pub creation_date: EpochMillis,
}

/// The page and its store facts share one authorized read snapshot.
#[derive(Debug)]
pub struct BucketPage {
    pub buckets: Vec<ListedBucket>,
    pub continuation_token: Option<String>,
    pub bucket_region: String,
    pub canonical_owner_id: String,
}

/// A native bucket-list page bound, distinct from object-list bounds.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct BucketPageSize(u32);

impl BucketPageSize {
    /// # Errors
    /// Rejects values outside 1-10,000 without clamping or defaulting them.
    pub fn new(value: u32) -> Result<Self, ObjectValueError> {
        if !(1..=10_000).contains(&value) {
            return Err(ObjectValueError::PageSize);
        }

        Ok(Self(value))
    }

    #[must_use]
    pub const fn get(self) -> u32 {
        self.0
    }
}

impl Default for BucketPageSize {
    fn default() -> Self {
        Self(1000)
    }
}
