use mikrio_core::integrity::{ObjectChecksum, PutIntegrity, PutIntegrityInput};
use mikrio_core::multipart::{CompletionParts, PartNumber, SelectedPart};
use mikrio_core::object::{ByteLength, WriteConditions};

use crate::{ChecksumAlgorithm, ChecksumType, StorageError};

/// An original part number, quoted `ETag` and optional checksum acknowledged by its producer.
#[derive(Debug, Clone)]
pub struct CompletedPart {
    selected: SelectedPart,
    checksums: PutIntegrity,
}

/// Builder for one completion selection entry.
#[derive(Debug, Clone)]
pub struct CompletedPartBuilder {
    part_number: u32,
    etag: String,
    checksums: PutIntegrityInput,
}

impl CompletedPart {
    /// Identify one original committed part; construction performs no lookup.
    #[must_use]
    pub fn builder(part_number: u32, etag: impl Into<String>) -> CompletedPartBuilder {
        CompletedPartBuilder {
            part_number,
            etag: etag.into(),
            checksums: PutIntegrityInput::default(),
        }
    }

    /// Original part number.
    #[must_use]
    pub const fn part_number(&self) -> u32 {
        self.selected.part_number.get()
    }

    /// Supplied quoted entity tag.
    #[must_use]
    pub fn etag(&self) -> &str {
        &self.selected.etag
    }

    /// Supplied part checksum assertion.
    #[must_use]
    pub fn checksum_crc32(&self) -> Option<&str> {
        self.checksums.fields().checksum_crc32.as_deref()
    }

    /// Supplied part checksum assertion.
    #[must_use]
    pub fn checksum_crc32c(&self) -> Option<&str> {
        self.checksums.fields().checksum_crc32c.as_deref()
    }

    /// Supplied part checksum assertion.
    #[must_use]
    pub fn checksum_crc64_nvme(&self) -> Option<&str> {
        self.checksums.fields().checksum_crc64_nvme.as_deref()
    }

    /// Supplied part checksum assertion.
    #[must_use]
    pub fn checksum_sha1(&self) -> Option<&str> {
        self.checksums.fields().checksum_sha1.as_deref()
    }

    /// Supplied part checksum assertion.
    #[must_use]
    pub fn checksum_sha256(&self) -> Option<&str> {
        self.checksums.fields().checksum_sha256.as_deref()
    }
}

impl CompletedPartBuilder {
    /// Set the canonical base64 checksum for this part.
    #[must_use]
    pub fn checksum_crc32(mut self, value: impl Into<String>) -> Self {
        self.checksums.checksum_crc32 = Some(value.into());
        self
    }

    /// Set the canonical base64 checksum for this part.
    #[must_use]
    pub fn checksum_crc32c(mut self, value: impl Into<String>) -> Self {
        self.checksums.checksum_crc32c = Some(value.into());
        self
    }

    /// Set the canonical base64 checksum for this part.
    #[must_use]
    pub fn checksum_crc64_nvme(mut self, value: impl Into<String>) -> Self {
        self.checksums.checksum_crc64_nvme = Some(value.into());
        self
    }

    /// Set the canonical base64 checksum for this part.
    #[must_use]
    pub fn checksum_sha1(mut self, value: impl Into<String>) -> Self {
        self.checksums.checksum_sha1 = Some(value.into());
        self
    }

    /// Set the canonical base64 checksum for this part.
    #[must_use]
    pub fn checksum_sha256(mut self, value: impl Into<String>) -> Self {
        self.checksums.checksum_sha256 = Some(value.into());
        self
    }

    /// Validate one part's identity and assertions without storage access.
    ///
    /// # Errors
    /// Rejects malformed part numbers, entity tags and checksum values.
    pub fn build(self) -> Result<CompletedPart, StorageError> {
        let checksums =
            PutIntegrity::new(self.checksums).map_err(StorageError::from_integrity_input)?;

        let selected = SelectedPart {
            part_number: PartNumber::new(self.part_number).map_err(StorageError::from_object)?,
            etag: self.etag,
            checksum: checksums.expected().checksum(),
        };

        CompletionParts::new([selected.clone()]).map_err(StorageError::from_operation)?;

        Ok(CompletedPart {
            selected,
            checksums,
        })
    }
}

