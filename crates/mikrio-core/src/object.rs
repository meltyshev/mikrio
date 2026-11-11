//! Owned object values shared by storage and protocol adapters.

use std::fmt;

use crate::EpochMillis;

mod copy;
pub use copy::{CopiedObject, CopyDirective, CopyRequest, ObjectRef};
mod attributes;
pub use attributes::{
    Attribute, AttributeSelection, AttributesRequest, StoredAttributes, StoredPartPage,
};
mod deletion;
pub use deletion::{
    DeleteObjectProgress, DeleteObjectResult, DeleteObjectsError, DeleteObjectsRequest,
    ObjectIdentifier,
};
mod metadata;
pub use metadata::UserMetadata;
mod tags;
pub use tags::ObjectTags;
mod range;
pub use range::{ByteRange, SelectedRange};
mod read;
pub use read::{ReadConditionFailure, ReadConditions, ReadConditionsInput};
mod listing;
pub use listing::{
    BucketPage, BucketPageSize, ListBucketsRequest, ListObjectsRequest, ListedBucket, ListedObject,
    ObjectPage, ObjectPageSize,
};
mod operation;
pub(crate) use operation::{Authority, SelectedPart};
pub use operation::{PutRequest, ReadRequest, StoredObject};

#[cfg(test)]
mod tests;

const KEY_MAX_BYTES: usize = 1024;
const METADATA_MAX_BYTES: usize = 2048;
const METADATA_MAX_ENTRIES: usize = 64;

/// Maximum decoded payload of one PUT, independent of multipart object size.
pub const PUT_MAX_BYTES: u64 = 5 * 1024 * 1024 * 1024;

/// Invalid object input, or corruption when encountered in persisted state.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ObjectValueError {
    BucketName,
    KeyEncoding,
    KeyLength,
    KeyRelativeSegments,
    MetadataKey,
    MetadataValue,
    MetadataDuplicate,
    MetadataCount,
    MetadataSize,
    MetadataEncoding,
    TagKey,
    TagValue,
    TagDuplicate,
    TagCount,
    TagEncoding,
    RepresentationValue,
    RepresentationSize,
    ByteLength,
    PutObjectSize,
    EntityTagCondition,
    CreateOnlyCondition,
    CopyDirective,
    ReadCondition,
    ReadRange,
    PageSize,
    AttributeSelection,
    BatchSize,
    PartNumber,
}

impl fmt::Display for ObjectValueError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(match self {
            Self::BucketName => "invalid bucket name",
            Self::KeyEncoding => "object key is not UTF-8",
            Self::KeyLength => "object key must contain 1-1024 UTF-8 bytes",
            Self::KeyRelativeSegments => "object key has unbalanced relative segments",
            Self::MetadataKey => "metadata key must be a nonempty ASCII HTTP token",
            Self::MetadataValue => "metadata value contains a control character",
            Self::MetadataDuplicate => "duplicate metadata key",
            Self::MetadataCount => "metadata exceeds its entry limit",
            Self::MetadataSize => "metadata exceeds its byte limit",
            Self::MetadataEncoding => "invalid persisted metadata object",
            Self::TagKey => "invalid or reserved object tag key",
            Self::TagValue => "invalid object tag value",
            Self::TagDuplicate => "duplicate object tag key",
            Self::TagCount => "object tags exceed ten entries",
            Self::TagEncoding => "invalid persisted tag object",
            Self::RepresentationValue => "representation header must contain printable ASCII",
            Self::RepresentationSize => "representation headers exceed their byte limit",
            Self::ByteLength => "byte length exceeds the storage integer range",
            Self::PutObjectSize => "single PUT exceeds 5 GiB",
            Self::EntityTagCondition => "condition must be one quoted entity tag or *",
            Self::CreateOnlyCondition => "write If-None-Match must be *",
            Self::CopyDirective => "invalid copy directive or conflicting replacement fields",
            Self::ReadCondition => "read condition must be an entity-tag list or *",
            Self::ReadRange => "range must be one valid HTTP byte range",
            Self::AttributeSelection => "invalid object attribute selection",
            Self::PageSize => "page size is outside this operation's range",
            Self::PartNumber => "part number must be within 1-10,000",
            Self::BatchSize => "delete batch must contain 1-1000 entries",
        })
    }
}

impl std::error::Error for ObjectValueError {}

/// Destination conditions whose evaluation belongs in the publication transaction.
#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct WriteConditions {
    if_match: Option<String>,
    if_none_match: Option<String>,
}

