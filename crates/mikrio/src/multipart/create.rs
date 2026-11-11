use std::collections::BTreeMap;

use mikrio_core::multipart::MultipartChecksum;
use mikrio_core::object::{ObjectTags, Representation, RepresentationInput, UserMetadata};

use crate::{ChecksumAlgorithm, ChecksumType, ObjectRef, StorageError, Tag, Timestamp};

/// Owned initiation values, retained unchanged until completion.
#[derive(Debug, Clone)]
pub struct CreateMultipartUploadInput {
    object: ObjectRef,
    expected_bucket_owner: Option<String>,
    representation: Representation,
    metadata: UserMetadata,
    tags: Option<Vec<Tag>>,
    checksum: MultipartChecksum,
}

/// Builder for an upload's immutable representation and checksum selection.
#[derive(Debug, Clone)]
pub struct CreateMultipartUploadInputBuilder {
    bucket: String,
    key: String,
    expected_bucket_owner: Option<String>,
    representation: RepresentationInput,
    metadata: Vec<(String, String)>,
    tags: Option<Vec<Tag>>,
    checksum_algorithm: Option<ChecksumAlgorithm>,
    checksum_type: Option<ChecksumType>,
}

impl CreateMultipartUploadInput {
    /// Start an upload with a CRC64NVME full-object default.
    #[must_use]
    pub fn builder(
        bucket: impl Into<String>,
        key: impl Into<String>,
    ) -> CreateMultipartUploadInputBuilder {
        CreateMultipartUploadInputBuilder {
            bucket: bucket.into(),
            key: key.into(),
            expected_bucket_owner: None,
            representation: RepresentationInput::default(),
            metadata: Vec::new(),
            tags: None,
            checksum_algorithm: None,
            checksum_type: None,
        }
    }

    /// Exact bucket name.
    #[must_use]
    pub fn bucket(&self) -> &str {
        self.object.bucket()
    }

    /// Exact object key.
    #[must_use]
    pub fn key(&self) -> &str {
        self.object.key()
    }

    /// Account assertion, without authorization.
    #[must_use]
    pub fn expected_bucket_owner(&self) -> Option<&str> {
        self.expected_bucket_owner.as_deref()
    }

    /// Validated lowercase metadata keys.
    #[must_use]
    pub const fn metadata(&self) -> &BTreeMap<String, String> {
        self.metadata.entries()
    }

    /// Complete tag set; omission and empty are preserved.
    #[must_use]
    pub fn tags(&self) -> Option<&[Tag]> {
        self.tags.as_deref()
    }

    /// Resolved immutable algorithm.
    #[must_use]
    pub const fn checksum_algorithm(&self) -> ChecksumAlgorithm {
        ChecksumAlgorithm::from_core(self.checksum.algorithm())
    }

    /// Resolved immutable checksum type.
    #[must_use]
    pub const fn checksum_type(&self) -> ChecksumType {
        ChecksumType::from_core(self.checksum.checksum_type())
    }

    /// Supplied representation value, preserving empty strings.
    #[must_use]
    pub fn content_type(&self) -> Option<&str> {
        self.representation.fields().content_type.as_deref()
    }

    /// Supplied representation value, preserving empty strings.
    #[must_use]
    pub fn content_encoding(&self) -> Option<&str> {
        self.representation.fields().content_encoding.as_deref()
    }

    /// Supplied representation value, preserving empty strings.
    #[must_use]
    pub fn content_disposition(&self) -> Option<&str> {
        self.representation.fields().content_disposition.as_deref()
    }

    /// Supplied representation value, preserving empty strings.
    #[must_use]
    pub fn content_language(&self) -> Option<&str> {
        self.representation.fields().content_language.as_deref()
    }

    /// Supplied representation value, preserving empty strings.
    #[must_use]
    pub fn cache_control(&self) -> Option<&str> {
        self.representation.fields().cache_control.as_deref()
    }

    /// Cache expiry instant, unrelated to retention.
    #[must_use]
    pub fn expires(&self) -> Option<Timestamp> {
        self.representation
            .fields()
            .expires
            .map(Timestamp::from_core)
    }
}

impl CreateMultipartUploadInputBuilder {
    /// Set a representation field; validated by `build`.
    #[must_use]
    pub fn content_type(mut self, value: impl Into<String>) -> Self {
        self.representation.content_type = Some(value.into());
        self
    }

    /// Set a representation field; validated by `build`.
    #[must_use]
    pub fn content_encoding(mut self, value: impl Into<String>) -> Self {
        self.representation.content_encoding = Some(value.into());
        self
    }

    /// Set a representation field; validated by `build`.
    #[must_use]
    pub fn content_disposition(mut self, value: impl Into<String>) -> Self {
        self.representation.content_disposition = Some(value.into());
        self
    }

