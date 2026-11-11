use mikrio_core::object::{
    ByteRange, ReadConditions, ReadConditionsInput, Representation, RepresentationInput,
};

use crate::{ObjectRef, StorageError, Timestamp};

pub(crate) mod output;

pub use output::{ChecksumType, GetObjectOutput, HeadObjectOutput};

/// Request the stored protocol checksum facts applicable to the selected read.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub enum ChecksumMode {
    /// Return the applicable stored checksum and its full/composite meaning.
    Enabled,
}

impl ChecksumMode {
    /// Exact S3 spelling.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Enabled => "ENABLED",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct ReadInput {
    object: ObjectRef,
    expected_bucket_owner: Option<String>,
    checksum_mode: Option<ChecksumMode>,
    conditions: ReadConditions,
    range: Option<ByteRange>,
    part_number: Option<mikrio_core::multipart::PartNumber>,
    response: Representation,
}

#[derive(Debug, Clone)]
struct ReadInputBuilder {
    bucket: String,
    key: String,
    expected_bucket_owner: Option<String>,
    checksum_mode: Option<ChecksumMode>,
    conditions: ReadConditionsInput,
    range: Option<String>,
    part_number: Option<u32>,
    response: RepresentationInput,
}

impl ReadInputBuilder {
    fn new(bucket: impl Into<String>, key: impl Into<String>) -> Self {
        Self {
            bucket: bucket.into(),
            key: key.into(),
            expected_bucket_owner: None,
            checksum_mode: None,
            conditions: ReadConditionsInput::default(),
            range: None,
            part_number: None,
            response: RepresentationInput::default(),
        }
    }

    fn build(self) -> Result<ReadInput, StorageError> {
        let object = ObjectRef::new(&self.bucket, &self.key)?;

        if let Some(account) = &self.expected_bucket_owner {
            mikrio_core::access::validate_account_id(account)
                .map_err(StorageError::from_identity)?;
        }

        Ok(ReadInput {
            object,
            expected_bucket_owner: self.expected_bucket_owner,
            checksum_mode: self.checksum_mode,
            conditions: ReadConditions::new(self.conditions).map_err(StorageError::from_object)?,
            range: self
                .range
                .as_deref()
                .map(ByteRange::parse)
                .transpose()
                .map_err(StorageError::from_object)?,
            part_number: self
                .part_number
                .map(mikrio_core::multipart::PartNumber::new)
                .transpose()
                .map_err(StorageError::from_object)?,
            response: Representation::new(self.response).map_err(StorageError::from_object)?,
        })
    }
}

/// Owned values for reading object bytes and metadata without HTTP encoding.
/// Construction performs no I/O and acquires no body or read protection.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GetObjectInput(ReadInput);

impl GetObjectInput {
    /// Start a builder with the required literal bucket/key pair.
    #[must_use]
    pub fn builder(bucket: impl Into<String>, key: impl Into<String>) -> GetObjectInputBuilder {
        GetObjectInputBuilder(ReadInputBuilder::new(bucket, key))
    }

    /// Exact validated bucket name.
    #[must_use]
    pub fn bucket(&self) -> &str {
        self.0.object.bucket()
    }

    /// Exact validated object key.
    #[must_use]
    pub fn key(&self) -> &str {
        self.0.object.key()
    }

    /// Optional account assertion; it supplies no authorization.
    #[must_use]
    pub fn expected_bucket_owner(&self) -> Option<&str> {
        self.0.expected_bucket_owner.as_deref()
    }

    /// Whether the caller requested stored protocol checksum facts.
    #[must_use]
    pub const fn checksum_mode(&self) -> Option<ChecksumMode> {
        self.0.checksum_mode
    }

    /// Optional one-based completed-object assembly position.
    #[must_use]
    pub fn part_number(&self) -> Option<u32> {
        self.0
            .part_number
            .map(mikrio_core::multipart::PartNumber::get)
    }

    /// Optional single HTTP byte range, retaining exact accepted syntax.
    #[must_use]
    pub fn range(&self) -> Option<&str> {
        self.0.range.as_ref().map(ByteRange::as_str)
    }

    /// Optional response-only content type override.
    #[must_use]
    pub fn response_content_type(&self) -> Option<&str> {
        self.0.response.fields().content_type.as_deref()
    }

    /// Optional response-only content encoding override.
    #[must_use]
    pub fn response_content_encoding(&self) -> Option<&str> {
        self.0.response.fields().content_encoding.as_deref()
    }

    /// Optional response-only content disposition override.
    #[must_use]
    pub fn response_content_disposition(&self) -> Option<&str> {
        self.0.response.fields().content_disposition.as_deref()
    }

    /// Optional response-only content language override.
    #[must_use]
    pub fn response_content_language(&self) -> Option<&str> {
        self.0.response.fields().content_language.as_deref()
    }

