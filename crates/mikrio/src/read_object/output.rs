use std::collections::BTreeMap;

use crate::{ObjectBody, Timestamp};

/// Whether a returned checksum describes payload bytes or selected part digests.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub enum ChecksumType {
    /// Checksum of the complete object's bytes.
    FullObject,
    /// Checksum derived from ordered selected-part digests.
    Composite,
}

impl ChecksumType {
    /// Exact S3 spelling.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::FullObject => "FULL_OBJECT",
            Self::Composite => "COMPOSITE",
        }
    }

    pub(crate) const fn into_core(self) -> mikrio_core::integrity::ChecksumType {
        match self {
            Self::FullObject => mikrio_core::integrity::ChecksumType::FullObject,
            Self::Composite => mikrio_core::integrity::ChecksumType::Composite,
        }
    }

    pub(crate) const fn from_core(value: mikrio_core::integrity::ChecksumType) -> Self {
        match value {
            mikrio_core::integrity::ChecksumType::FullObject => Self::FullObject,
            mikrio_core::integrity::ChecksumType::Composite => Self::Composite,
        }
    }
}

/// Internal detached facts, constructed only after the owning backend validates
/// required response fields. This is not a supported application API.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ReadMetadata {
    /// Selected byte count.
    pub content_length: u64,
    /// Required opaque quoted entity tag.
    pub etag: String,
    /// Required modification instant.
    pub last_modified: Timestamp,
    /// Canonical lowercase user metadata.
    pub metadata: BTreeMap<String, String>,
    /// Required tag count, never inferred from a missing permission-dependent fact.
    pub tag_count: u32,
    /// Optional media type.
    pub content_type: Option<String>,
    /// Optional representation encoding.
    pub content_encoding: Option<String>,
    /// Optional content disposition.
    pub content_disposition: Option<String>,
    /// Optional content language.
    pub content_language: Option<String>,
    /// Optional cache policy.
    pub cache_control: Option<String>,
    /// Optional cache expiry instant.
    pub expires: Option<Timestamp>,
    /// Optional selected range.
    pub content_range: Option<String>,
    /// Optional completed-object part count.
    pub parts_count: Option<u32>,
    /// Optional S3 CRC32 checksum.
    pub checksum_crc32: Option<String>,
    /// Optional S3 CRC32C checksum.
    pub checksum_crc32c: Option<String>,
    /// Optional S3 SHA1 checksum.
    pub checksum_sha1: Option<String>,
    /// Optional S3 SHA256 checksum.
    pub checksum_sha256: Option<String>,
    /// Optional S3 CRC64NVME checksum.
    pub checksum_crc64_nvme: Option<String>,
    /// Meaning of returned checksum facts.
    pub checksum_type: Option<ChecksumType>,
}

impl ReadMetadata {
    pub(super) fn from_core(
        mut object: mikrio_core::object::StoredObject,
        include_checksums: bool,
    ) -> Self {
        let content_length = object.content_length();
        let content_range = object.content_range();
        let parts_count = object.parts_count();
        let fields = object.representation.fields();

        if !include_checksums {
            object.checksums.clear();
            object.checksum_type = None;
        }

        Self {
            content_length,
            etag: object.etag,
            last_modified: Timestamp::from_core(object.last_modified),
            metadata: object.metadata.into_entries(),
            tag_count: object.tags.count(),
            content_type: Some(
                fields
                    .content_type
                    .clone()
                    .unwrap_or_else(|| "binary/octet-stream".into()),
            ),
            content_encoding: fields.content_encoding.clone(),
            content_disposition: fields.content_disposition.clone(),
            content_language: fields.content_language.clone(),
            cache_control: fields.cache_control.clone(),
            expires: fields.expires.map(Timestamp::from_core),
            content_range,
            parts_count,
            checksum_crc32: object.checksums.remove("CRC32"),
            checksum_crc32c: object.checksums.remove("CRC32C"),
            checksum_crc64_nvme: object.checksums.remove("CRC64NVME"),
            checksum_sha1: object.checksums.remove("SHA1"),
            checksum_sha256: object.checksums.remove("SHA256"),
            checksum_type: object.checksum_type.map(ChecksumType::from_core),
        }
    }
}

