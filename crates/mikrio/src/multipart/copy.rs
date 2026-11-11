use mikrio_core::multipart::{CopyRange, PartNumber};
use mikrio_core::object::{ReadConditions, ReadConditionsInput};

use crate::{ObjectRef, StorageError, Timestamp, UploadPartOutput};

/// One guarded local source selection for a numbered upload part.
#[derive(Debug, Clone)]
pub struct UploadPartCopyInput {
    upload: super::Upload,
    part_number: PartNumber,
    copy_source: ObjectRef,
    expected_source_bucket_owner: Option<String>,
    source_conditions: ReadConditions,
    copy_source_range: Option<String>,
    range: Option<CopyRange>,
}

/// Builder for a whole-source or closed-range part copy.
#[derive(Debug, Clone)]
pub struct UploadPartCopyInputBuilder {
    bucket: String,
    key: String,
    upload_id: String,
    part_number: u32,
    copy_source: ObjectRef,
    expected_bucket_owner: Option<String>,
    expected_source_bucket_owner: Option<String>,
    source_conditions: ReadConditionsInput,
    copy_source_range: Option<String>,
}

impl UploadPartCopyInput {
    /// Identify the destination part and a structured, unencoded local source.
    #[must_use]
    pub fn builder(
        bucket: impl Into<String>,
        key: impl Into<String>,
        upload_id: impl Into<String>,
        part_number: u32,
        copy_source: ObjectRef,
    ) -> UploadPartCopyInputBuilder {
        UploadPartCopyInputBuilder {
            bucket: bucket.into(),
            key: key.into(),
            upload_id: upload_id.into(),
            part_number,
            copy_source,
            expected_bucket_owner: None,
            expected_source_bucket_owner: None,
            source_conditions: ReadConditionsInput::default(),
            copy_source_range: None,
        }
    }

    /// Exact destination bucket.
    #[must_use]
    pub fn bucket(&self) -> &str {
        self.upload.object.bucket()
    }

    /// Exact destination key.
    #[must_use]
    pub fn key(&self) -> &str {
        self.upload.object.key()
    }

    /// Opaque upload identity.
    #[must_use]
    pub fn upload_id(&self) -> &str {
        &self.upload.token
    }

    /// Original part position.
    #[must_use]
    pub const fn part_number(&self) -> u32 {
        self.part_number.get()
    }

    /// Structured local source.
    #[must_use]
    pub const fn copy_source(&self) -> &ObjectRef {
        &self.copy_source
    }

    /// Supplied inclusive closed source range.
    #[must_use]
    pub fn copy_source_range(&self) -> Option<&str> {
        self.copy_source_range.as_deref()
    }

    /// Destination account assertion.
    #[must_use]
    pub fn expected_bucket_owner(&self) -> Option<&str> {
        self.upload.owner.as_deref()
    }

    /// Source account assertion.
    #[must_use]
    pub fn expected_source_bucket_owner(&self) -> Option<&str> {
        self.expected_source_bucket_owner.as_deref()
    }

    /// Supplied source condition.
    #[must_use]
    pub fn copy_source_if_match(&self) -> Option<&str> {
        self.source_conditions.fields().if_match.as_deref()
    }

    /// Supplied source condition.
    #[must_use]
    pub fn copy_source_if_none_match(&self) -> Option<&str> {
        self.source_conditions.fields().if_none_match.as_deref()
    }

    /// Source date condition normalized to whole seconds.
    #[must_use]
    pub fn copy_source_if_modified_since(&self) -> Option<Timestamp> {
        self.source_conditions
            .fields()
            .if_modified_since
            .map(Timestamp::from_core)
    }

    /// Source date condition normalized to whole seconds.
    #[must_use]
    pub fn copy_source_if_unmodified_since(&self) -> Option<Timestamp> {
        self.source_conditions
            .fields()
            .if_unmodified_since
            .map(Timestamp::from_core)
    }
}

impl UploadPartCopyInputBuilder {
    /// Set a literal request value; validated by `build`.
    #[must_use]
    pub fn expected_bucket_owner(mut self, value: impl Into<String>) -> Self {
        self.expected_bucket_owner = Some(value.into());
        self
    }

    /// Set a literal request value; validated by `build`.
    #[must_use]
    pub fn expected_source_bucket_owner(mut self, value: impl Into<String>) -> Self {
        self.expected_source_bucket_owner = Some(value.into());
        self
    }

    /// Set a literal request value; validated by `build`.
    #[must_use]
    pub fn copy_source_range(mut self, value: impl Into<String>) -> Self {
        self.copy_source_range = Some(value.into());
        self
    }

