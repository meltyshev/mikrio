//! Detached completed-object metadata, selected by the shared core worker.

use std::collections::BTreeMap;

use mikrio_core::object::{Attribute, AttributeSelection, AttributesRequest};

use crate::{ChecksumType, ObjectRef, StorageError, Timestamp};

/// Attribute groups; unselected or unavailable groups remain absent.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub enum ObjectAttribute {
    /// The quoted whole-object entity tag.
    Etag,
    /// Applicable stored checksum facts.
    Checksum,
    /// Completed-object part count and applicable part entries.
    ObjectParts,
    /// The object's storage class.
    StorageClass,
    /// Full object byte count.
    ObjectSize,
}

impl ObjectAttribute {
    /// Exact case-sensitive S3 spelling.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Etag => "ETag",
            Self::Checksum => "Checksum",
            Self::ObjectParts => "ObjectParts",
            Self::StorageClass => "StorageClass",
            Self::ObjectSize => "ObjectSize",
        }
    }

    /// Parse a selector without aliases or normalization.
    ///
    /// # Errors
    /// Rejects unknown selectors as `InvalidArgument`.
    pub fn parse(value: &str) -> Result<Self, StorageError> {
        match value {
            "ETag" => Ok(Self::Etag),
            "Checksum" => Ok(Self::Checksum),
            "ObjectParts" => Ok(Self::ObjectParts),
            "StorageClass" => Ok(Self::StorageClass),
            "ObjectSize" => Ok(Self::ObjectSize),
            _ => Err(StorageError::from_object(
                mikrio_core::object::ObjectValueError::AttributeSelection,
            )),
        }
    }

    const fn into_core(self) -> Attribute {
        match self {
            Self::Etag => Attribute::Etag,
            Self::Checksum => Attribute::Checksum,
            Self::ObjectParts => Attribute::ObjectParts,
            Self::StorageClass => Attribute::StorageClass,
            Self::ObjectSize => Attribute::ObjectSize,
        }
    }
}

/// Owned, validated Attributes selection. Construction performs no I/O.
#[derive(Debug, Clone)]
pub struct GetObjectAttributesInput {
    request: AttributesRequest,
    attributes: Vec<ObjectAttribute>,
}

impl GetObjectAttributesInput {
    /// Start a builder with all required fields; at most five unique selectors.
    #[must_use]
    pub fn builder(
        bucket: impl Into<String>,
        key: impl Into<String>,
        object_attributes: Vec<ObjectAttribute>,
    ) -> GetObjectAttributesInputBuilder {
        GetObjectAttributesInputBuilder {
            bucket: bucket.into(),
            key: key.into(),
            expected_bucket_owner: None,
            object_attributes,
            max_parts: 1000,
            part_number_marker: 0,
        }
    }

    /// Validated bucket name.
    #[must_use]
    pub fn bucket(&self) -> &str {
        self.request.bucket.as_str()
    }

    /// Exact object key.
    #[must_use]
    pub fn key(&self) -> &str {
        self.request.key.as_str()
    }

    /// Optional account assertion; it grants no permission.
    #[must_use]
    pub fn expected_bucket_owner(&self) -> Option<&str> {
        self.request.expected_owner.as_deref()
    }

    /// Requested groups, in caller order.
    #[must_use]
    pub fn object_attributes(&self) -> &[ObjectAttribute] {
        &self.attributes
    }

    /// Positive page bound, default 1,000.
    #[must_use]
    pub fn max_parts(&self) -> u32 {
        self.request.max_parts.get()
    }

    /// Exclusive original-part-number marker, default zero.
    #[must_use]
    pub const fn part_number_marker(&self) -> u32 {
        self.request.part_number_marker
    }
}

/// Builder for a bounded, metadata-only Attributes read.
#[derive(Debug, Clone)]
pub struct GetObjectAttributesInputBuilder {
    bucket: String,
    key: String,
    expected_bucket_owner: Option<String>,
    object_attributes: Vec<ObjectAttribute>,
    max_parts: u32,
    part_number_marker: u32,
}

impl GetObjectAttributesInputBuilder {
    /// Assert the bucket's twelve-digit owner account.
    #[must_use]
    pub fn expected_bucket_owner(mut self, value: impl Into<String>) -> Self {
        self.expected_bucket_owner = Some(value.into());
        self
    }

    /// Limit an entry page to 1-1,000 parts.
    #[must_use]
    pub const fn max_parts(mut self, value: u32) -> Self {
        self.max_parts = value;
        self
    }

