//! Local object copies with independent source and destination assertions.

use std::collections::BTreeMap;

use mikrio_core::object::{
    CopyRequest, ObjectTags, ReadConditions, ReadConditionsInput, Representation,
    RepresentationInput, UserMetadata, WriteConditions,
};

use crate::{ChecksumAlgorithm, ObjectRef, PutObjectOutput, StorageError, Tag, Timestamp};

/// Select source metadata/tags or replace the corresponding complete set.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
#[non_exhaustive]
pub enum CopyDirective {
    /// Copy the selected source's corresponding fields.
    #[default]
    Copy,
    /// Replace the corresponding fields with the supplied values.
    Replace,
}

impl CopyDirective {
    /// Exact S3 spelling.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Copy => "COPY",
            Self::Replace => "REPLACE",
        }
    }

    pub(crate) const fn into_core(self) -> mikrio_core::object::CopyDirective {
        match self {
            Self::Copy => mikrio_core::object::CopyDirective::Copy,
            Self::Replace => mikrio_core::object::CopyDirective::Replace,
        }
    }
}

/// Owned copy options. Construction performs no source read or mutation.
#[derive(Debug, Clone)]
pub struct CopyObjectInput {
    object: ObjectRef,
    copy_source: ObjectRef,
    expected_bucket_owner: Option<String>,
    expected_source_bucket_owner: Option<String>,
    source_conditions: ReadConditions,
    conditions: WriteConditions,
    metadata_directive: CopyDirective,
    representation: Representation,
    metadata: UserMetadata,
    tagging_directive: CopyDirective,
    tags: Option<Vec<Tag>>,
    checksum_algorithm: Option<ChecksumAlgorithm>,
}

/// Builder for independently validated copy options.
#[derive(Debug, Clone)]
pub struct CopyObjectInputBuilder {
    bucket: String,
    key: String,
    copy_source: ObjectRef,
    expected_bucket_owner: Option<String>,
    expected_source_bucket_owner: Option<String>,
    source_conditions: ReadConditionsInput,
    if_match: Option<String>,
    if_none_match: Option<String>,
    metadata_directive: CopyDirective,
    representation: RepresentationInput,
    metadata: Vec<(String, String)>,
    tagging_directive: CopyDirective,
    tags: Option<Vec<Tag>>,
    checksum_algorithm: Option<ChecksumAlgorithm>,
}

impl CopyObjectInput {
    /// Start a copy to a literal destination from a validated local source.
    #[must_use]
    pub fn builder(
        bucket: impl Into<String>,
        key: impl Into<String>,
        copy_source: ObjectRef,
    ) -> CopyObjectInputBuilder {
        CopyObjectInputBuilder {
            bucket: bucket.into(),
            key: key.into(),
            copy_source,
            expected_bucket_owner: None,
            expected_source_bucket_owner: None,
            source_conditions: ReadConditionsInput::default(),
            if_match: None,
            if_none_match: None,
            metadata_directive: CopyDirective::Copy,
            representation: RepresentationInput::default(),
            metadata: Vec::new(),
            tagging_directive: CopyDirective::Copy,
            tags: None,
            checksum_algorithm: None,
        }
    }

    /// Exact destination bucket.
    #[must_use]
    pub fn bucket(&self) -> &str {
        self.object.bucket()
    }

    /// Exact destination key.
    #[must_use]
    pub fn key(&self) -> &str {
        self.object.key()
    }

    /// Structured local source, without HTTP encoding.
    #[must_use]
    pub const fn copy_source(&self) -> &ObjectRef {
        &self.copy_source
    }

    /// Supplied metadata, with validated lowercase keys.
    #[must_use]
    pub const fn metadata(&self) -> &BTreeMap<String, String> {
        self.metadata.entries()
    }

    /// Supplied tag set, retaining omission versus an explicit empty set.
    #[must_use]
    pub fn tags(&self) -> Option<&[Tag]> {
        self.tags.as_deref()
    }

    /// Optional account assertion; it supplies no authorization.
    #[must_use]
    pub fn expected_bucket_owner(&self) -> Option<&str> {
        self.expected_bucket_owner.as_deref()
    }

