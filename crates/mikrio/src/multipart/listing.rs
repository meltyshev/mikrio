use mikrio_core::multipart::{PartPageSize, UploadPageSize};
use mikrio_core::object::BucketName;

use crate::{ChecksumAlgorithm, ChecksumType, StorageError, Timestamp};

/// Validated request for `AbortMultipartUpload`.
#[derive(Debug, Clone)]
pub struct AbortMultipartUploadInput {
    upload: super::Upload,
}

/// Builder for `AbortMultipartUploadInput`.
#[derive(Debug, Clone)]
pub struct AbortMultipartUploadInputBuilder {
    bucket: String,
    key: String,
    upload_id: String,
    expected_bucket_owner: Option<String>,
}

impl AbortMultipartUploadInput {
    /// Identify one upload; an ID supplies no authority.
    #[must_use]
    pub fn builder(
        bucket: impl Into<String>,
        key: impl Into<String>,
        upload_id: impl Into<String>,
    ) -> AbortMultipartUploadInputBuilder {
        AbortMultipartUploadInputBuilder {
            bucket: bucket.into(),
            key: key.into(),
            upload_id: upload_id.into(),
            expected_bucket_owner: None,
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
}

impl AbortMultipartUploadInputBuilder {
    /// Assert destination ownership.
    #[must_use]
    pub fn expected_bucket_owner(mut self, value: impl Into<String>) -> Self {
        self.expected_bucket_owner = Some(value.into());
        self
    }

    /// Validate without opening storage.
    ///
    /// # Errors
    /// Rejects malformed bucket/key or upload identities and owner assertions.
    pub fn build(self) -> Result<AbortMultipartUploadInput, StorageError> {
        Ok(AbortMultipartUploadInput {
            upload: super::Upload::new(
                &self.bucket,
                &self.key,
                self.upload_id,
                self.expected_bucket_owner,
            )?,
        })
    }
}

/// Validated request for `ListParts`.
#[derive(Debug, Clone)]
pub struct ListPartsInput {
    upload: super::Upload,
    part_number_marker: u32,
    max_parts: PartPageSize,
}

/// Builder for `ListPartsInput`.
#[derive(Debug, Clone)]
pub struct ListPartsInputBuilder {
    bucket: String,
    key: String,
    upload_id: String,
    expected_bucket_owner: Option<String>,
    part_number_marker: u32,
    max_parts: u32,
}

impl ListPartsInput {
    /// Identify one upload; an ID supplies no authority.
    #[must_use]
    pub fn builder(
        bucket: impl Into<String>,
        key: impl Into<String>,
        upload_id: impl Into<String>,
    ) -> ListPartsInputBuilder {
        ListPartsInputBuilder {
            bucket: bucket.into(),
            key: key.into(),
            upload_id: upload_id.into(),
            expected_bucket_owner: None,
            part_number_marker: 0,
            max_parts: 1000,
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

    /// Exclusive original-part-number marker; defaults to zero.
    #[must_use]
    pub const fn part_number_marker(&self) -> u32 {
        self.part_number_marker
    }

    /// Page bound from zero through 1,000; defaults to 1,000.
    #[must_use]
    pub const fn max_parts(&self) -> u32 {
        self.max_parts.get()
    }
}

impl ListPartsInputBuilder {
    /// Assert destination ownership.
    #[must_use]
    pub fn expected_bucket_owner(mut self, value: impl Into<String>) -> Self {
        self.expected_bucket_owner = Some(value.into());
        self
    }

    /// List original part numbers strictly above this marker.
    #[must_use]
    pub const fn part_number_marker(mut self, value: u32) -> Self {
        self.part_number_marker = value;
        self
    }

    /// Set a page bound from zero through 1,000; checked by `build`.
    #[must_use]
    pub const fn max_parts(mut self, value: u32) -> Self {
        self.max_parts = value;
        self
    }

    /// Validate without opening storage.
    ///
    /// # Errors
    /// Rejects invalid identity, owner or page bounds.
    pub fn build(self) -> Result<ListPartsInput, StorageError> {
        Ok(ListPartsInput {
            upload: super::Upload::new(
                &self.bucket,
                &self.key,
                self.upload_id,
                self.expected_bucket_owner,
            )?,
            part_number_marker: self.part_number_marker,
            max_parts: PartPageSize::new(self.max_parts).map_err(StorageError::from_object)?,
        })
    }
}

/// Successful abort acknowledgment.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub struct AbortMultipartUploadOutput;

/// A committed part in a single consistent listing snapshot.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PartSummary {
    part_number: u32,
    size: u64,
    etag: String,
    last_modified: Timestamp,
    checksum_algorithm: ChecksumAlgorithm,
    checksum: String,
}

impl PartSummary {
    /// Original part position.
    #[must_use]
    pub const fn part_number(&self) -> u32 {
        self.part_number
    }

    /// Full committed part length.
    #[must_use]
    pub const fn size(&self) -> u64 {
        self.size
    }

    /// Opaque quoted entity tag.
    #[must_use]
    pub fn etag(&self) -> &str {
        &self.etag
    }

    /// Confirmed part modification instant.
    #[must_use]
    pub const fn last_modified(&self) -> Timestamp {
        self.last_modified
    }

    /// Verified checksum when this algorithm was negotiated.
    #[must_use]
    pub fn checksum_crc32(&self) -> Option<&str> {
        (self.checksum_algorithm == ChecksumAlgorithm::Crc32).then_some(self.checksum.as_str())
    }

    /// Verified checksum when this algorithm was negotiated.
    #[must_use]
    pub fn checksum_crc32c(&self) -> Option<&str> {
        (self.checksum_algorithm == ChecksumAlgorithm::Crc32c).then_some(self.checksum.as_str())
    }

    /// Verified checksum when this algorithm was negotiated.
    #[must_use]
    pub fn checksum_crc64_nvme(&self) -> Option<&str> {
        (self.checksum_algorithm == ChecksumAlgorithm::Crc64Nvme).then_some(self.checksum.as_str())
    }

    /// Verified checksum when this algorithm was negotiated.
    #[must_use]
    pub fn checksum_sha1(&self) -> Option<&str> {
        (self.checksum_algorithm == ChecksumAlgorithm::Sha1).then_some(self.checksum.as_str())
    }

    /// Verified checksum when this algorithm was negotiated.
    #[must_use]
    pub fn checksum_sha256(&self) -> Option<&str> {
        (self.checksum_algorithm == ChecksumAlgorithm::Sha256).then_some(self.checksum.as_str())
    }
}

/// One bounded part page; subsequent pages observe live replacements.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ListPartsOutput {
    parts: Vec<PartSummary>,
    next_part_number_marker: Option<u32>,
    checksum_algorithm: ChecksumAlgorithm,
    checksum_type: ChecksumType,
}

impl ListPartsOutput {
    /// Ordered committed parts.
    #[must_use]
    pub fn parts(&self) -> &[PartSummary] {
        &self.parts
    }

    /// Whether another page exists in this snapshot.
    #[must_use]
    pub const fn is_truncated(&self) -> bool {
        self.next_part_number_marker.is_some()
    }

    /// Exclusive numeric boundary for the next request.
    #[must_use]
    pub const fn next_part_number_marker(&self) -> Option<u32> {
        self.next_part_number_marker
    }

    /// Immutable upload algorithm.
    #[must_use]
    pub const fn checksum_algorithm(&self) -> ChecksumAlgorithm {
        self.checksum_algorithm
    }

    /// Immutable upload checksum type.
    #[must_use]
    pub const fn checksum_type(&self) -> ChecksumType {
        self.checksum_type
    }
}

/// Literal upload namespace filters and paired continuation markers.
#[derive(Debug, Clone)]
pub struct ListMultipartUploadsInput {
    bucket: BucketName,
    expected_bucket_owner: Option<String>,
    prefix: Option<String>,
    delimiter: Option<String>,
    key_marker: Option<String>,
    upload_id_marker: Option<String>,
    max_uploads: UploadPageSize,
}

/// Builder for a bounded upload namespace page.
#[derive(Debug, Clone)]
pub struct ListMultipartUploadsInputBuilder {
    bucket: String,
    expected_bucket_owner: Option<String>,
    prefix: Option<String>,
    delimiter: Option<String>,
    key_marker: Option<String>,
    upload_id_marker: Option<String>,
    max_uploads: u32,
}

impl ListMultipartUploadsInput {
    /// Start a page with the default limit of 1,000.
    #[must_use]
    pub fn builder(bucket: impl Into<String>) -> ListMultipartUploadsInputBuilder {
        ListMultipartUploadsInputBuilder {
            bucket: bucket.into(),
            expected_bucket_owner: None,
            prefix: None,
            delimiter: None,
            key_marker: None,
            upload_id_marker: None,
            max_uploads: 1000,
        }
    }

    /// Exact bucket name.
    #[must_use]
    pub fn bucket(&self) -> &str {
        self.bucket.as_str()
    }

    /// Requested page bound.
    #[must_use]
    pub const fn max_uploads(&self) -> u32 {
        self.max_uploads.get()
    }

    /// Supplied literal request value, preserving absence.
    #[must_use]
    pub fn expected_bucket_owner(&self) -> Option<&str> {
        self.expected_bucket_owner.as_deref()
    }

    /// Supplied literal request value, preserving absence.
    #[must_use]
    pub fn prefix(&self) -> Option<&str> {
        self.prefix.as_deref()
    }

    /// Supplied literal request value, preserving absence.
    #[must_use]
    pub fn delimiter(&self) -> Option<&str> {
        self.delimiter.as_deref()
    }

    /// Supplied literal request value, preserving absence.
    #[must_use]
    pub fn key_marker(&self) -> Option<&str> {
        self.key_marker.as_deref()
    }

    /// Supplied literal request value, preserving absence.
    #[must_use]
    pub fn upload_id_marker(&self) -> Option<&str> {
        self.upload_id_marker.as_deref()
    }
}

impl ListMultipartUploadsInputBuilder {
    /// Set a literal value without normalization.
    #[must_use]
    pub fn expected_bucket_owner(mut self, value: impl Into<String>) -> Self {
        self.expected_bucket_owner = Some(value.into());
        self
    }

    /// Set a literal value without normalization.
    #[must_use]
    pub fn prefix(mut self, value: impl Into<String>) -> Self {
        self.prefix = Some(value.into());
        self
    }

    /// Set a literal value without normalization.
    #[must_use]
    pub fn delimiter(mut self, value: impl Into<String>) -> Self {
        self.delimiter = Some(value.into());
        self
    }

    /// Set a literal value without normalization.
    #[must_use]
    pub fn key_marker(mut self, value: impl Into<String>) -> Self {
        self.key_marker = Some(value.into());
        self
    }

    /// Set a literal value without normalization.
    #[must_use]
    pub fn upload_id_marker(mut self, value: impl Into<String>) -> Self {
        self.upload_id_marker = Some(value.into());
        self
    }

    /// Set a limit from 1 through 1,000.
    #[must_use]
    pub const fn max_uploads(mut self, value: u32) -> Self {
        self.max_uploads = value;
        self
    }

    /// Validate owned values; marker scope is checked during execution.
    ///
    /// # Errors
    /// Rejects invalid buckets, owners and page bounds.
    pub fn build(self) -> Result<ListMultipartUploadsInput, StorageError> {
        super::validate_owner(self.expected_bucket_owner.as_deref())?;

        Ok(ListMultipartUploadsInput {
            bucket: BucketName::parse(&self.bucket).map_err(StorageError::from_object)?,
            expected_bucket_owner: self.expected_bucket_owner,
            prefix: self.prefix,
            delimiter: self.delimiter,
            key_marker: self.key_marker,
            upload_id_marker: self.upload_id_marker,
            max_uploads: UploadPageSize::new(self.max_uploads)
                .map_err(StorageError::from_object)?,
        })
    }
}

/// An active upload, identified independently of object publication.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MultipartUploadSummary {
    key: String,
    upload_id: String,
    initiated: Timestamp,
    checksum_algorithm: ChecksumAlgorithm,
    checksum_type: ChecksumType,
}

impl MultipartUploadSummary {
    /// Exact destination key.
    #[must_use]
    pub fn key(&self) -> &str {
        &self.key
    }

