use mikrio_core::integrity::{PutIntegrity, PutIntegrityInput};
use mikrio_core::multipart::PartNumber;
use mikrio_core::object::ByteLength;

use crate::{BodySource, ChecksumAlgorithm, PutObjectOutput, StorageError};

/// One owned part source; construction never polls it.
#[derive(Debug)]
pub struct UploadPartInput {
    upload: super::Upload,
    part_number: PartNumber,
    body: BodySource,
    content_length: Option<ByteLength>,
    integrity: PutIntegrity,
}

/// Builder for a part replacement.
#[derive(Debug)]
pub struct UploadPartInputBuilder {
    bucket: String,
    key: String,
    upload_id: String,
    part_number: u32,
    expected_bucket_owner: Option<String>,
    body: BodySource,
    content_length: Option<u64>,
    integrity: PutIntegrityInput,
}

impl UploadPartInput {
    /// Own a source for one numbered part of an active upload.
    #[must_use]
    pub fn builder(
        bucket: impl Into<String>,
        key: impl Into<String>,
        upload_id: impl Into<String>,
        part_number: u32,
        body: impl Into<BodySource>,
    ) -> UploadPartInputBuilder {
        UploadPartInputBuilder {
            bucket: bucket.into(),
            key: key.into(),
            upload_id: upload_id.into(),
            part_number,
            expected_bucket_owner: None,
            body: body.into(),
            content_length: None,
            integrity: PutIntegrityInput::default(),
        }
    }

    /// Exact bucket name.
    #[must_use]
    pub fn bucket(&self) -> &str {
        self.upload.object.bucket()
    }

    /// Exact object key.
    #[must_use]
    pub fn key(&self) -> &str {
        self.upload.object.key()
    }

    /// Opaque upload identity.
    #[must_use]
    pub fn upload_id(&self) -> &str {
        &self.upload.token
    }

    /// Account assertion without authorization.
    #[must_use]
    pub fn expected_bucket_owner(&self) -> Option<&str> {
        self.upload.owner.as_deref()
    }

    /// Original part position, 1 through 10,000.
    #[must_use]
    pub const fn part_number(&self) -> u32 {
        self.part_number.get()
    }

    /// Declared byte count, verified during execution.
    #[must_use]
    pub fn content_length(&self) -> Option<u64> {
        self.content_length.map(ByteLength::get)
    }

    /// Borrow the source without polling it.
    pub fn body_mut(&mut self) -> &mut BodySource {
        &mut self.body
    }

    /// Recover the source while discarding the request.
    #[must_use]
    pub fn into_body(self) -> BodySource {
        self.body
    }

    /// Explicit algorithm assertion; omission uses the upload's selection.
    #[must_use]
    pub fn checksum_algorithm(&self) -> Option<ChecksumAlgorithm> {
        self.integrity
            .fields()
            .checksum_algorithm
            .map(ChecksumAlgorithm::from_core)
    }

    /// Supplied digest assertion.
    #[must_use]
    pub fn content_md5(&self) -> Option<&str> {
        self.integrity.fields().content_md5.as_deref()
    }

    /// Supplied digest assertion.
    #[must_use]
    pub fn checksum_crc32(&self) -> Option<&str> {
        self.integrity.fields().checksum_crc32.as_deref()
    }

    /// Supplied digest assertion.
    #[must_use]
    pub fn checksum_crc32c(&self) -> Option<&str> {
        self.integrity.fields().checksum_crc32c.as_deref()
    }

    /// Supplied digest assertion.
    #[must_use]
    pub fn checksum_crc64_nvme(&self) -> Option<&str> {
        self.integrity.fields().checksum_crc64_nvme.as_deref()
    }

    /// Supplied digest assertion.
    #[must_use]
    pub fn checksum_sha1(&self) -> Option<&str> {
        self.integrity.fields().checksum_sha1.as_deref()
    }

    /// Supplied digest assertion.
    #[must_use]
    pub fn checksum_sha256(&self) -> Option<&str> {
        self.integrity.fields().checksum_sha256.as_deref()
    }
}

impl UploadPartInputBuilder {
    /// Assert destination ownership.
    #[must_use]
    pub fn expected_bucket_owner(mut self, value: impl Into<String>) -> Self {
        self.expected_bucket_owner = Some(value.into());
        self
    }

    /// Declare the exact source length.
    #[must_use]
    pub const fn content_length(mut self, value: u64) -> Self {
        self.content_length = Some(value);
        self
    }

    /// Assert the negotiated algorithm without changing it.
    #[must_use]
    pub const fn checksum_algorithm(mut self, value: ChecksumAlgorithm) -> Self {
        self.integrity.checksum_algorithm = Some(value.into_core());
        self
    }