/// Owned bounded completion selection, independent of current object metadata.
#[derive(Debug, Clone)]
pub struct CompleteMultipartUploadInput {
    upload: super::Upload,
    parts: Vec<CompletedPart>,
    conditions: WriteConditions,
    content_length: Option<ByteLength>,
    checksum_type: Option<ChecksumType>,
    checksum: Option<ObjectChecksum>,
    checksum_value: String,
}

/// Builder for an atomic completion attempt.
#[derive(Debug, Clone)]
pub struct CompleteMultipartUploadInputBuilder {
    bucket: String,
    key: String,
    upload_id: String,
    expected_bucket_owner: Option<String>,
    parts: Vec<CompletedPart>,
    if_match: Option<String>,
    if_none_match: Option<String>,
    content_length: Option<u64>,
    checksum_type: Option<ChecksumType>,
    checksum_crc32: Option<String>,
    checksum_crc32c: Option<String>,
    checksum_crc64_nvme: Option<String>,
    checksum_sha1: Option<String>,
    checksum_sha256: Option<String>,
}

impl CompleteMultipartUploadInput {
    /// Select acknowledged parts in ascending original-number order.
    #[must_use]
    pub fn builder(
        bucket: impl Into<String>,
        key: impl Into<String>,
        upload_id: impl Into<String>,
        parts: Vec<CompletedPart>,
    ) -> CompleteMultipartUploadInputBuilder {
        CompleteMultipartUploadInputBuilder {
            bucket: bucket.into(),
            key: key.into(),
            upload_id: upload_id.into(),
            expected_bucket_owner: None,
            parts,
            if_match: None,
            if_none_match: None,
            content_length: None,
            checksum_type: None,
            checksum_crc32: None,
            checksum_crc32c: None,
            checksum_crc64_nvme: None,
            checksum_sha1: None,
            checksum_sha256: None,
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

    /// Account assertion.
    #[must_use]
    pub fn expected_bucket_owner(&self) -> Option<&str> {
        self.upload.owner.as_deref()
    }

    /// Ascending selected parts.
    #[must_use]
    pub fn parts(&self) -> &[CompletedPart] {
        &self.parts
    }

    /// Atomic destination assertion.
    #[must_use]
    pub fn if_match(&self) -> Option<&str> {
        self.conditions.if_match()
    }

    /// Atomic create-only assertion.
    #[must_use]
    pub fn if_none_match(&self) -> Option<&str> {
        self.conditions.if_none_match()
    }

    /// Expected assembled byte length.
    #[must_use]
    pub fn content_length(&self) -> Option<u64> {
        self.content_length.map(ByteLength::get)
    }

    /// Optional assertion of the immutable upload mode.
    #[must_use]
    pub const fn checksum_type(&self) -> Option<ChecksumType> {
        self.checksum_type
    }

    /// Supplied completed-object checksum assertion.
    #[must_use]
    pub fn checksum_crc32(&self) -> Option<&str> {
        self.checksum
            .as_ref()
            .filter(|checksum| {
                checksum.digest().algorithm() == mikrio_core::integrity::ChecksumAlgorithm::Crc32
            })
            .map(|_| self.checksum_value.as_str())
    }

    /// Supplied completed-object checksum assertion.
    #[must_use]
    pub fn checksum_crc32c(&self) -> Option<&str> {
        self.checksum
            .as_ref()
            .filter(|checksum| {
                checksum.digest().algorithm() == mikrio_core::integrity::ChecksumAlgorithm::Crc32c
            })
            .map(|_| self.checksum_value.as_str())
    }

    /// Supplied completed-object checksum assertion.
    #[must_use]
    pub fn checksum_crc64_nvme(&self) -> Option<&str> {
        self.checksum
            .as_ref()
            .filter(|checksum| {
                checksum.digest().algorithm()
                    == mikrio_core::integrity::ChecksumAlgorithm::Crc64Nvme
            })
            .map(|_| self.checksum_value.as_str())
    }

    /// Supplied completed-object checksum assertion.
    #[must_use]
    pub fn checksum_sha1(&self) -> Option<&str> {
        self.checksum
            .as_ref()
            .filter(|checksum| {
                checksum.digest().algorithm() == mikrio_core::integrity::ChecksumAlgorithm::Sha1
            })
            .map(|_| self.checksum_value.as_str())
    }

    /// Supplied completed-object checksum assertion.
    #[must_use]
    pub fn checksum_sha256(&self) -> Option<&str> {
        self.checksum
            .as_ref()
            .filter(|checksum| {
                checksum.digest().algorithm() == mikrio_core::integrity::ChecksumAlgorithm::Sha256
            })
            .map(|_| self.checksum_value.as_str())
    }
}

impl CompleteMultipartUploadInputBuilder {
    /// Set an assertion, validated by `build` and checked at publication.
    #[must_use]
    pub fn expected_bucket_owner(mut self, value: impl Into<String>) -> Self {
        self.expected_bucket_owner = Some(value.into());
        self
    }

    /// Set an assertion, validated by `build` and checked at publication.
    #[must_use]
    pub fn if_match(mut self, value: impl Into<String>) -> Self {
        self.if_match = Some(value.into());
        self
    }

    /// Set an assertion, validated by `build` and checked at publication.
    #[must_use]
    pub fn if_none_match(mut self, value: impl Into<String>) -> Self {
        self.if_none_match = Some(value.into());
        self
    }

    /// Set an assertion, validated by `build` and checked at publication.
    #[must_use]
    pub fn checksum_crc32(mut self, value: impl Into<String>) -> Self {
        self.checksum_crc32 = Some(value.into());
        self
    }

    /// Set an assertion, validated by `build` and checked at publication.
    #[must_use]
    pub fn checksum_crc32c(mut self, value: impl Into<String>) -> Self {
        self.checksum_crc32c = Some(value.into());
        self
    }

    /// Set an assertion, validated by `build` and checked at publication.
    #[must_use]
    pub fn checksum_crc64_nvme(mut self, value: impl Into<String>) -> Self {
        self.checksum_crc64_nvme = Some(value.into());
        self
    }

    /// Set an assertion, validated by `build` and checked at publication.
    #[must_use]
    pub fn checksum_sha1(mut self, value: impl Into<String>) -> Self {
        self.checksum_sha1 = Some(value.into());
        self
    }

    /// Set an assertion, validated by `build` and checked at publication.
    #[must_use]
    pub fn checksum_sha256(mut self, value: impl Into<String>) -> Self {
        self.checksum_sha256 = Some(value.into());
        self
    }

    /// Assert the exact assembled length, independently of request encoding.
    #[must_use]
    pub const fn content_length(mut self, value: u64) -> Self {
        self.content_length = Some(value);
        self
    }

    /// Assert the upload's checksum type without renegotiating it.
    #[must_use]
    pub const fn checksum_type(mut self, value: ChecksumType) -> Self {
        self.checksum_type = Some(value);
        self
    }

    /// Validate the bounded selection without opening source files.
    ///
    /// # Errors
    /// Rejects invalid identities, empty/reordered selections, conditions and digests.
    pub fn build(self) -> Result<CompleteMultipartUploadInput, StorageError> {
        CompletionParts::new(self.parts.iter().map(|part| part.selected.clone()))
            .map_err(StorageError::from_operation)?;

        let mut checksum = None;
        let mut checksum_value = String::new();

        for (algorithm, value) in [
            (ChecksumAlgorithm::Crc32, self.checksum_crc32),
            (ChecksumAlgorithm::Crc32c, self.checksum_crc32c),
            (ChecksumAlgorithm::Crc64Nvme, self.checksum_crc64_nvme),
            (ChecksumAlgorithm::Sha1, self.checksum_sha1),
            (ChecksumAlgorithm::Sha256, self.checksum_sha256),
        ] {
            let Some(value) = value else { continue };

            if checksum.is_some() {
                return Err(StorageError::from_integrity_input(
                    mikrio_core::integrity::IntegrityError::AlgorithmMismatch,
                ));
            }

            let mode = self.checksum_type.unwrap_or(if value.contains('-') {
                ChecksumType::Composite
            } else {
                ChecksumType::FullObject
            });

            checksum = Some(
                ObjectChecksum::parse(algorithm.into_core(), mode.into_core(), &value)
                    .map_err(StorageError::from_integrity_input)?,
            );

            checksum_value = value;
        }

        Ok(CompleteMultipartUploadInput {
            upload: super::Upload::new(
                &self.bucket,
                &self.key,
                self.upload_id,
                self.expected_bucket_owner,
            )?,
            parts: self.parts,
            conditions: WriteConditions::new(self.if_match, self.if_none_match)
                .map_err(StorageError::from_object)?,
            content_length: self
                .content_length
                .map(ByteLength::new)
                .transpose()
                .map_err(StorageError::from_object)?,
            checksum_type: self.checksum_type,
            checksum,
            checksum_value,
        })
    }
}

/// Facts from one confirmed completion; retrying its upload ID is not a replay.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CompleteMultipartUploadOutput {
    etag: String,
    checksum_algorithm: ChecksumAlgorithm,
    checksum_type: ChecksumType,
    checksum: String,
}

impl CompleteMultipartUploadOutput {
    /// Opaque quoted multipart entity tag.
    #[must_use]
    pub fn etag(&self) -> &str {
        &self.etag
    }

