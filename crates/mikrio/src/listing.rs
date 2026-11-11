use mikrio_core::object::{BucketName, BucketPageSize, ObjectPageSize};

use crate::{StorageError, Store, Timestamp};

/// Owned literal listing values. Tokens are validated by the selected backend,
/// against its store/bucket identity and filters when the operation executes.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ListObjectsV2Input {
    bucket: BucketName,
    expected_bucket_owner: Option<String>,
    prefix: Option<String>,
    delimiter: Option<String>,
    continuation_token: Option<String>,
    start_after: Option<String>,
    max_keys: ObjectPageSize,
}

impl ListObjectsV2Input {
    /// Start a single-page request with the native default of 1,000 entries.
    #[must_use]
    pub fn builder(bucket: impl Into<String>) -> ListObjectsV2InputBuilder {
        ListObjectsV2InputBuilder {
            bucket: bucket.into(),
            prefix: None,
            delimiter: None,
            continuation_token: None,
            start_after: None,
            expected_bucket_owner: None,
            max_keys: None,
        }
    }

    /// Exact validated bucket name.
    #[must_use]
    pub fn bucket(&self) -> &str {
        self.bucket.as_str()
    }

    /// Supplied literal value, preserving absence and empty strings.
    #[must_use]
    pub fn prefix(&self) -> Option<&str> {
        self.prefix.as_deref()
    }

    /// Supplied literal value, preserving absence and empty strings.
    #[must_use]
    pub fn delimiter(&self) -> Option<&str> {
        self.delimiter.as_deref()
    }

    /// Supplied literal value, preserving absence and empty strings.
    #[must_use]
    pub fn continuation_token(&self) -> Option<&str> {
        self.continuation_token.as_deref()
    }

    /// Supplied literal value, preserving absence and empty strings.
    #[must_use]
    pub fn start_after(&self) -> Option<&str> {
        self.start_after.as_deref()
    }

    /// Optional twelve-digit account assertion; it supplies no authorization.
    #[must_use]
    pub fn expected_bucket_owner(&self) -> Option<&str> {
        self.expected_bucket_owner.as_deref()
    }

    /// Validated page bound, with the native default applied only on omission.
    #[must_use]
    pub const fn max_keys(&self) -> u32 {
        self.max_keys.get()
    }

    fn into_core(self) -> mikrio_core::object::ListObjectsRequest {
        mikrio_core::object::ListObjectsRequest {
            bucket: self.bucket,
            expected_owner: self.expected_bucket_owner,
            prefix: self.prefix,
            delimiter: self.delimiter,
            start_after: self.start_after,
            continuation_token: self.continuation_token,
            max_keys: self.max_keys,
        }
    }
}

/// Builder for a single native listing page; construction performs no I/O.
#[derive(Debug, Clone)]
pub struct ListObjectsV2InputBuilder {
    bucket: String,
    expected_bucket_owner: Option<String>,
    prefix: Option<String>,
    delimiter: Option<String>,
    continuation_token: Option<String>,
    start_after: Option<String>,
    max_keys: Option<u32>,
}

impl ListObjectsV2InputBuilder {
    /// Set a literal value without path, Unicode or percent-encoding normalization.
    #[must_use]
    pub fn prefix(mut self, value: impl Into<String>) -> Self {
        self.prefix = Some(value.into());
        self
    }

    /// Set a literal value without path, Unicode or percent-encoding normalization.
    #[must_use]
    pub fn delimiter(mut self, value: impl Into<String>) -> Self {
        self.delimiter = Some(value.into());
        self
    }

    /// Set a literal value without path, Unicode or percent-encoding normalization.
    #[must_use]
    pub fn continuation_token(mut self, value: impl Into<String>) -> Self {
        self.continuation_token = Some(value.into());
        self
    }

    /// Set a literal value without path, Unicode or percent-encoding normalization.
    #[must_use]
    pub fn start_after(mut self, value: impl Into<String>) -> Self {
        self.start_after = Some(value.into());
        self
    }

    /// Assert ownership by this twelve-digit account, preserving leading zeroes.
    #[must_use]
    pub fn expected_bucket_owner(mut self, value: impl Into<String>) -> Self {
        self.expected_bucket_owner = Some(value.into());
        self
    }

    /// Set the page bound; invalid values are rejected by `build()`.
    #[must_use]
    pub const fn max_keys(mut self, value: u32) -> Self {
        self.max_keys = Some(value);
        self
    }