    /// Select original part numbers strictly above this marker (0-10,000).
    #[must_use]
    pub const fn part_number_marker(mut self, value: u32) -> Self {
        self.part_number_marker = value;
        self
    }

    /// Validate without opening a store or payload.
    ///
    /// # Errors
    /// Rejects invalid identities, repeated/empty selectors and invalid bounds.
    pub fn build(self) -> Result<GetObjectAttributesInput, StorageError> {
        let (bucket, key) = ObjectRef::new(&self.bucket, &self.key)?.into_core();

        if let Some(owner) = &self.expected_bucket_owner {
            mikrio_core::access::validate_account_id(owner).map_err(StorageError::from_identity)?;
        }

        if self.max_parts == 0 || self.part_number_marker > 10_000 {
            return Err(StorageError::from_object(
                mikrio_core::object::ObjectValueError::PageSize,
            ));
        }

        let max_parts = mikrio_core::multipart::PartPageSize::new(self.max_parts)
            .map_err(StorageError::from_object)?;

        let attributes = AttributeSelection::new(
            self.object_attributes
                .iter()
                .copied()
                .map(ObjectAttribute::into_core),
        )
        .map_err(StorageError::from_operation)?;

        Ok(GetObjectAttributesInput {
            request: AttributesRequest {
                bucket,
                key,
                expected_owner: self.expected_bucket_owner,
                attributes,
                max_parts,
                part_number_marker: self.part_number_marker,
            },
            attributes: self.object_attributes,
        })
    }
}

/// Named optional checksum values; composite Attributes values have no count suffix.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Checksum {
    crc32: Option<String>,
    crc32c: Option<String>,
    sha1: Option<String>,
    sha256: Option<String>,
    crc64_nvme: Option<String>,
    kind: Option<ChecksumType>,
}

impl Checksum {
    /// Optional CRC32 digest, encoded as padded base64.
    #[must_use]
    pub fn checksum_crc32(&self) -> Option<&str> {
        self.crc32.as_deref()
    }

    /// Optional CRC32C digest, encoded as padded base64.
    #[must_use]
    pub fn checksum_crc32c(&self) -> Option<&str> {
        self.crc32c.as_deref()
    }

    /// Optional SHA1 digest, encoded as padded base64.
    #[must_use]
    pub fn checksum_sha1(&self) -> Option<&str> {
        self.sha1.as_deref()
    }

    /// Optional SHA256 digest, encoded as padded base64.
    #[must_use]
    pub fn checksum_sha256(&self) -> Option<&str> {
        self.sha256.as_deref()
    }

    /// Optional CRC64NVME digest, encoded as padded base64.
    #[must_use]
    pub fn checksum_crc64_nvme(&self) -> Option<&str> {
        self.crc64_nvme.as_deref()
    }

    /// Meaning of the returned digest.
    #[must_use]
    pub const fn checksum_type(&self) -> Option<ChecksumType> {
        self.kind
    }

    fn from_core(
        mut values: BTreeMap<String, String>,
        checksum_type: Option<ChecksumType>,
    ) -> Self {
        Self {
            crc32: values.remove("CRC32"),
            crc32c: values.remove("CRC32C"),
            sha1: values.remove("SHA1"),
            sha256: values.remove("SHA256"),
            crc64_nvme: values.remove("CRC64NVME"),
            kind: checksum_type,
        }
    }
}

/// One completed part's immutable facts; never a live-upload `PartSummary`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ObjectPart {
    part_number: u32,
    size: u64,
    checksum: Checksum,
}

impl ObjectPart {
    /// Original multipart part number.
    #[must_use]
    pub const fn part_number(&self) -> u32 {
        self.part_number
    }

    /// Selected part byte count.
    #[must_use]
    pub const fn size(&self) -> u64 {
        self.size
    }

    /// Optional verified CRC32 part digest.
    #[must_use]
    pub fn checksum_crc32(&self) -> Option<&str> {
        self.checksum.checksum_crc32()
    }

    /// Optional verified CRC32C part digest.
    #[must_use]
    pub fn checksum_crc32c(&self) -> Option<&str> {
        self.checksum.checksum_crc32c()
    }

    /// Optional verified SHA1 part digest.
    #[must_use]
    pub fn checksum_sha1(&self) -> Option<&str> {
        self.checksum.checksum_sha1()
    }

    /// Optional verified SHA256 part digest.
    #[must_use]
    pub fn checksum_sha256(&self) -> Option<&str> {
        self.checksum.checksum_sha256()
    }

