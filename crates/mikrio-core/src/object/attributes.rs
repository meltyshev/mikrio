//! Detached internal attribute facts; wire field presence is a separate projection.

use std::collections::BTreeMap;

use super::{BucketName, ByteLength, ObjectKey};
use crate::integrity::{ChecksumType, PartLayout};
use crate::multipart::PartPageSize;
use crate::{EpochMillis, OperationError, OperationFailure};

/// Attribute groups selectable without opening the payload.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Attribute {
    Etag,
    Checksum,
    ObjectParts,
    StorageClass,
    ObjectSize,
}

/// A nonempty, duplicate-free bounded attribute selection.
#[derive(Debug, Clone)]
pub struct AttributeSelection(Vec<Attribute>);

impl AttributeSelection {
    /// # Errors
    /// Rejects empty or repeated groups, consuming at most six inputs.
    pub fn new(values: impl IntoIterator<Item = Attribute>) -> Result<Self, OperationError> {
        let mut selected = Vec::with_capacity(5);

        for value in values {
            if selected.contains(&value) || selected.len() == 5 {
                return Err(OperationError::new(OperationFailure::InvalidArgument));
            }

            selected.push(value);
        }

        if selected.is_empty() {
            return Err(OperationError::new(OperationFailure::InvalidArgument));
        }

        Ok(Self(selected))
    }

    #[must_use]
    pub fn contains(&self, value: Attribute) -> bool {
        self.0.contains(&value)
    }
}

/// Internal selection uses original retained part numbers and positive native bounds.
#[derive(Debug, Clone)]
pub struct AttributesRequest {
    pub bucket: BucketName,
    pub key: ObjectKey,
    pub expected_owner: Option<String>,
    pub attributes: AttributeSelection,
    pub max_parts: PartPageSize,
    pub part_number_marker: u32,
}

/// Completed layout projection; count-only responses do not scan child rows.
#[derive(Debug)]
pub struct StoredPartPage {
    pub total_parts_count: u32,
    pub entries_available: bool,
    pub parts: Vec<PartLayout>,
    pub next_part_number_marker: Option<u32>,
}

/// One authorized snapshot, with no body, representation, user metadata or tags.
#[derive(Debug)]
pub struct StoredAttributes {
    pub object_size: Option<ByteLength>,
    pub etag: Option<String>,
    pub checksum: Option<(ChecksumType, BTreeMap<String, String>)>,
    pub storage_class: Option<&'static str>,
    pub object_parts: Option<StoredPartPage>,
    pub last_modified: EpochMillis,
}