    /// Optional response-only cache control override.
    #[must_use]
    pub fn response_cache_control(&self) -> Option<&str> {
        self.0.response.fields().cache_control.as_deref()
    }

    /// Optional response-only expiry instant.
    #[must_use]
    pub fn response_expires(&self) -> Option<Timestamp> {
        self.0.response.fields().expires.map(Timestamp::from_core)
    }

    /// Exact entity-tag condition syntax; comparison occurs during read selection.
    #[must_use]
    pub fn if_match(&self) -> Option<&str> {
        self.0.conditions.fields().if_match.as_deref()
    }

    /// Exact entity-tag condition syntax; comparison occurs during read selection.
    #[must_use]
    pub fn if_none_match(&self) -> Option<&str> {
        self.0.conditions.fields().if_none_match.as_deref()
    }

    /// Optional condition instant, rounded down to HTTP whole seconds.
    #[must_use]
    pub fn if_modified_since(&self) -> Option<Timestamp> {
        self.0
            .conditions
            .fields()
            .if_modified_since
            .map(Timestamp::from_core)
    }

    /// Optional condition instant, rounded down to HTTP whole seconds.
    #[must_use]
    pub fn if_unmodified_since(&self) -> Option<Timestamp> {
        self.0
            .conditions
            .fields()
            .if_unmodified_since
            .map(Timestamp::from_core)
    }
}

/// Builder for [`GetObjectInput`]. Validation occurs in [`Self::build`].
#[derive(Debug, Clone)]
pub struct GetObjectInputBuilder(ReadInputBuilder);

impl GetObjectInputBuilder {
    /// Select a completed part by its one-based assembly position (1-10,000).
    #[must_use]
    pub const fn part_number(mut self, value: u32) -> Self {
        self.0.part_number = Some(value);
        self
    }

    /// Select a single closed, open-ended or suffix HTTP byte range.
    #[must_use]
    pub fn range(mut self, value: impl Into<String>) -> Self {
        self.0.range = Some(value.into());
        self
    }

    /// Override the returned content type without changing stored metadata.
    #[must_use]
    pub fn response_content_type(mut self, value: impl Into<String>) -> Self {
        self.0.response.content_type = Some(value.into());
        self
    }

    /// Override the returned content encoding without changing stored metadata.
    #[must_use]
    pub fn response_content_encoding(mut self, value: impl Into<String>) -> Self {
        self.0.response.content_encoding = Some(value.into());
        self
    }

    /// Override the returned content disposition without changing stored metadata.
    #[must_use]
    pub fn response_content_disposition(mut self, value: impl Into<String>) -> Self {
        self.0.response.content_disposition = Some(value.into());
        self
    }

    /// Override the returned content language without changing stored metadata.
    #[must_use]
    pub fn response_content_language(mut self, value: impl Into<String>) -> Self {
        self.0.response.content_language = Some(value.into());
        self
    }

    /// Override the returned cache control without changing stored metadata.
    #[must_use]
    pub fn response_cache_control(mut self, value: impl Into<String>) -> Self {
        self.0.response.cache_control = Some(value.into());
        self
    }

    /// Override the returned expiry instant without changing stored metadata.
    #[must_use]
    pub const fn response_expires(mut self, value: Timestamp) -> Self {
        self.0.response.expires = Some(value.into_core());
        self
    }

    /// Supply an entity-tag condition, retaining exact strong/weak/list syntax.
    #[must_use]
    pub fn if_match(mut self, value: impl Into<String>) -> Self {
        self.0.conditions.if_match = Some(value.into());
        self
    }

    /// Supply an entity-tag condition, retaining exact strong/weak/list syntax.
    #[must_use]
    pub fn if_none_match(mut self, value: impl Into<String>) -> Self {
        self.0.conditions.if_none_match = Some(value.into());
        self
    }

    /// Supply a condition instant; `build()` rounds down to HTTP whole seconds.
    #[must_use]
    pub const fn if_modified_since(mut self, value: Timestamp) -> Self {
        self.0.conditions.if_modified_since = Some(value.into_core());
        self
    }

    /// Supply a condition instant; `build()` rounds down to HTTP whole seconds.
    #[must_use]
    pub const fn if_unmodified_since(mut self, value: Timestamp) -> Self {
        self.0.conditions.if_unmodified_since = Some(value.into_core());
        self
    }

    /// Assert ownership by this twelve-digit account, preserving leading zeroes.
    #[must_use]
    pub fn expected_bucket_owner(mut self, account: impl Into<String>) -> Self {
        self.0.expected_bucket_owner = Some(account.into());
        self
    }

    /// Request the applicable stored checksum facts.
    #[must_use]
    pub const fn checksum_mode(mut self, mode: ChecksumMode) -> Self {
        self.0.checksum_mode = Some(mode);
        self
    }