    /// Optional verified CRC64NVME part digest.
    #[must_use]
    pub fn checksum_crc64_nvme(&self) -> Option<&str> {
        self.checksum.checksum_crc64_nvme()
    }
}

/// A bounded entry page, or a normalized count-only response.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ObjectParts {
    total_parts_count: u32,
    max_parts: u32,
    part_number_marker: u32,
    parts: Vec<ObjectPart>,
    is_truncated: bool,
    next_part_number_marker: Option<u32>,
}

impl ObjectParts {
    /// Total selected multipart parts, independent of page size.
    #[must_use]
    pub const fn total_parts_count(&self) -> u32 {
        self.total_parts_count
    }

    /// Requested positive page bound.
    #[must_use]
    pub const fn max_parts(&self) -> u32 {
        self.max_parts
    }

    /// Requested exclusive marker.
    #[must_use]
    pub const fn part_number_marker(&self) -> u32 {
        self.part_number_marker
    }

    /// Whether another entry page exists.
    #[must_use]
    pub const fn is_truncated(&self) -> bool {
        self.is_truncated
    }

    /// Continuation only when truncated.
    #[must_use]
    pub const fn next_part_number_marker(&self) -> Option<u32> {
        self.next_part_number_marker
    }

    /// Available entries; empty for count-only objects and exhausted pages.
    #[must_use]
    pub fn parts(&self) -> &[ObjectPart] {
        &self.parts
    }
}

/// Detached selected groups, usable after the connection closes.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GetObjectAttributesOutput {
    object_size: Option<u64>,
    etag: Option<String>,
    checksum: Option<Checksum>,
    storage_class: Option<String>,
    object_parts: Option<ObjectParts>,
    last_modified: Timestamp,
}

impl GetObjectAttributesOutput {
    /// Full object size when selected.
    #[must_use]
    pub const fn object_size(&self) -> Option<u64> {
        self.object_size
    }

    /// Quoted whole-object `ETag` when selected.
    #[must_use]
    pub fn etag(&self) -> Option<&str> {
        self.etag.as_deref()
    }

    /// Applicable checksum group when selected.
    #[must_use]
    pub const fn checksum(&self) -> Option<&Checksum> {
        self.checksum.as_ref()
    }

    /// Storage class when selected.
    #[must_use]
    pub fn storage_class(&self) -> Option<&str> {
        self.storage_class.as_deref()
    }

    /// Completed-object part facts when selected and available.
    #[must_use]
    pub const fn object_parts(&self) -> Option<&ObjectParts> {
        self.object_parts.as_ref()
    }

    /// Whole-object modification instant from the selected snapshot.
    #[must_use]
    pub const fn last_modified(&self) -> Timestamp {
        self.last_modified
    }
}

impl crate::Store {
    /// Select Attributes without opening a payload file or acquiring a body cursor.
    ///
    /// # Errors
    /// Preserves owner, missing-target, lifecycle, contention and corruption failures.
    pub async fn get_object_attributes(
        &self,
        input: GetObjectAttributesInput,
    ) -> Result<GetObjectAttributesOutput, StorageError> {
        let max_parts = input.max_parts();
        let part_number_marker = input.part_number_marker();

        let selected = self
            .connection
            .get_object_attributes(input.request)
            .await
            .map_err(StorageError::from_operation)?;

        Ok(GetObjectAttributesOutput {
            object_size: selected
                .object_size
                .map(mikrio_core::object::ByteLength::get),
            etag: selected.etag,
            checksum: selected.checksum.map(|(mode, values)| {
                Checksum::from_core(values, Some(ChecksumType::from_core(mode)))
            }),
            storage_class: selected.storage_class.map(str::to_owned),
            object_parts: selected.object_parts.map(|page| ObjectParts {
                total_parts_count: page.total_parts_count,
                max_parts,
                part_number_marker,
                parts: page
                    .parts
                    .into_iter()
                    .map(|part| ObjectPart {
                        part_number: part.part_number,
                        size: part.byte_length,
                        checksum: Checksum::from_core(
                            part.checksums
                                .into_iter()
                                .map(|digest| {
                                    (digest.algorithm().as_str().to_owned(), digest.encode())
                                })
                                .collect(),
                            None,
                        ),
                    })
                    .collect(),
                is_truncated: page.next_part_number_marker.is_some(),
                next_part_number_marker: page.next_part_number_marker,
            }),
            last_modified: Timestamp::from_core(selected.last_modified),
        })
    }
}