    /// Set a canonical base64 digest assertion.
    #[must_use]
    pub fn content_md5(mut self, value: impl Into<String>) -> Self {
        self.integrity.content_md5 = Some(value.into());
        self
    }

    /// Set a canonical base64 digest assertion.
    #[must_use]
    pub fn checksum_crc32(mut self, value: impl Into<String>) -> Self {
        self.integrity.checksum_crc32 = Some(value.into());
        self
    }

    /// Set a canonical base64 digest assertion.
    #[must_use]
    pub fn checksum_crc32c(mut self, value: impl Into<String>) -> Self {
        self.integrity.checksum_crc32c = Some(value.into());
        self
    }

    /// Set a canonical base64 digest assertion.
    #[must_use]
    pub fn checksum_crc64_nvme(mut self, value: impl Into<String>) -> Self {
        self.integrity.checksum_crc64_nvme = Some(value.into());
        self
    }

    /// Set a canonical base64 digest assertion.
    #[must_use]
    pub fn checksum_sha1(mut self, value: impl Into<String>) -> Self {
        self.integrity.checksum_sha1 = Some(value.into());
        self
    }

    /// Set a canonical base64 digest assertion.
    #[must_use]
    pub fn checksum_sha256(mut self, value: impl Into<String>) -> Self {
        self.integrity.checksum_sha256 = Some(value.into());
        self
    }

    /// Validate values without polling the source.
    ///
    /// # Errors
    /// Rejects invalid identities, part numbers, lengths, owners and digest encodings.
    pub fn build(self) -> Result<UploadPartInput, StorageError> {
        Ok(UploadPartInput {
            upload: super::Upload::new(
                &self.bucket,
                &self.key,
                self.upload_id,
                self.expected_bucket_owner,
            )?,
            part_number: PartNumber::new(self.part_number).map_err(StorageError::from_object)?,
            body: self.body,
            content_length: self
                .content_length
                .map(ByteLength::for_put)
                .transpose()
                .map_err(StorageError::from_object)?,
            integrity: PutIntegrity::new(self.integrity)
                .map_err(StorageError::from_integrity_input)?,
        })
    }
}

/// Confirmed, verified part facts. This does not publish the completed object.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct UploadPartOutput {
    facts: PutObjectOutput,
}

impl UploadPartOutput {
    /// Opaque quoted part entity tag.
    #[must_use]
    pub fn etag(&self) -> &str {
        self.facts.etag()
    }

    /// Verified part checksum in canonical base64.
    #[must_use]
    pub fn checksum_crc32(&self) -> Option<&str> {
        self.facts.checksum_crc32()
    }

    /// Verified part checksum in canonical base64.
    #[must_use]
    pub fn checksum_crc32c(&self) -> Option<&str> {
        self.facts.checksum_crc32c()
    }

    /// Verified part checksum in canonical base64.
    #[must_use]
    pub fn checksum_crc64_nvme(&self) -> Option<&str> {
        self.facts.checksum_crc64_nvme()
    }

    /// Verified part checksum in canonical base64.
    #[must_use]
    pub fn checksum_sha1(&self) -> Option<&str> {
        self.facts.checksum_sha1()
    }

    /// Verified part checksum in canonical base64.
    #[must_use]
    pub fn checksum_sha256(&self) -> Option<&str> {
        self.facts.checksum_sha256()
    }

    pub(crate) fn from_facts(facts: &mikrio_core::integrity::PayloadFacts) -> Self {
        Self {
            facts: PutObjectOutput::from_facts(facts),
        }
    }
}

impl crate::Store {
    /// Durably replace one part, retaining the upload's immutable selection.
    ///
    /// # Errors
    /// Reports identity, state, source, integrity and storage failures with mutation outcome.
    pub async fn upload_part(
        &self,
        input: UploadPartInput,
    ) -> Result<UploadPartOutput, StorageError> {
        let upload = input.upload.into_core();

        let request = mikrio_core::multipart::PartRequest {
            bucket: upload.bucket,
            key: upload.key,
            upload_id: upload.upload_id,
            expected_owner: upload.expected_owner,
            part_number: input.part_number,
            content_length: input.content_length,
            checksum_algorithm: input.integrity.fields().checksum_algorithm,
            expected: input.integrity.expected(),
        };

        let facts = self
            .connection
            .upload_part(request, input.body)
            .await
            .map_err(StorageError::from_operation)?;

        Ok(UploadPartOutput::from_facts(&facts))
    }
}