    /// Set a representation field; validated by `build`.
    #[must_use]
    pub fn content_language(mut self, value: impl Into<String>) -> Self {
        self.representation.content_language = Some(value.into());
        self
    }

    /// Set a representation field; validated by `build`.
    #[must_use]
    pub fn cache_control(mut self, value: impl Into<String>) -> Self {
        self.representation.cache_control = Some(value.into());
        self
    }

    /// Assert destination ownership.
    #[must_use]
    pub fn expected_bucket_owner(mut self, value: impl Into<String>) -> Self {
        self.expected_bucket_owner = Some(value.into());
        self
    }

    /// Set cache expiry.
    #[must_use]
    pub const fn expires(mut self, value: Timestamp) -> Self {
        self.representation.expires = Some(value.into_core());
        self
    }

    /// Replace all metadata entries; duplicate folded keys are invalid.
    #[must_use]
    pub fn metadata(mut self, values: impl IntoIterator<Item = (String, String)>) -> Self {
        self.metadata = values.into_iter().collect();
        self
    }

    /// Replace all tags, preserving an explicit empty set.
    #[must_use]
    pub fn tags(mut self, values: impl IntoIterator<Item = Tag>) -> Self {
        self.tags = Some(values.into_iter().collect());
        self
    }

    /// Select the upload's algorithm.
    #[must_use]
    pub const fn checksum_algorithm(mut self, value: ChecksumAlgorithm) -> Self {
        self.checksum_algorithm = Some(value);
        self
    }

    /// Select full-object or composite integrity.
    #[must_use]
    pub const fn checksum_type(mut self, value: ChecksumType) -> Self {
        self.checksum_type = Some(value);
        self
    }

    /// Validate values without accessing storage.
    ///
    /// # Errors
    /// Rejects invalid identities, representation, tags and checksum pairs.
    pub fn build(self) -> Result<CreateMultipartUploadInput, StorageError> {
        super::validate_owner(self.expected_bucket_owner.as_deref())?;

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

        let checksum = MultipartChecksum::resolve(
            self.checksum_algorithm.map(ChecksumAlgorithm::into_core),
            self.checksum_type.map(ChecksumType::into_core),
        )
        .map_err(StorageError::from_integrity_input)?;

        Ok(CreateMultipartUploadInput {
            object: ObjectRef::new(&self.bucket, &self.key)?,
            expected_bucket_owner: self.expected_bucket_owner,
            representation: Representation::new(self.representation)
                .map_err(StorageError::from_object)?,
            metadata: UserMetadata::new(self.metadata).map_err(StorageError::from_object)?,
            tags,
            checksum,
        })
    }
}

/// Confirmed upload identity and its immutable integrity selection.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CreateMultipartUploadOutput {
    upload_id: String,
    checksum_algorithm: ChecksumAlgorithm,
    checksum_type: ChecksumType,
}

impl CreateMultipartUploadOutput {
    /// Opaque upload identity; it supplies no authority.
    #[must_use]
    pub fn upload_id(&self) -> &str {
        &self.upload_id
    }

    /// Negotiated algorithm.
    #[must_use]
    pub const fn checksum_algorithm(&self) -> ChecksumAlgorithm {
        self.checksum_algorithm
    }

    /// Negotiated integrity mode.
    #[must_use]
    pub const fn checksum_type(&self) -> ChecksumType {
        self.checksum_type
    }
}

impl crate::Store {
    /// Durably initiate an upload without publishing an object.
    ///
    /// # Errors
    /// Reports owner, bucket, lifecycle and storage failures with mutation outcome.
    pub async fn create_multipart_upload(
        &self,
        input: CreateMultipartUploadInput,
    ) -> Result<CreateMultipartUploadOutput, StorageError> {
        let (bucket, key) = input.object.into_core();

        let tags = input
            .tags
            .map(|tags| ObjectTags::new(tags.into_iter().map(|tag| (tag.key, tag.value))))
            .transpose()
            .map_err(StorageError::from_object)?;

        let output = self
            .connection
            .create_multipart_upload(mikrio_core::multipart::CreateRequest {
                bucket,
                key,
                expected_owner: input.expected_bucket_owner,
                representation: input.representation,
                metadata: input.metadata,
                tags,
                checksum: input.checksum,
            })
            .await
            .map_err(StorageError::from_operation)?;

        Ok(CreateMultipartUploadOutput {
            upload_id: output.upload_id.to_string(),
            checksum_algorithm: ChecksumAlgorithm::from_core(output.checksum.algorithm()),
            checksum_type: ChecksumType::from_core(output.checksum.checksum_type()),
        })
    }
}