    /// Assert the selected source's entity tag.
    #[must_use]
    pub fn copy_source_if_match(mut self, value: impl Into<String>) -> Self {
        self.source_conditions.if_match = Some(value.into());
        self
    }

    /// Assert the selected source's entity tag.
    #[must_use]
    pub fn copy_source_if_none_match(mut self, value: impl Into<String>) -> Self {
        self.source_conditions.if_none_match = Some(value.into());
        self
    }

    /// Assert the selected source's modification time.
    #[must_use]
    pub const fn copy_source_if_modified_since(mut self, value: Timestamp) -> Self {
        self.source_conditions.if_modified_since = Some(value.into_core());
        self
    }

    /// Assert the selected source's modification time.
    #[must_use]
    pub const fn copy_source_if_unmodified_since(mut self, value: Timestamp) -> Self {
        self.source_conditions.if_unmodified_since = Some(value.into_core());
        self
    }

    /// Validate values without reading the source or opening the upload.
    ///
    /// # Errors
    /// Rejects invalid identities, owners, part positions, source conditions and range syntax.
    pub fn build(self) -> Result<UploadPartCopyInput, StorageError> {
        super::validate_owner(self.expected_source_bucket_owner.as_deref())?;

        let range = self
            .copy_source_range
            .as_deref()
            .map(CopyRange::parse)
            .transpose()
            .map_err(StorageError::from_object)?;

        Ok(UploadPartCopyInput {
            upload: super::Upload::new(
                &self.bucket,
                &self.key,
                self.upload_id,
                self.expected_bucket_owner,
            )?,
            part_number: PartNumber::new(self.part_number).map_err(StorageError::from_object)?,
            copy_source: self.copy_source,
            expected_source_bucket_owner: self.expected_source_bucket_owner,
            source_conditions: ReadConditions::new(self.source_conditions)
                .map_err(StorageError::from_object)?,
            copy_source_range: self.copy_source_range,
            range,
        })
    }
}

/// Confirmed independently stored part facts and publication instant.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct UploadPartCopyOutput {
    part: UploadPartOutput,
    last_modified: Timestamp,
}

impl UploadPartCopyOutput {
    /// Opaque quoted part entity tag.
    #[must_use]
    pub fn etag(&self) -> &str {
        self.part.etag()
    }

    /// Part publication instant.
    #[must_use]
    pub const fn last_modified(&self) -> Timestamp {
        self.last_modified
    }

    /// Verified checksum of the copied part bytes.
    #[must_use]
    pub fn checksum_crc32(&self) -> Option<&str> {
        self.part.checksum_crc32()
    }

    /// Verified checksum of the copied part bytes.
    #[must_use]
    pub fn checksum_crc32c(&self) -> Option<&str> {
        self.part.checksum_crc32c()
    }

    /// Verified checksum of the copied part bytes.
    #[must_use]
    pub fn checksum_crc64_nvme(&self) -> Option<&str> {
        self.part.checksum_crc64_nvme()
    }

    /// Verified checksum of the copied part bytes.
    #[must_use]
    pub fn checksum_sha1(&self) -> Option<&str> {
        self.part.checksum_sha1()
    }

    /// Verified checksum of the copied part bytes.
    #[must_use]
    pub fn checksum_sha256(&self) -> Option<&str> {
        self.part.checksum_sha256()
    }
}

impl crate::Store {
    /// Copy a guarded source or closed slice into an independent upload part.
    ///
    /// # Errors
    /// Reports source, range, upload, integrity and storage failures with mutation outcome.
    pub async fn upload_part_copy(
        &self,
        input: UploadPartCopyInput,
    ) -> Result<UploadPartCopyOutput, StorageError> {
        let upload = input.upload.into_core();
        let (source_bucket, source_key) = input.copy_source.into_core();

        let output = self
            .connection
            .upload_part_copy(mikrio_core::multipart::CopyPartRequest {
                bucket: upload.bucket,
                key: upload.key,
                upload_id: upload.upload_id,
                expected_owner: upload.expected_owner,
                part_number: input.part_number,
                source: mikrio_core::object::ObjectRef {
                    bucket: source_bucket,
                    key: source_key,
                },
                expected_source_owner: input.expected_source_bucket_owner,
                source_conditions: input.source_conditions,
                source_range: input.range,
            })
            .await
            .map_err(StorageError::from_operation)?;

        Ok(UploadPartCopyOutput {
            part: UploadPartOutput::from_facts(&output.facts),
            last_modified: Timestamp::from_core(output.last_modified),
        })
    }
}