    /// Opaque identity.
    #[must_use]
    pub fn upload_id(&self) -> &str {
        &self.upload_id
    }

    /// Durable original initiation time.
    #[must_use]
    pub const fn initiated(&self) -> Timestamp {
        self.initiated
    }

    /// Immutable algorithm.
    #[must_use]
    pub const fn checksum_algorithm(&self) -> ChecksumAlgorithm {
        self.checksum_algorithm
    }

    /// Immutable checksum type.
    #[must_use]
    pub const fn checksum_type(&self) -> ChecksumType {
        self.checksum_type
    }
}

/// One consistent bounded upload and common-prefix page.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ListMultipartUploadsOutput {
    uploads: Vec<MultipartUploadSummary>,
    common_prefixes: Vec<crate::CommonPrefix>,
    next_key_marker: Option<String>,
    next_upload_id_marker: Option<String>,
}

impl ListMultipartUploadsOutput {
    /// Ordered active uploads.
    #[must_use]
    pub fn uploads(&self) -> &[MultipartUploadSummary] {
        &self.uploads
    }

    /// Ordered literal common prefixes.
    #[must_use]
    pub fn common_prefixes(&self) -> &[crate::CommonPrefix] {
        &self.common_prefixes
    }

    /// Whether another page exists.
    #[must_use]
    pub const fn is_truncated(&self) -> bool {
        self.next_key_marker.is_some()
    }