impl WriteConditions {
    /// # Errors
    /// Rejects malformed If-Match values and any If-None-Match other than `*`.
    pub fn new(
        if_match: Option<String>,
        if_none_match: Option<String>,
    ) -> Result<Self, ObjectValueError> {
        if let Some(value) = &if_match {
            validate_entity_tag_condition(value)?;
        }

        if if_none_match.as_deref().is_some_and(|value| value != "*") {
            return Err(ObjectValueError::CreateOnlyCondition);
        }

        Ok(Self {
            if_match,
            if_none_match,
        })
    }

    #[must_use]
    pub fn if_match(&self) -> Option<&str> {
        self.if_match.as_deref()
    }

    #[must_use]
    pub fn if_none_match(&self) -> Option<&str> {
        self.if_none_match.as_deref()
    }
}

/// Validate a single strong entity tag or an existence wildcard without rewriting it.
/// The quoted payload is opaque, including backslashes and non-ASCII UTF-8 bytes.
///
/// # Errors
/// Rejects lists, weak validators, unquoted values and invalid entity-tag bytes.
pub fn validate_entity_tag_condition(value: &str) -> Result<(), ObjectValueError> {
    if value == "*" {
        return Ok(());
    }

    let Some(opaque) = value
        .strip_prefix('"')
        .and_then(|value| value.strip_suffix('"'))
    else {
        return Err(ObjectValueError::EntityTagCondition);
    };

    if !valid_entity_tag_bytes(opaque) {
        return Err(ObjectValueError::EntityTagCondition);
    }

    Ok(())
}

fn valid_entity_tag_bytes(value: &str) -> bool {
    value
        .bytes()
        .all(|byte| byte == 0x21 || (0x23..=0x7e).contains(&byte) || byte >= 0x80)
}

/// A literal general-purpose bucket name in the shared namespace subset.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
pub struct BucketName(String);

impl BucketName {
    /// # Errors
    /// Rejects malformed, IP-shaped and reserved bucket names without normalization.
    pub fn parse(value: &str) -> Result<Self, ObjectValueError> {
        let bytes = value.as_bytes();

        if !(3..=63).contains(&bytes.len()) {
            return Err(ObjectValueError::BucketName);
        }

        if !bytes[0].is_ascii_alphanumeric() || !bytes[bytes.len() - 1].is_ascii_alphanumeric() {
            return Err(ObjectValueError::BucketName);
        }

        if !bytes
            .iter()
            .all(|byte| byte.is_ascii_lowercase() || byte.is_ascii_digit() || b".-".contains(byte))
        {
            return Err(ObjectValueError::BucketName);
        }

        if value.contains("..") {
            return Err(ObjectValueError::BucketName);
        }

        if ["xn--", "sthree-", "amzn-s3-demo-"]
            .iter()
            .any(|prefix| value.starts_with(prefix))
        {
            return Err(ObjectValueError::BucketName);
        }

        if [
            "-s3alias",
            "--ol-s3",
            ".mrap",
            "--x-s3",
            "--table-s3",
            "-an",
        ]
        .iter()
        .any(|suffix| value.ends_with(suffix))
        {
            return Err(ObjectValueError::BucketName);
        }

        if is_ipv4_name(value) {
            return Err(ObjectValueError::BucketName);
        }

        Ok(Self(value.to_owned()))
    }

    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

fn is_ipv4_name(value: &str) -> bool {
    let mut count = 0;

    for label in value.split('.') {
        if label.is_empty()
            || !label.bytes().all(|byte| byte.is_ascii_digit())
            || label.parse::<u8>().is_err()
        {
            return false;
        }

        count += 1;
    }

    count == 4
}

/// An exact UTF-8 key. Relative segments are checked without normalizing them.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
pub struct ObjectKey(String);

impl ObjectKey {
    /// # Errors
    /// Rejects empty, overlong and unbalanced keys; accepted bytes remain unchanged.
    pub fn parse(value: &str) -> Result<Self, ObjectValueError> {
        if value.is_empty() || value.len() > KEY_MAX_BYTES {
            return Err(ObjectValueError::KeyLength);
        }

        let mut preceding_segments = 0_u16;

        for segment in value.split('/') {
            match segment {
                "" | "." => {}
                ".." => {
                    preceding_segments = preceding_segments
                        .checked_sub(1)
                        .ok_or(ObjectValueError::KeyRelativeSegments)?;
                }
                _ => preceding_segments += 1,
            }
        }

        Ok(Self(value.to_owned()))
    }