/// An owned response body and detached metadata for the same selected object.
/// Metadata inspection does not consume the body; explicitly close it when done.
#[derive(Debug)]
pub struct GetObjectOutput {
    pub(crate) body: ObjectBody,
    pub(crate) details: ReadMetadata,
}

impl GetObjectOutput {
    /// Borrow the single response cursor for bounded reads or explicit close.
    #[must_use]
    pub const fn body_mut(&mut self) -> &mut ObjectBody {
        &mut self.body
    }

    /// Transfer the response cursor, dropping the detached metadata.
    #[must_use]
    pub fn into_body(self) -> ObjectBody {
        self.body
    }

    /// Selected response byte count, including a range or part selection.
    #[must_use]
    pub const fn content_length(&self) -> u64 {
        self.details.content_length
    }

    /// Opaque quoted entity tag, independent of object generation identity.
    #[must_use]
    pub fn etag(&self) -> &str {
        &self.details.etag
    }

    /// The object's UTC modification instant at millisecond precision.
    #[must_use]
    pub const fn last_modified(&self) -> Timestamp {
        self.details.last_modified
    }

    /// User metadata with canonical lowercase keys; present even when empty.
    #[must_use]
    pub const fn metadata(&self) -> &BTreeMap<String, String> {
        &self.details.metadata
    }

    /// Number of object tags, from zero through ten; not the full tag set.
    #[must_use]
    pub const fn tag_count(&self) -> u32 {
        self.details.tag_count
    }

    /// Optional media type, retaining absence versus an explicit empty string.
    #[must_use]
    pub fn content_type(&self) -> Option<&str> {
        self.details.content_type.as_deref()
    }

    /// Optional user representation encoding, without transfer framing.
    #[must_use]
    pub fn content_encoding(&self) -> Option<&str> {
        self.details.content_encoding.as_deref()
    }

    /// Optional content disposition.
    #[must_use]
    pub fn content_disposition(&self) -> Option<&str> {
        self.details.content_disposition.as_deref()
    }

    /// Optional representation language.
    #[must_use]
    pub fn content_language(&self) -> Option<&str> {
        self.details.content_language.as_deref()
    }

    /// Optional cache policy.
    #[must_use]
    pub fn cache_control(&self) -> Option<&str> {
        self.details.cache_control.as_deref()
    }

    /// Optional cache expiry instant, not an object deletion deadline.
    #[must_use]
    pub const fn expires(&self) -> Option<Timestamp> {
        self.details.expires
    }

    /// The served byte range in S3 content-range form, when present.
    #[must_use]
    pub fn content_range(&self) -> Option<&str> {
        self.details.content_range.as_deref()
    }

    /// Completed-object part count when applicable to this response.
    #[must_use]
    pub const fn parts_count(&self) -> Option<u32> {
        self.details.parts_count
    }

    /// Optional S3-encoded CRC32 value, retaining any composite suffix.
    #[must_use]
    pub fn checksum_crc32(&self) -> Option<&str> {
        self.details.checksum_crc32.as_deref()
    }

    /// Optional S3-encoded CRC32C value, retaining any composite suffix.
    #[must_use]
    pub fn checksum_crc32c(&self) -> Option<&str> {
        self.details.checksum_crc32c.as_deref()
    }

    /// Optional S3-encoded SHA1 value, retaining any composite suffix.
    #[must_use]
    pub fn checksum_sha1(&self) -> Option<&str> {
        self.details.checksum_sha1.as_deref()
    }

    /// Optional S3-encoded SHA256 value, retaining any composite suffix.
    #[must_use]
    pub fn checksum_sha256(&self) -> Option<&str> {
        self.details.checksum_sha256.as_deref()
    }