    /// Validate identity, owner, conditions, selection and response overrides without I/O.
    ///
    /// # Errors
    /// Returns `InvalidBucketName` for an invalid bucket, or `InvalidArgument`
    /// for an invalid key, account assertion, entity-tag condition, range, part
    /// number or response override.
    pub fn build(self) -> Result<GetObjectInput, StorageError> {
        self.0.build().map(GetObjectInput)
    }
}

/// Owned values for inspecting object metadata without acquiring a response body.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HeadObjectInput(ReadInput);

impl HeadObjectInput {
    /// Start a builder with the required literal bucket/key pair.
    #[must_use]
    pub fn builder(bucket: impl Into<String>, key: impl Into<String>) -> HeadObjectInputBuilder {
        HeadObjectInputBuilder(ReadInputBuilder::new(bucket, key))
    }

    /// Exact validated bucket name.
    #[must_use]
    pub fn bucket(&self) -> &str {
        self.0.object.bucket()
    }

    /// Exact validated object key.
    #[must_use]
    pub fn key(&self) -> &str {
        self.0.object.key()
    }

    /// Optional account assertion; it supplies no authorization.
    #[must_use]
    pub fn expected_bucket_owner(&self) -> Option<&str> {
        self.0.expected_bucket_owner.as_deref()
    }

    /// Whether the caller requested stored protocol checksum facts.
    #[must_use]
    pub const fn checksum_mode(&self) -> Option<ChecksumMode> {
        self.0.checksum_mode
    }

    /// Optional one-based completed-object assembly position.
    #[must_use]
    pub fn part_number(&self) -> Option<u32> {
        self.0
            .part_number
            .map(mikrio_core::multipart::PartNumber::get)
    }

    /// Optional single HTTP byte range, retaining exact accepted syntax.
    #[must_use]
    pub fn range(&self) -> Option<&str> {
        self.0.range.as_ref().map(ByteRange::as_str)
    }

    /// Optional response-only content type override.
    #[must_use]
    pub fn response_content_type(&self) -> Option<&str> {
        self.0.response.fields().content_type.as_deref()
    }

    /// Optional response-only content encoding override.
    #[must_use]
    pub fn response_content_encoding(&self) -> Option<&str> {
        self.0.response.fields().content_encoding.as_deref()
    }

    /// Optional response-only content disposition override.
    #[must_use]
    pub fn response_content_disposition(&self) -> Option<&str> {
        self.0.response.fields().content_disposition.as_deref()
    }

    /// Optional response-only content language override.
    #[must_use]
    pub fn response_content_language(&self) -> Option<&str> {
        self.0.response.fields().content_language.as_deref()
    }

    /// Optional response-only cache control override.
    #[must_use]
    pub fn response_cache_control(&self) -> Option<&str> {
        self.0.response.fields().cache_control.as_deref()
    }

    /// Optional response-only expiry instant.
    #[must_use]
    pub fn response_expires(&self) -> Option<Timestamp> {
        self.0.response.fields().expires.map(Timestamp::from_core)
    }

    /// Exact entity-tag condition syntax; comparison occurs during read selection.
    #[must_use]
    pub fn if_match(&self) -> Option<&str> {
        self.0.conditions.fields().if_match.as_deref()
    }

    /// Exact entity-tag condition syntax; comparison occurs during read selection.
    #[must_use]
    pub fn if_none_match(&self) -> Option<&str> {
        self.0.conditions.fields().if_none_match.as_deref()
    }

    /// Optional condition instant, rounded down to HTTP whole seconds.
    #[must_use]
    pub fn if_modified_since(&self) -> Option<Timestamp> {
        self.0
            .conditions
            .fields()
            .if_modified_since
            .map(Timestamp::from_core)
    }

    /// Optional condition instant, rounded down to HTTP whole seconds.
    #[must_use]
    pub fn if_unmodified_since(&self) -> Option<Timestamp> {
        self.0
            .conditions
            .fields()
            .if_unmodified_since
            .map(Timestamp::from_core)
    }
}

/// Builder for [`HeadObjectInput`]. Validation occurs in [`Self::build`].
#[derive(Debug, Clone)]
pub struct HeadObjectInputBuilder(ReadInputBuilder);

impl HeadObjectInputBuilder {
    /// Select a completed part by its one-based assembly position (1-10,000).
    #[must_use]
    pub const fn part_number(mut self, value: u32) -> Self {
        self.0.part_number = Some(value);
        self
    }

    /// Select a single closed, open-ended or suffix HTTP byte range.
    #[must_use]
    pub fn range(mut self, value: impl Into<String>) -> Self {
        self.0.range = Some(value.into());
        self
    }

    /// Override the returned content type without changing stored metadata.
    #[must_use]
    pub fn response_content_type(mut self, value: impl Into<String>) -> Self {
        self.0.response.content_type = Some(value.into());
        self
    }