    /// Validate operation values without decoding a backend continuation token.
    ///
    /// # Errors
    /// Returns `InvalidArgument` for an invalid page bound or owner assertion.
    /// Invalid bucket names return `InvalidBucketName`.
    pub fn build(self) -> Result<ListObjectsV2Input, StorageError> {
        let bucket = BucketName::parse(&self.bucket).map_err(StorageError::from_object)?;

        if let Some(owner) = &self.expected_bucket_owner {
            mikrio_core::access::validate_account_id(owner).map_err(StorageError::from_identity)?;
        }

        let max_keys = self
            .max_keys
            .map(ObjectPageSize::new)
            .transpose()
            .map_err(StorageError::from_object)?
            .unwrap_or_default();

        Ok(ListObjectsV2Input {
            bucket,
            prefix: self.prefix,
            delimiter: self.delimiter,
            continuation_token: self.continuation_token,
            start_after: self.start_after,
            expected_bucket_owner: self.expected_bucket_owner,
            max_keys,
        })
    }
}

/// Owned literal listing values. Tokens are validated by the selected backend,
/// against its store/bucket identity and filters when the operation executes.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ListBucketsInput {
    prefix: Option<String>,
    continuation_token: Option<String>,
    max_buckets: BucketPageSize,
}

impl ListBucketsInput {
    /// Start a single-page request with the native default of 1,000 entries.
    #[must_use]
    pub fn builder() -> ListBucketsInputBuilder {
        ListBucketsInputBuilder {
            prefix: None,
            continuation_token: None,
            max_buckets: None,
        }
    }

    /// Supplied literal value, preserving absence and empty strings.
    #[must_use]
    pub fn prefix(&self) -> Option<&str> {
        self.prefix.as_deref()
    }

    /// Supplied literal value, preserving absence and empty strings.
    #[must_use]
    pub fn continuation_token(&self) -> Option<&str> {
        self.continuation_token.as_deref()
    }

    /// Validated page bound, with the native default applied only on omission.
    #[must_use]
    pub const fn max_buckets(&self) -> u32 {
        self.max_buckets.get()
    }
}

/// Builder for a single native listing page; construction performs no I/O.
#[derive(Debug, Clone)]
pub struct ListBucketsInputBuilder {
    prefix: Option<String>,
    continuation_token: Option<String>,
    max_buckets: Option<u32>,
}

impl ListBucketsInputBuilder {
    /// Set a literal value without path, Unicode or percent-encoding normalization.
    #[must_use]
    pub fn prefix(mut self, value: impl Into<String>) -> Self {
        self.prefix = Some(value.into());
        self
    }

    /// Set a literal value without path, Unicode or percent-encoding normalization.
    #[must_use]
    pub fn continuation_token(mut self, value: impl Into<String>) -> Self {
        self.continuation_token = Some(value.into());
        self
    }

    /// Set the page bound; invalid values are rejected by `build()`.
    #[must_use]
    pub const fn max_buckets(mut self, value: u32) -> Self {
        self.max_buckets = Some(value);
        self
    }

    /// Validate operation values without decoding a backend continuation token.
    ///
    /// # Errors
    /// Returns `InvalidArgument` for an invalid page bound.
    pub fn build(self) -> Result<ListBucketsInput, StorageError> {
        let max_buckets = self
            .max_buckets
            .map(BucketPageSize::new)
            .transpose()
            .map_err(StorageError::from_object)?
            .unwrap_or_default();

        Ok(ListBucketsInput {
            prefix: self.prefix,
            continuation_token: self.continuation_token,
            max_buckets,
        })
    }
}

/// Detached facts for one bucket.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BucketSummary {
    pub(crate) name: String,
    pub(crate) creation_date: Timestamp,
    pub(crate) bucket_region: String,
}

impl BucketSummary {
    /// Literal bucket name.
    #[must_use]
    pub fn name(&self) -> &str {
        &self.name
    }

    /// Bucket creation instant, retaining millisecond precision.
    #[must_use]
    pub const fn creation_date(&self) -> Timestamp {
        self.creation_date
    }

    /// Store region containing the bucket.
    #[must_use]
    pub fn bucket_region(&self) -> &str {
        &self.bucket_region
    }
}

/// Detached facts for one object; size describes the full object.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ObjectSummary {
    pub(crate) key: String,
    pub(crate) size: u64,
    pub(crate) etag: String,
    pub(crate) last_modified: Timestamp,
}

impl ObjectSummary {
    /// Exact literal key.
    #[must_use]
    pub fn key(&self) -> &str {
        &self.key
    }

    /// Full object size in bytes, independent of read ranges.
    #[must_use]
    pub const fn size(&self) -> u64 {
        self.size
    }