    /// Next lexical key boundary, paired with the optional upload boundary.
    #[must_use]
    pub fn next_key_marker(&self) -> Option<&str> {
        self.next_key_marker.as_deref()
    }

    /// Next lexical upload boundary; absent for a common-prefix boundary.
    #[must_use]
    pub fn next_upload_id_marker(&self) -> Option<&str> {
        self.next_upload_id_marker.as_deref()
    }
}

impl crate::Store {
    /// Fence an upload against further publication and retire its parts.
    ///
    /// # Errors
    /// Reports missing uploads, authority, state and storage failures with mutation outcome.
    pub async fn abort_multipart_upload(
        &self,
        input: AbortMultipartUploadInput,
    ) -> Result<AbortMultipartUploadOutput, StorageError> {
        self.connection
            .abort_multipart_upload(input.upload.into_core())
            .await
            .map_err(StorageError::from_operation)?;

        Ok(AbortMultipartUploadOutput)
    }

    /// Read one committed-part snapshot without opening payload files.
    ///
    /// # Errors
    /// Reports missing uploads, authority, lifecycle and storage failures.
    pub async fn list_parts(&self, input: ListPartsInput) -> Result<ListPartsOutput, StorageError> {
        let page = self
            .connection
            .list_parts(mikrio_core::multipart::ListPartsRequest {
                upload: input.upload.into_core(),
                part_number_marker: input.part_number_marker,
                max_parts: input.max_parts,
            })
            .await
            .map_err(StorageError::from_operation)?;

        Ok(ListPartsOutput {
            parts: page
                .parts
                .into_iter()
                .map(|part| PartSummary {
                    part_number: part.part_number.get(),
                    size: part.size.get(),
                    etag: part.etag,
                    last_modified: Timestamp::from_core(part.last_modified),
                    checksum_algorithm: ChecksumAlgorithm::from_core(part.checksum.algorithm()),
                    checksum: part.checksum.encode(),
                })
                .collect(),
            next_part_number_marker: page.next_part_number_marker,
            checksum_algorithm: ChecksumAlgorithm::from_core(page.checksum.algorithm()),
            checksum_type: ChecksumType::from_core(page.checksum.checksum_type()),
        })
    }