    /// Optional S3-encoded CRC64NVME full-object value.
    #[must_use]
    pub fn checksum_crc64_nvme(&self) -> Option<&str> {
        self.details.checksum_crc64_nvme.as_deref()
    }

    /// Whether returned checksum facts describe the full object or its part digests.
    #[must_use]
    pub const fn checksum_type(&self) -> Option<ChecksumType> {
        self.details.checksum_type
    }
}

/// Detached object response metadata, with no response body or cursor.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HeadObjectOutput {
    pub(crate) details: ReadMetadata,
}

impl HeadObjectOutput {
    /// Byte count GET would return for the same selection.
    #[must_use]
    pub const fn content_length(&self) -> u64 {
        self.details.content_length
    }

    /// Opaque quoted entity tag, independent of object generation identity.
    #[must_use]
    pub fn etag(&self) -> &str {
        &self.details.etag
    }

    /// The object's UTC modification instant at millisecond precision.
    #[must_use]
    pub const fn last_modified(&self) -> Timestamp {
        self.details.last_modified
    }

    /// User metadata with canonical lowercase keys; present even when empty.
    #[must_use]
    pub const fn metadata(&self) -> &BTreeMap<String, String> {
        &self.details.metadata
    }

    /// Number of object tags, from zero through ten; not the full tag set.
    #[must_use]
    pub const fn tag_count(&self) -> u32 {
        self.details.tag_count
    }

    /// Optional media type, retaining absence versus an explicit empty string.
    #[must_use]
    pub fn content_type(&self) -> Option<&str> {
        self.details.content_type.as_deref()
    }

    /// Optional user representation encoding, without transfer framing.
    #[must_use]
    pub fn content_encoding(&self) -> Option<&str> {
        self.details.content_encoding.as_deref()
    }

    /// Optional content disposition.
    #[must_use]
    pub fn content_disposition(&self) -> Option<&str> {
        self.details.content_disposition.as_deref()
    }

    /// Optional representation language.
    #[must_use]
    pub fn content_language(&self) -> Option<&str> {
        self.details.content_language.as_deref()
    }

    /// Optional cache policy.
    #[must_use]
    pub fn cache_control(&self) -> Option<&str> {
        self.details.cache_control.as_deref()
    }

    /// Optional cache expiry instant, not an object deletion deadline.
    #[must_use]
    pub const fn expires(&self) -> Option<Timestamp> {
        self.details.expires
    }

    /// The selected byte range in S3 content-range form, when present.
    #[must_use]
    pub fn content_range(&self) -> Option<&str> {
        self.details.content_range.as_deref()
    }

    /// Completed-object part count when applicable to this response.
    #[must_use]
    pub const fn parts_count(&self) -> Option<u32> {
        self.details.parts_count
    }

    /// Optional S3-encoded CRC32 value, retaining any composite suffix.
    #[must_use]
    pub fn checksum_crc32(&self) -> Option<&str> {
        self.details.checksum_crc32.as_deref()
    }

    /// Optional S3-encoded CRC32C value, retaining any composite suffix.
    #[must_use]
    pub fn checksum_crc32c(&self) -> Option<&str> {
        self.details.checksum_crc32c.as_deref()
    }

    /// Optional S3-encoded SHA1 value, retaining any composite suffix.
    #[must_use]
    pub fn checksum_sha1(&self) -> Option<&str> {
        self.details.checksum_sha1.as_deref()
    }

    /// Optional S3-encoded SHA256 value, retaining any composite suffix.
    #[must_use]
    pub fn checksum_sha256(&self) -> Option<&str> {
        self.details.checksum_sha256.as_deref()
    }

    /// Optional S3-encoded CRC64NVME full-object value.
    #[must_use]
    pub fn checksum_crc64_nvme(&self) -> Option<&str> {
        self.details.checksum_crc64_nvme.as_deref()
    }

    /// Whether returned checksum facts describe the full object or its part digests.
    #[must_use]
    pub const fn checksum_type(&self) -> Option<ChecksumType> {
        self.details.checksum_type
    }
}