    /// Optional account assertion; it supplies no authorization.
    #[must_use]
    pub fn expected_source_bucket_owner(&self) -> Option<&str> {
        self.expected_source_bucket_owner.as_deref()
    }

    /// Supplied replacement representation field, preserving empty strings.
    #[must_use]
    pub fn content_type(&self) -> Option<&str> {
        self.representation.fields().content_type.as_deref()
    }

    /// Supplied replacement representation field, preserving empty strings.
    #[must_use]
    pub fn content_encoding(&self) -> Option<&str> {
        self.representation.fields().content_encoding.as_deref()
    }

    /// Supplied replacement representation field, preserving empty strings.
    #[must_use]
    pub fn content_disposition(&self) -> Option<&str> {
        self.representation.fields().content_disposition.as_deref()
    }

    /// Supplied replacement representation field, preserving empty strings.
    #[must_use]
    pub fn content_language(&self) -> Option<&str> {
        self.representation.fields().content_language.as_deref()
    }

    /// Supplied replacement representation field, preserving empty strings.
    #[must_use]
    pub fn cache_control(&self) -> Option<&str> {
        self.representation.fields().cache_control.as_deref()
    }

    /// Supplied replacement cache expiry.
    #[must_use]
    pub fn expires(&self) -> Option<Timestamp> {
        self.representation
            .fields()
            .expires
            .map(Timestamp::from_core)
    }

    /// Destination assertion evaluated atomically at publication.
    #[must_use]
    pub fn if_match(&self) -> Option<&str> {
        self.conditions.if_match()
    }

    /// Source assertion evaluated against the selected source.
    #[must_use]
    pub fn copy_source_if_match(&self) -> Option<&str> {
        self.source_conditions.fields().if_match.as_deref()
    }

    /// Destination assertion evaluated atomically at publication.
    #[must_use]
    pub fn if_none_match(&self) -> Option<&str> {
        self.conditions.if_none_match()
    }

    /// Source assertion evaluated against the selected source.
    #[must_use]
    pub fn copy_source_if_none_match(&self) -> Option<&str> {
        self.source_conditions.fields().if_none_match.as_deref()
    }

    /// Source instant normalized to HTTP whole-second precision.
    #[must_use]
    pub fn copy_source_if_modified_since(&self) -> Option<Timestamp> {
        self.source_conditions
            .fields()
            .if_modified_since
            .map(Timestamp::from_core)
    }

    /// Source instant normalized to HTTP whole-second precision.
    #[must_use]
    pub fn copy_source_if_unmodified_since(&self) -> Option<Timestamp> {
        self.source_conditions
            .fields()
            .if_unmodified_since
            .map(Timestamp::from_core)
    }

    /// Validated selection for the corresponding copy behavior.
    #[must_use]
    pub const fn metadata_directive(&self) -> CopyDirective {
        self.metadata_directive
    }

    /// Validated selection for the corresponding copy behavior.
    #[must_use]
    pub const fn tagging_directive(&self) -> CopyDirective {
        self.tagging_directive
    }

    /// Validated selection for the corresponding copy behavior.
    #[must_use]
    pub const fn checksum_algorithm(&self) -> Option<ChecksumAlgorithm> {
        self.checksum_algorithm
    }

    fn try_into_core(self) -> Result<CopyRequest, StorageError> {
        let (bucket, key) = self.object.into_core();
        let (source_bucket, source_key) = self.copy_source.into_core();

        Ok(CopyRequest {
            bucket,
            key,
            source: mikrio_core::object::ObjectRef {
                bucket: source_bucket,
                key: source_key,
            },
            expected_owner: self.expected_bucket_owner,
            expected_source_owner: self.expected_source_bucket_owner,
            source_conditions: self.source_conditions,
            conditions: self.conditions,
            metadata_directive: self.metadata_directive.into_core(),
            representation: self.representation,
            metadata: self.metadata,
            tagging_directive: self.tagging_directive.into_core(),
            tags: self
                .tags
                .map(|tags| ObjectTags::new(tags.into_iter().map(|tag| (tag.key, tag.value))))
                .transpose()
                .map_err(StorageError::from_object)?,
            checksum_algorithm: self.checksum_algorithm.map(ChecksumAlgorithm::into_core),
        })
    }
}