    /// Meaning of the returned checksum.
    #[must_use]
    pub const fn checksum_type(&self) -> ChecksumType {
        self.checksum_type
    }

    /// Verified completed-object checksum, including its composite suffix when applicable.
    #[must_use]
    pub fn checksum_crc32(&self) -> Option<&str> {
        (self.checksum_algorithm == ChecksumAlgorithm::Crc32).then_some(self.checksum.as_str())
    }

    /// Verified completed-object checksum, including its composite suffix when applicable.
    #[must_use]
    pub fn checksum_crc32c(&self) -> Option<&str> {
        (self.checksum_algorithm == ChecksumAlgorithm::Crc32c).then_some(self.checksum.as_str())
    }

    /// Verified completed-object checksum, including its composite suffix when applicable.
    #[must_use]
    pub fn checksum_crc64_nvme(&self) -> Option<&str> {
        (self.checksum_algorithm == ChecksumAlgorithm::Crc64Nvme).then_some(self.checksum.as_str())
    }

    /// Verified completed-object checksum, including its composite suffix when applicable.
    #[must_use]
    pub fn checksum_sha1(&self) -> Option<&str> {
        (self.checksum_algorithm == ChecksumAlgorithm::Sha1).then_some(self.checksum.as_str())
    }

    /// Verified completed-object checksum, including its composite suffix when applicable.
    #[must_use]
    pub fn checksum_sha256(&self) -> Option<&str> {
        (self.checksum_algorithm == ChecksumAlgorithm::Sha256).then_some(self.checksum.as_str())
    }
}

impl crate::Store {
    /// Assemble exactly the selected parts and atomically publish the object.
    ///
    /// # Errors
    /// Reports selection, checksum, condition, upload state and storage failures with mutation outcome.
    pub async fn complete_multipart_upload(
        &self,
        input: CompleteMultipartUploadInput,
    ) -> Result<CompleteMultipartUploadOutput, StorageError> {
        let request = mikrio_core::multipart::CompleteRequest {
            upload: input.upload.into_core(),
            parts: CompletionParts::new(input.parts.into_iter().map(|part| part.selected))
                .map_err(StorageError::from_operation)?,
            conditions: input.conditions,
            content_length: input.content_length,
            checksum: input.checksum,
            checksum_type: input.checksum_type.map(ChecksumType::into_core),
        };

        let result = self
            .connection
            .complete_multipart_upload(request)
            .await
            .map_err(StorageError::from_operation)?;

        Ok(CompleteMultipartUploadOutput {
            etag: result.etag,
            checksum_algorithm: ChecksumAlgorithm::from_core(result.checksum.digest().algorithm()),
            checksum_type: ChecksumType::from_core(result.checksum.checksum_type()),
            checksum: result.checksum.encode(),
        })
    }
}