    /// Quoted opaque entity tag.
    #[must_use]
    pub fn etag(&self) -> &str {
        &self.etag
    }

    /// Object modification instant, retaining millisecond precision.
    #[must_use]
    pub const fn last_modified(&self) -> Timestamp {
        self.last_modified
    }
}

/// A literal grouping prefix in an object listing.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CommonPrefix {
    pub(crate) prefix: String,
}

impl CommonPrefix {
    pub(crate) fn new(prefix: String) -> Self {
        Self { prefix }
    }

    /// Literal grouping prefix; not a normalized path.
    #[must_use]
    pub fn prefix(&self) -> &str {
        &self.prefix
    }
}

/// One object-list page, including empty collections and continuation state.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ListObjectsV2Output {
    pub(crate) contents: Vec<ObjectSummary>,
    pub(crate) common_prefixes: Vec<CommonPrefix>,
    pub(crate) is_truncated: bool,
    pub(crate) next_continuation_token: Option<String>,
}

impl ListObjectsV2Output {
    /// Object entries in this page.
    #[must_use]
    pub fn contents(&self) -> &[ObjectSummary] {
        &self.contents
    }

    /// Grouping prefixes; an empty object collection does not imply the last page.
    #[must_use]
    pub fn common_prefixes(&self) -> &[CommonPrefix] {
        &self.common_prefixes
    }

    /// Whether another page remains.
    #[must_use]
    pub const fn is_truncated(&self) -> bool {
        self.is_truncated
    }

    /// Opaque token for the next page within this backend and filter scope.
    #[must_use]
    pub fn next_continuation_token(&self) -> Option<&str> {
        self.next_continuation_token.as_deref()
    }
}

/// One bucket-list page; an absent continuation token marks its end.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ListBucketsOutput {
    pub(crate) buckets: Vec<BucketSummary>,
    pub(crate) continuation_token: Option<String>,
}

impl ListBucketsOutput {
    /// Bucket entries, present even when empty.
    #[must_use]
    pub fn buckets(&self) -> &[BucketSummary] {
        &self.buckets
    }

    /// Opaque next-page token; this operation has no truncation flag.
    #[must_use]
    pub fn continuation_token(&self) -> Option<&str> {
        self.continuation_token.as_deref()
    }
}

impl Store {
    /// Read one live bucket page. The native default is 1,000 entries; tokens
    /// remain scoped to this store and the exact prefix, including its presence.
    ///
    /// # Errors
    /// Returns invalid token/filter, lifecycle or storage failures.
    pub async fn list_buckets(
        &self,
        input: ListBucketsInput,
    ) -> Result<ListBucketsOutput, StorageError> {
        let page = self
            .connection
            .list_buckets(mikrio_core::object::ListBucketsRequest {
                prefix: input.prefix,
                continuation_token: input.continuation_token,
                max_buckets: Some(input.max_buckets),
                bucket_region: None,
            })
            .await
            .map_err(StorageError::from_operation)?;

        Ok(ListBucketsOutput {
            buckets: page
                .buckets
                .into_iter()
                .map(|bucket| BucketSummary {
                    name: bucket.name,
                    creation_date: Timestamp::from_core(bucket.creation_date),
                    bucket_region: page.bucket_region.clone(),
                })
                .collect(),
            continuation_token: page.continuation_token,
        })
    }

    /// Read one consistent object/prefix page. Continuation observes the live
    /// namespace and requires the same prefix and delimiter, including presence.
    /// An explicit zero page returns no entries or continuation. No payload opens.
    ///
    /// # Errors
    /// Returns invalid filter/token, missing bucket, owner, lifecycle or storage
    /// failures. A token from a retired/recreated bucket is invalid.
    pub async fn list_objects_v2(
        &self,
        input: ListObjectsV2Input,
    ) -> Result<ListObjectsV2Output, StorageError> {
        let output = self
            .connection
            .list_objects_v2(input.into_core())
            .await
            .map_err(StorageError::from_operation)?;

        Ok(ListObjectsV2Output {
            contents: output
                .contents
                .into_iter()
                .map(|object| ObjectSummary {
                    key: object.key,
                    size: object.size,
                    etag: object.etag,
                    last_modified: Timestamp::from_core(object.last_modified),
                })
                .collect(),
            common_prefixes: output
                .common_prefixes
                .into_iter()
                .map(|prefix| CommonPrefix { prefix })
                .collect(),
            is_truncated: output.next_continuation_token.is_some(),
            next_continuation_token: output.next_continuation_token,
        })
    }
}