impl CopyObjectInputBuilder {
    /// Set the corresponding assertion; syntax is checked by `build()`.
    #[must_use]
    pub fn expected_bucket_owner(mut self, value: impl Into<String>) -> Self {
        self.expected_bucket_owner = Some(value.into());
        self
    }

    /// Set the corresponding assertion; syntax is checked by `build()`.
    #[must_use]
    pub fn expected_source_bucket_owner(mut self, value: impl Into<String>) -> Self {
        self.expected_source_bucket_owner = Some(value.into());
        self
    }

    /// Set the corresponding assertion; syntax is checked by `build()`.
    #[must_use]
    pub fn if_match(mut self, value: impl Into<String>) -> Self {
        self.if_match = Some(value.into());
        self
    }

    /// Set the corresponding assertion; syntax is checked by `build()`.
    #[must_use]
    pub fn if_none_match(mut self, value: impl Into<String>) -> Self {
        self.if_none_match = Some(value.into());
        self
    }

    /// Set a source entity-tag condition, independently of destination conditions.
    #[must_use]
    pub fn copy_source_if_match(mut self, value: impl Into<String>) -> Self {
        self.source_conditions.if_match = Some(value.into());
        self
    }

    /// Set a source entity-tag condition, independently of destination conditions.
    #[must_use]
    pub fn copy_source_if_none_match(mut self, value: impl Into<String>) -> Self {
        self.source_conditions.if_none_match = Some(value.into());
        self
    }

    /// Set a source time condition, normalized by `build()`.
    #[must_use]
    pub const fn copy_source_if_modified_since(mut self, value: Timestamp) -> Self {
        self.source_conditions.if_modified_since = Some(value.into_core());
        self
    }

    /// Set a source time condition, normalized by `build()`.
    #[must_use]
    pub const fn copy_source_if_unmodified_since(mut self, value: Timestamp) -> Self {
        self.source_conditions.if_unmodified_since = Some(value.into_core());
        self
    }

    /// Set a replacement representation field, validated by `build()`.
    #[must_use]
    pub fn content_type(mut self, value: impl Into<String>) -> Self {
        self.representation.content_type = Some(value.into());
        self
    }

    /// Set a replacement representation field, validated by `build()`.
    #[must_use]
    pub fn content_encoding(mut self, value: impl Into<String>) -> Self {
        self.representation.content_encoding = Some(value.into());
        self
    }

    /// Set a replacement representation field, validated by `build()`.
    #[must_use]
    pub fn content_disposition(mut self, value: impl Into<String>) -> Self {
        self.representation.content_disposition = Some(value.into());
        self
    }

    /// Set a replacement representation field, validated by `build()`.
    #[must_use]
    pub fn content_language(mut self, value: impl Into<String>) -> Self {
        self.representation.content_language = Some(value.into());
        self
    }

    /// Set a replacement representation field, validated by `build()`.
    #[must_use]
    pub fn cache_control(mut self, value: impl Into<String>) -> Self {
        self.representation.cache_control = Some(value.into());
        self
    }

    /// Set replacement cache expiry without changing retention policy.
    #[must_use]
    pub const fn expires(mut self, value: Timestamp) -> Self {
        self.representation.expires = Some(value.into_core());
        self
    }

    /// Supply the replacement metadata set; duplicate folded keys reject.
    #[must_use]
    pub fn metadata(mut self, entries: Vec<(String, String)>) -> Self {
        self.metadata = entries;
        self
    }

    /// Supply replacement tags. Requires the REPLACE tagging directive.
    #[must_use]
    pub fn tags(mut self, tags: Vec<Tag>) -> Self {
        self.tags = Some(tags);
        self
    }

    /// Select independent COPY or REPLACE behavior.
    #[must_use]
    pub const fn metadata_directive(mut self, value: CopyDirective) -> Self {
        self.metadata_directive = value;
        self
    }

    /// Select independent COPY or REPLACE behavior.
    #[must_use]
    pub const fn tagging_directive(mut self, value: CopyDirective) -> Self {
        self.tagging_directive = value;
        self
    }

    /// Override source checksum-algorithm inheritance for the new bytes.
    #[must_use]
    pub const fn checksum_algorithm(mut self, value: ChecksumAlgorithm) -> Self {
        self.checksum_algorithm = Some(value);
        self
    }

