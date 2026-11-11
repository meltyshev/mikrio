//! Owned operation facts crossing the metadata and payload boundaries.

use std::collections::BTreeMap;
use std::sync::Arc;

use super::{
    BucketName, ByteLength, ByteRange, ObjectKey, ObjectTags, ReadConditions, Representation,
    SelectedRange, UserMetadata, WriteConditions,
};
use crate::integrity::{ChecksumType, PutIntegrity};
use crate::{BlobId, EpochMillis};

/// Constructed only by the separate native and authenticated connection entries.
#[derive(Clone)]
pub(crate) enum Authority {
    Native,
    Http(Arc<crate::access::HttpIdentity>),
}

pub struct PutRequest {
    pub bucket: BucketName,
    pub key: ObjectKey,
    pub expected_owner: Option<String>,
    pub content_length: Option<ByteLength>,
    pub representation: Representation,
    pub metadata: UserMetadata,
    pub tags: Option<ObjectTags>,
    pub integrity: PutIntegrity,
    pub conditions: WriteConditions,
}

#[derive(Clone)]
pub struct ReadRequest {
    pub bucket: BucketName,
    pub key: ObjectKey,
    pub expected_owner: Option<String>,
    pub conditions: ReadConditions,
    pub range: Option<ByteRange>,
    pub part_number: Option<crate::multipart::PartNumber>,
    pub response: Representation,
}

#[derive(Debug, Clone)]
pub struct StoredObject {
    pub bucket_id: [u8; 16],
    pub generation: [u8; 16],
    pub blob_id: BlobId,
    pub length: ByteLength,
    pub range: Option<SelectedRange>,
    pub(crate) part: Option<SelectedPart>,
    pub etag: String,
    pub(crate) md5: [u8; 16],
    pub(crate) part_count: Option<u32>,
    pub representation: Representation,
    pub metadata: UserMetadata,
    pub tags: ObjectTags,
    /// Disclosure decided with the selected object/access snapshot.
    pub tag_count: Option<u32>,
    pub last_modified: EpochMillis,
    pub checksum_type: Option<ChecksumType>,
    pub checksums: BTreeMap<String, String>,
}

/// A validated part extent can be empty, unlike an HTTP byte range.
#[derive(Debug, Clone, Copy)]
pub(crate) struct SelectedPart {
    pub offset: u64,
    pub length: u64,
}

impl StoredObject {
    #[must_use]
    pub fn is_part_read(&self) -> bool {
        self.part.is_some()
    }

    #[must_use]
    pub fn parts_count(&self) -> Option<u32> {
        self.part.and(self.part_count)
    }

    #[must_use]
    pub fn content_offset(&self) -> u64 {
        self.part.map_or_else(
            || self.range.map_or(0, SelectedRange::offset),
            |part| part.offset,
        )
    }

    /// Number of bytes returned by this selection; `length` remains the full size.
    #[must_use]
    pub fn content_length(&self) -> u64 {
        self.part.map_or_else(
            || {
                self.range
                    .map_or_else(|| self.length.get(), SelectedRange::length)
            },
            |part| part.length,
        )
    }

    #[must_use]
    pub fn content_range(&self) -> Option<String> {
        if let Some(part) = self.part {
            return (part.length != 0).then(|| {
                format!(
                    "bytes {}-{}/{}",
                    part.offset,
                    part.offset + part.length - 1,
                    self.length.get()
                )
            });
        }

        self.range.map(|range| range.content_range(self.length))
    }
}
