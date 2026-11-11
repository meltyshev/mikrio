use std::collections::BTreeMap;

use mikrio_core::integrity::{ChecksumAlgorithm as Algorithm, PutIntegrity, PutIntegrityInput};
use mikrio_core::object::{
    ByteLength, ObjectTags, Representation, RepresentationInput, UserMetadata, WriteConditions,
};

use crate::{BodySource, ChecksumAlgorithm, ObjectRef, StorageError, Tag, Timestamp};

/// Owned upload input. Building validates values without polling the source.
/// It does not verify payload bytes, evaluate conditions or publish an object.
#[derive(Debug)]
pub struct PutObjectInput {
    object: ObjectRef,
    expected_bucket_owner: Option<String>,
    body: BodySource,
    content_length: Option<ByteLength>,
    representation: Representation,
    metadata: UserMetadata,
    tags: Option<Vec<Tag>>,
    integrity: PutIntegrity,
    conditions: WriteConditions,
}

impl PutObjectInput {
    /// Transfer the required literal identity and owned source into a builder.
    #[must_use]
    pub fn builder(
        bucket: impl Into<String>,
        key: impl Into<String>,
        body: impl Into<BodySource>,
    ) -> PutObjectInputBuilder {
        PutObjectInputBuilder {
            bucket: bucket.into(),
            key: key.into(),
            expected_bucket_owner: None,
            body: body.into(),
            content_length: None,
            representation: RepresentationInput::default(),
            metadata: Vec::new(),
            tags: None,
            integrity: PutIntegrityInput::default(),
            if_match: None,
            if_none_match: None,
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

    /// Borrow the source without copying, polling or rewinding it.
    pub fn body_mut(&mut self) -> &mut BodySource {
        &mut self.body
    }

    /// Recover the owned source, discarding the other request values.
    #[must_use]
    pub fn into_body(self) -> BodySource {
        self.body
    }

    /// Declared byte count, checked against actual bytes during execution.
    #[must_use]
    pub fn content_length(&self) -> Option<u64> {
        self.content_length.map(ByteLength::get)
    }

    /// Lowercase metadata keys with exact validated UTF-8 values.
    #[must_use]
    pub const fn metadata(&self) -> &BTreeMap<String, String> {
        self.metadata.entries()
    }

    /// Supplied tags in ascending UTF-8 key order, retaining omission versus empty.
    #[must_use]
    pub fn tags(&self) -> Option<&[Tag]> {
        self.tags.as_deref()
    }

    /// Supplied representation value; absence differs from an empty string.
    #[must_use]
    pub fn content_type(&self) -> Option<&str> {
        self.representation.fields().content_type.as_deref()
    }

    /// Supplied representation value; absence differs from an empty string.
    #[must_use]
    pub fn content_encoding(&self) -> Option<&str> {
        self.representation.fields().content_encoding.as_deref()
    }

    /// Supplied representation value; absence differs from an empty string.
    #[must_use]
    pub fn content_disposition(&self) -> Option<&str> {
        self.representation.fields().content_disposition.as_deref()
    }

    /// Supplied representation value; absence differs from an empty string.
    #[must_use]
    pub fn content_language(&self) -> Option<&str> {
        self.representation.fields().content_language.as_deref()
    }

    /// Supplied representation value; absence differs from an empty string.
    #[must_use]
    pub fn cache_control(&self) -> Option<&str> {
        self.representation.fields().cache_control.as_deref()
    }

    /// Supplied cache expiry, retaining millisecond precision.
    #[must_use]
    pub fn expires(&self) -> Option<Timestamp> {
        self.representation
            .fields()
            .expires
            .map(Timestamp::from_core)
    }

    /// Explicit algorithm selection, preserving omission.
    #[must_use]
    pub fn checksum_algorithm(&self) -> Option<ChecksumAlgorithm> {
        self.integrity
            .fields()
            .checksum_algorithm
            .map(ChecksumAlgorithm::from_core)
    }

    /// Supplied canonical padded base64 digest, validated before source consumption.
    #[must_use]
    pub fn content_md5(&self) -> Option<&str> {
        self.integrity.fields().content_md5.as_deref()
    }

    /// Supplied canonical padded base64 digest, validated before source consumption.
    #[must_use]
    pub fn checksum_crc32(&self) -> Option<&str> {
        self.integrity.fields().checksum_crc32.as_deref()
    }

    /// Supplied canonical padded base64 digest, validated before source consumption.
    #[must_use]
    pub fn checksum_crc32c(&self) -> Option<&str> {
        self.integrity.fields().checksum_crc32c.as_deref()
    }

    /// Supplied canonical padded base64 digest, validated before source consumption.
    #[must_use]
    pub fn checksum_crc64_nvme(&self) -> Option<&str> {
        self.integrity.fields().checksum_crc64_nvme.as_deref()
    }

    /// Supplied canonical padded base64 digest, validated before source consumption.
    #[must_use]
    pub fn checksum_sha1(&self) -> Option<&str> {
        self.integrity.fields().checksum_sha1.as_deref()
    }

    /// Supplied canonical padded base64 digest, validated before source consumption.
    #[must_use]
    pub fn checksum_sha256(&self) -> Option<&str> {
        self.integrity.fields().checksum_sha256.as_deref()
    }

    /// Optional destination `ETag` or existence wildcard.
    #[must_use]
    pub fn if_match(&self) -> Option<&str> {
        self.conditions.if_match()
    }

    /// Optional create-only wildcard.
    #[must_use]
    pub fn if_none_match(&self) -> Option<&str> {
        self.conditions.if_none_match()
    }
}

/// Builder for an owned upload. A failed build drops its source without polling it.
#[derive(Debug)]
pub struct PutObjectInputBuilder {
    bucket: String,
    key: String,
    expected_bucket_owner: Option<String>,
    body: BodySource,
    content_length: Option<u64>,
    representation: RepresentationInput,
    metadata: Vec<(String, String)>,
    tags: Option<Vec<Tag>>,
    integrity: PutIntegrityInput,
    if_match: Option<String>,
    if_none_match: Option<String>,
}

impl PutObjectInputBuilder {
    /// Assert ownership by this twelve-digit account, preserving leading zeroes.
    #[must_use]
    pub fn expected_bucket_owner(mut self, account: impl Into<String>) -> Self {
        self.expected_bucket_owner = Some(account.into());
        self
    }

    /// Declare up to 5 GiB without truncating, padding or reading the source.
    #[must_use]
    pub const fn content_length(mut self, length: u64) -> Self {
        self.content_length = Some(length);
        self
    }

    /// Supply entries before case folding so duplicate keys cannot be overwritten.
    #[must_use]
    pub fn metadata(mut self, entries: Vec<(String, String)>) -> Self {
        self.metadata = entries;
        self
    }

    /// Supply the whole new tag set; duplicate keys fail `build()`.
    #[must_use]
    pub fn tags(mut self, tags: Vec<Tag>) -> Self {
        self.tags = Some(tags);
        self
    }

    /// Set the optional representation header; validated by `build()`.
    #[must_use]
    pub fn content_type(mut self, value: impl Into<String>) -> Self {
        self.representation.content_type = Some(value.into());
        self
    }

    /// Set the optional representation header; validated by `build()`.
    #[must_use]
    pub fn content_encoding(mut self, value: impl Into<String>) -> Self {
        self.representation.content_encoding = Some(value.into());
        self
    }

    /// Set the optional representation header; validated by `build()`.
    #[must_use]
    pub fn content_disposition(mut self, value: impl Into<String>) -> Self {
        self.representation.content_disposition = Some(value.into());
        self
    }

    /// Set the optional representation header; validated by `build()`.
    #[must_use]
    pub fn content_language(mut self, value: impl Into<String>) -> Self {
        self.representation.content_language = Some(value.into());
        self
    }

    /// Set the optional representation header; validated by `build()`.
    #[must_use]
    pub fn cache_control(mut self, value: impl Into<String>) -> Self {
        self.representation.cache_control = Some(value.into());
        self
    }

    /// Set cache expiry without changing the instant or object retention.
    #[must_use]
    pub const fn expires(mut self, value: Timestamp) -> Self {
        self.representation.expires = Some(value.into_core());
        self
    }

    /// Select one flexible algorithm; a supplied digest must agree.
    #[must_use]
    pub const fn checksum_algorithm(mut self, algorithm: ChecksumAlgorithm) -> Self {
        self.integrity.checksum_algorithm = Some(algorithm.into_core());
        self
    }

    /// Set a canonical padded base64 expectation; validated by `build()`.
    #[must_use]
    pub fn content_md5(mut self, value: impl Into<String>) -> Self {
        self.integrity.content_md5 = Some(value.into());
        self
    }

    /// Set a canonical padded base64 expectation; validated by `build()`.
    #[must_use]
    pub fn checksum_crc32(mut self, value: impl Into<String>) -> Self {
        self.integrity.checksum_crc32 = Some(value.into());
        self
    }

    /// Set a canonical padded base64 expectation; validated by `build()`.
    #[must_use]
    pub fn checksum_crc32c(mut self, value: impl Into<String>) -> Self {
        self.integrity.checksum_crc32c = Some(value.into());
        self
    }

    /// Set a canonical padded base64 expectation; validated by `build()`.
    #[must_use]
    pub fn checksum_crc64_nvme(mut self, value: impl Into<String>) -> Self {
        self.integrity.checksum_crc64_nvme = Some(value.into());
        self
    }

    /// Set a canonical padded base64 expectation; validated by `build()`.
    #[must_use]
    pub fn checksum_sha1(mut self, value: impl Into<String>) -> Self {
        self.integrity.checksum_sha1 = Some(value.into());
        self
    }

    /// Set a canonical padded base64 expectation; validated by `build()`.
    #[must_use]
    pub fn checksum_sha256(mut self, value: impl Into<String>) -> Self {
        self.integrity.checksum_sha256 = Some(value.into());
        self
    }

    /// Require a destination `ETag` or existence wildcard at publication.
    #[must_use]
    pub fn if_match(mut self, value: impl Into<String>) -> Self {
        self.if_match = Some(value.into());
        self
    }

    /// Require the create-only `*` condition at publication.
    #[must_use]
    pub fn if_none_match(mut self, value: impl Into<String>) -> Self {
        self.if_none_match = Some(value.into());
        self
    }

    /// Validate owned values without consuming source bytes or opening a store.
    ///
    /// # Errors
    /// Returns `InvalidBucketName` for an invalid bucket, or `InvalidArgument`
    /// for invalid keys, owners, lengths, metadata, tags, headers, digests or conditions.
    /// Payload length/digest verification and condition evaluation occur during
    /// execution, with their own operation errors and mutation outcomes.
    pub fn build(self) -> Result<PutObjectInput, StorageError> {
        let object = ObjectRef::new(&self.bucket, &self.key)?;

        if let Some(account) = &self.expected_bucket_owner {
            mikrio_core::access::validate_account_id(account)
                .map_err(StorageError::from_identity)?;
        }

        let content_length = self
            .content_length
            .map(ByteLength::for_put)
            .transpose()
            .map_err(StorageError::from_object)?;

        let representation =
            Representation::new(self.representation).map_err(StorageError::from_object)?;

        let metadata = UserMetadata::new(self.metadata).map_err(StorageError::from_object)?;

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

        let integrity =
            PutIntegrity::new(self.integrity).map_err(StorageError::from_integrity_input)?;

        let conditions = WriteConditions::new(self.if_match, self.if_none_match)
            .map_err(StorageError::from_object)?;

        Ok(PutObjectInput {
            object,
            expected_bucket_owner: self.expected_bucket_owner,
            body: self.body,
            content_length,
            representation,
            metadata,
            tags,
            integrity,
            conditions,
        })
    }
}

/// Detached protocol facts returned after successful object publication.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PutObjectOutput {
    pub(crate) etag: String,
    pub(crate) checksum_crc32: Option<String>,
    pub(crate) checksum_crc32c: Option<String>,
    pub(crate) checksum_crc64_nvme: Option<String>,
    pub(crate) checksum_sha1: Option<String>,
    pub(crate) checksum_sha256: Option<String>,
}

impl PutObjectOutput {
    /// Quoted opaque entity tag; it is not a generation or commit witness.
    #[must_use]
    pub fn etag(&self) -> &str {
        &self.etag
    }