    /// Validate values without selecting a source or evaluating live conditions.
    ///
    /// # Errors
    /// Returns an input error for malformed identities, assertions, metadata,
    /// tags, dates or headers. Supplied tags require REPLACE.
    pub fn build(self) -> Result<CopyObjectInput, StorageError> {
        let object = ObjectRef::new(&self.bucket, &self.key)?;

        for owner in [
            &self.expected_bucket_owner,
            &self.expected_source_bucket_owner,
        ]
        .into_iter()
        .flatten()
        {
            mikrio_core::access::validate_account_id(owner).map_err(StorageError::from_identity)?;
        }

        if self.tags.is_some() && self.tagging_directive != CopyDirective::Replace {
            return Err(StorageError::from_object(
                mikrio_core::object::ObjectValueError::CopyDirective,
            ));
        }

        let tags = self
            .tags
            .map(|tags| ObjectTags::new(tags.into_iter().map(|tag| (tag.key, tag.value))))
            .transpose()
            .map_err(StorageError::from_object)?
            .map(|tags| {
                tags.into_entries()
                    .into_iter()
                    .map(|(key, value)| Tag { key, value })
                    .collect()
            });

        Ok(CopyObjectInput {
            object,
            copy_source: self.copy_source,
            expected_bucket_owner: self.expected_bucket_owner,
            expected_source_bucket_owner: self.expected_source_bucket_owner,
            source_conditions: ReadConditions::new(self.source_conditions)
                .map_err(StorageError::from_object)?,
            conditions: WriteConditions::new(self.if_match, self.if_none_match)
                .map_err(StorageError::from_object)?,
            metadata_directive: self.metadata_directive,
            representation: Representation::new(self.representation)
                .map_err(StorageError::from_object)?,
            metadata: UserMetadata::new(self.metadata).map_err(StorageError::from_object)?,
            tagging_directive: self.tagging_directive,
            tags,
            checksum_algorithm: self.checksum_algorithm,
        })
    }
}

/// Detached facts from this copy's confirmed publication, independent of later writes.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CopyObjectOutput {
    facts: PutObjectOutput,
    last_modified: Timestamp,
}

impl CopyObjectOutput {
    /// Quoted opaque destination entity tag.
    #[must_use]
    pub fn etag(&self) -> &str {
        self.facts.etag()
    }

    /// Timestamp captured by this publication, including after outcome resolution.
    #[must_use]
    pub const fn last_modified(&self) -> Timestamp {
        self.last_modified
    }

    /// Optional checksum over the entire copied destination.
    #[must_use]
    pub fn checksum_crc32(&self) -> Option<&str> {
        self.facts.checksum_crc32()
    }

    /// Optional checksum over the entire copied destination.
    #[must_use]
    pub fn checksum_crc32c(&self) -> Option<&str> {
        self.facts.checksum_crc32c()
    }

    /// Optional checksum over the entire copied destination.
    #[must_use]
    pub fn checksum_crc64_nvme(&self) -> Option<&str> {
        self.facts.checksum_crc64_nvme()
    }

    /// Optional checksum over the entire copied destination.
    #[must_use]
    pub fn checksum_sha1(&self) -> Option<&str> {
        self.facts.checksum_sha1()
    }

    /// Optional checksum over the entire copied destination.
    #[must_use]
    pub fn checksum_sha256(&self) -> Option<&str> {
        self.facts.checksum_sha256()
    }
}

impl crate::Store {
    /// Copy a selected local source to independent bytes, metadata and tags.
    /// Source selection is stable for this call; publication checks current
    /// destination conditions and close/cancellation fences.
    ///
    /// # Errors
    /// Reports source, condition, ownership, integrity and storage failures with
    /// mutation outcomes. An uncertain mutation is never automatically replayed.
    pub async fn copy_object(
        &self,
        input: CopyObjectInput,
    ) -> Result<CopyObjectOutput, StorageError> {
        let copied = self
            .connection
            .copy_object(input.try_into_core()?)
            .await
            .map_err(StorageError::from_operation)?;

        Ok(CopyObjectOutput {
            facts: PutObjectOutput::from_facts(&copied.facts),
            last_modified: Timestamp::from_core(copied.last_modified),
        })
    }
}