    /// # Errors
    /// Rejects malformed UTF-8 and any invalid key, including on persisted reads.
    pub fn from_bytes(bytes: &[u8]) -> Result<Self, ObjectValueError> {
        let value = std::str::from_utf8(bytes).map_err(|_| ObjectValueError::KeyEncoding)?;
        Self::parse(value)
    }

    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }

    #[must_use]
    pub fn as_bytes(&self) -> &[u8] {
        self.0.as_bytes()
    }
}

/// A byte count which fits SQLite INTEGER without narrowing or saturation.
/// Operation-specific size limits are checked separately.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub struct ByteLength(i64);

impl ByteLength {
    /// # Errors
    /// Rejects counts above the signed 64-bit storage range.
    pub fn new(value: u64) -> Result<Self, ObjectValueError> {
        let stored = i64::try_from(value).map_err(|_| ObjectValueError::ByteLength)?;
        Ok(Self(stored))
    }

    /// Validate a single PUT's declared or observed decoded byte count.
    ///
    /// # Errors
    /// Rejects lengths above 5 GiB without narrowing or reading a source.
    pub fn for_put(value: u64) -> Result<Self, ObjectValueError> {
        if value > PUT_MAX_BYTES {
            return Err(ObjectValueError::PutObjectSize);
        }

        Self::new(value)
    }

    /// # Errors
    /// Rejects negative stored lengths as invalid state.
    pub fn from_stored(value: i64) -> Result<Self, ObjectValueError> {
        if value < 0 {
            return Err(ObjectValueError::ByteLength);
        }

        Ok(Self(value))
    }

    #[must_use]
    pub const fn as_stored(self) -> i64 {
        self.0
    }

    #[must_use]
    pub fn get(self) -> u64 {
        self.0.unsigned_abs()
    }
}

/// Raw representation fields, with absence distinct from an explicit empty value.
#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct RepresentationInput {
    pub content_type: Option<String>,
    pub content_encoding: Option<String>,
    pub content_disposition: Option<String>,
    pub content_language: Option<String>,
    pub cache_control: Option<String>,
    pub expires: Option<EpochMillis>,
}

/// Representation values safe to project as headers after shared validation.
#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct Representation(RepresentationInput);

impl Representation {
    /// # Errors
    /// Rejects non-printable/non-ASCII headers and the combined byte-budget overflow.
    pub fn new(input: RepresentationInput) -> Result<Self, ObjectValueError> {
        let mut remaining = METADATA_MAX_BYTES;

        for (name, value) in [
            ("content-type", &input.content_type),
            ("content-encoding", &input.content_encoding),
            ("content-disposition", &input.content_disposition),
            ("content-language", &input.content_language),
            ("cache-control", &input.cache_control),
        ] {
            let Some(value) = value else { continue };

            if !value.bytes().all(|byte| (0x20..=0x7e).contains(&byte)) {
                return Err(ObjectValueError::RepresentationValue);
            }

            remaining = remaining
                .checked_sub(name.len())
                .and_then(|remaining| remaining.checked_sub(value.len()))
                .ok_or(ObjectValueError::RepresentationSize)?;
        }

        if input.expires.is_some() && remaining < "expires".len() + 29 {
            return Err(ObjectValueError::RepresentationSize);
        }

        Ok(Self(input))
    }

    #[must_use]
    pub const fn fields(&self) -> &RepresentationInput {
        &self.0
    }

    /// Replace only supplied response fields, preserving explicit empty values.
    /// The selected response is detached and is never persisted.
    pub(crate) fn override_with(&mut self, response: &Self) {
        let fields = &response.0;

        if let Some(value) = &fields.content_type {
            self.0.content_type = Some(value.clone());
        }

        if let Some(value) = &fields.content_encoding {
            self.0.content_encoding = Some(value.clone());
        }

        if let Some(value) = &fields.content_disposition {
            self.0.content_disposition = Some(value.clone());
        }

        if let Some(value) = &fields.content_language {
            self.0.content_language = Some(value.clone());
        }

        if let Some(value) = &fields.cache_control {
            self.0.cache_control = Some(value.clone());
        }

        if let Some(value) = fields.expires {
            self.0.expires = Some(value);
        }
    }

    /// Content type for a newly written object, without inferring it from bytes or keys.
    /// An explicitly empty value remains empty; only absence selects the default.
    #[must_use]
    pub fn effective_content_type(&self) -> &str {
        self.0
            .content_type
            .as_deref()
            .unwrap_or("binary/octet-stream")
    }
}