    /// Optional full-object checksum in canonical padded base64.
    #[must_use]
    pub fn checksum_crc32(&self) -> Option<&str> {
        self.checksum_crc32.as_deref()
    }

    /// Optional full-object checksum in canonical padded base64.
    #[must_use]
    pub fn checksum_crc32c(&self) -> Option<&str> {
        self.checksum_crc32c.as_deref()
    }

    /// Optional full-object checksum in canonical padded base64.
    #[must_use]
    pub fn checksum_crc64_nvme(&self) -> Option<&str> {
        self.checksum_crc64_nvme.as_deref()
    }

    /// Optional full-object checksum in canonical padded base64.
    #[must_use]
    pub fn checksum_sha1(&self) -> Option<&str> {
        self.checksum_sha1.as_deref()
    }

    /// Optional full-object checksum in canonical padded base64.
    #[must_use]
    pub fn checksum_sha256(&self) -> Option<&str> {
        self.checksum_sha256.as_deref()
    }

    pub(crate) fn from_facts(facts: &mikrio_core::integrity::PayloadFacts) -> Self {
        let checksum = facts.checksum();

        let mut result = PutObjectOutput {
            etag: facts.etag(),
            checksum_crc32: None,
            checksum_crc32c: None,
            checksum_crc64_nvme: None,
            checksum_sha1: None,
            checksum_sha256: None,
        };

        let target = match checksum.algorithm() {
            Algorithm::Crc32 => &mut result.checksum_crc32,
            Algorithm::Crc32c => &mut result.checksum_crc32c,
            Algorithm::Crc64Nvme => &mut result.checksum_crc64_nvme,
            Algorithm::Sha1 => &mut result.checksum_sha1,
            Algorithm::Sha256 => &mut result.checksum_sha256,
        };

        *target = Some(checksum.encode());
        result
    }
}

impl crate::Store {
    /// Publish the verified source durably, replacing the current object atomically.
    /// The source is consumed once with bounded reads and is never replayed.
    ///
    /// # Errors
    /// Reports source, length, digest, owner, condition and storage failures with
    /// their mutation outcome. Cancellation before commit leaves the old mapping.
    pub async fn put_object(&self, input: PutObjectInput) -> Result<PutObjectOutput, StorageError> {
        let (bucket, key) = input.object.into_core();

        let tags = input
            .tags
            .map(|tags| ObjectTags::new(tags.into_iter().map(|tag| (tag.key, tag.value))))
            .transpose()
            .map_err(StorageError::from_object)?;

        let request = mikrio_core::object::PutRequest {
            bucket,
            key,
            expected_owner: input.expected_bucket_owner,
            content_length: input.content_length,
            representation: input.representation,
            metadata: input.metadata,
            tags,
            integrity: input.integrity,
            conditions: input.conditions,
        };

        let facts = self
            .connection
            .put_object(request, input.body)
            .await
            .map_err(StorageError::from_operation)?;

        Ok(PutObjectOutput::from_facts(&facts))
    }
}