    /// Override the returned content encoding without changing stored metadata.
    #[must_use]
    pub fn response_content_encoding(mut self, value: impl Into<String>) -> Self {
        self.0.response.content_encoding = Some(value.into());
        self
    }

    /// Override the returned content disposition without changing stored metadata.
    #[must_use]
    pub fn response_content_disposition(mut self, value: impl Into<String>) -> Self {
        self.0.response.content_disposition = Some(value.into());
        self
    }

    /// Override the returned content language without changing stored metadata.
    #[must_use]
    pub fn response_content_language(mut self, value: impl Into<String>) -> Self {
        self.0.response.content_language = Some(value.into());
        self
    }

    /// Override the returned cache control without changing stored metadata.
    #[must_use]
    pub fn response_cache_control(mut self, value: impl Into<String>) -> Self {
        self.0.response.cache_control = Some(value.into());
        self
    }

    /// Override the returned expiry instant without changing stored metadata.
    #[must_use]
    pub const fn response_expires(mut self, value: Timestamp) -> Self {
        self.0.response.expires = Some(value.into_core());
        self
    }

    /// Supply an entity-tag condition, retaining exact strong/weak/list syntax.
    #[must_use]
    pub fn if_match(mut self, value: impl Into<String>) -> Self {
        self.0.conditions.if_match = Some(value.into());
        self
    }

    /// Supply an entity-tag condition, retaining exact strong/weak/list syntax.
    #[must_use]
    pub fn if_none_match(mut self, value: impl Into<String>) -> Self {
        self.0.conditions.if_none_match = Some(value.into());
        self
    }

    /// Supply a condition instant; `build()` rounds down to HTTP whole seconds.
    #[must_use]
    pub const fn if_modified_since(mut self, value: Timestamp) -> Self {
        self.0.conditions.if_modified_since = Some(value.into_core());
        self
    }

    /// Supply a condition instant; `build()` rounds down to HTTP whole seconds.
    #[must_use]
    pub const fn if_unmodified_since(mut self, value: Timestamp) -> Self {
        self.0.conditions.if_unmodified_since = Some(value.into_core());
        self
    }

    /// Assert ownership by this twelve-digit account, preserving leading zeroes.
    #[must_use]
    pub fn expected_bucket_owner(mut self, account: impl Into<String>) -> Self {
        self.0.expected_bucket_owner = Some(account.into());
        self
    }

    /// Request the applicable stored checksum facts.
    #[must_use]
    pub const fn checksum_mode(mut self, mode: ChecksumMode) -> Self {
        self.0.checksum_mode = Some(mode);
        self
    }

    /// Validate identity, owner, conditions, selection and response overrides without I/O.
    ///
    /// # Errors
    /// Returns `InvalidBucketName` for an invalid bucket, or `InvalidArgument`
    /// for an invalid key, account assertion, entity-tag condition, range, part
    /// number or response override.
    pub fn build(self) -> Result<HeadObjectInput, StorageError> {
        self.0.build().map(HeadObjectInput)
    }
}

impl ReadInput {
    fn into_core(self) -> mikrio_core::object::ReadRequest {
        let (bucket, key) = self.object.into_core();

        mikrio_core::object::ReadRequest {
            bucket,
            key,
            expected_owner: self.expected_bucket_owner,
            conditions: self.conditions,
            range: self.range,
            part_number: self.part_number,
            response: self.response,
        }
    }
}

impl crate::Store {
    /// Select detached metadata and one protected cursor for the same generation.
    ///
    /// # Errors
    /// Reports missing/conditional targets, owner assertions, contention, corrupt
    /// payloads and storage failures. Close the returned body when done reading.
    pub async fn get_object(&self, input: GetObjectInput) -> Result<GetObjectOutput, StorageError> {
        let include_checksums = input.0.checksum_mode.is_some();

        let (selected, body) = self
            .connection
            .get_object(input.0.into_core())
            .await
            .map_err(StorageError::from_operation)?;

        Ok(GetObjectOutput {
            details: output::ReadMetadata::from_core(selected, include_checksums),
            body: crate::ObjectBody { body },
        })
    }

    /// Select metadata in a short current snapshot without opening a payload file.
    ///
    /// # Errors
    /// Reports missing/conditional targets, owner assertions and storage failures.
    pub async fn head_object(
        &self,
        input: HeadObjectInput,
    ) -> Result<HeadObjectOutput, StorageError> {
        let include_checksums = input.0.checksum_mode.is_some();

        let selected = self
            .connection
            .head_object(input.0.into_core())
            .await
            .map_err(StorageError::from_operation)?;

        Ok(HeadObjectOutput {
            details: output::ReadMetadata::from_core(selected, include_checksums),
        })
    }
}