    /// Read a live upload namespace page using paired lexical markers.
    ///
    /// # Errors
    /// Reports invalid filters, missing buckets, authority and storage failures.
    pub async fn list_multipart_uploads(
        &self,
        input: ListMultipartUploadsInput,
    ) -> Result<ListMultipartUploadsOutput, StorageError> {
        let page = self
            .connection
            .list_multipart_uploads(mikrio_core::multipart::ListUploadsRequest {
                bucket: input.bucket,
                expected_owner: input.expected_bucket_owner,
                prefix: input.prefix,
                delimiter: input.delimiter,
                key_marker: input.key_marker,
                upload_id_marker: input.upload_id_marker,
                max_uploads: input.max_uploads,
            })
            .await
            .map_err(StorageError::from_operation)?;

        let (next_key_marker, next_upload_id_marker) = page
            .next_marker
            .map_or((None, None), |marker| (Some(marker.key), marker.upload_id));

        Ok(ListMultipartUploadsOutput {
            uploads: page
                .uploads
                .into_iter()
                .map(|upload| MultipartUploadSummary {
                    key: upload.key,
                    initiated: Timestamp::from_core(upload.upload_id.initiated_at()),
                    upload_id: upload.upload_id.to_string(),
                    checksum_algorithm: ChecksumAlgorithm::from_core(upload.checksum.algorithm()),
                    checksum_type: ChecksumType::from_core(upload.checksum.checksum_type()),
                })
                .collect(),
            common_prefixes: page
                .common_prefixes
                .into_iter()
                .map(crate::CommonPrefix::new)
                .collect(),
            next_key_marker,
            next_upload_id_marker,
        })
    }
}
