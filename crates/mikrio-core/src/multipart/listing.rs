//! Upload namespace pages use paired lexical markers rather than object tokens.

use super::MultipartChecksum;
use crate::UploadId;
use crate::access::OwnerInfo;
use crate::object::{BucketName, ObjectValueError};

/// Native upload page bound, distinct from the zero-capable part-page bound.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct UploadPageSize(u32);

impl UploadPageSize {
    /// # Errors
    /// Rejects values outside 1-1,000 without clamping.
    pub fn new(value: u32) -> Result<Self, ObjectValueError> {
        if !(1..=1000).contains(&value) {
            return Err(ObjectValueError::PageSize);
        }

        Ok(Self(value))
    }

    #[must_use]
    pub const fn get(self) -> u32 {
        self.0
    }
}

impl Default for UploadPageSize {
    fn default() -> Self {
        Self(1000)
    }
}

/// Literal filters and markers do not need to name existing keys or uploads.
#[derive(Clone)]
pub struct ListUploadsRequest {
    pub bucket: BucketName,
    pub expected_owner: Option<String>,
    pub prefix: Option<String>,
    pub delimiter: Option<String>,
    pub key_marker: Option<String>,
    /// Ignored when `key_marker` is absent; never parsed as a stored upload ID.
    pub upload_id_marker: Option<String>,
    pub max_uploads: UploadPageSize,
}

/// One validated active upload. Original initiation time is encoded in its ID.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ListedUpload {
    pub key: String,
    pub upload_id: UploadId,
    pub checksum: MultipartChecksum,
    pub initiator: String,
}

/// A common-prefix boundary has a key marker and no upload-ID marker.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct UploadMarker {
    pub key: String,
    pub upload_id: Option<String>,
}

/// One authorized snapshot, with a continuation only when another result exists.
#[derive(Debug)]
pub struct UploadPage {
    pub uploads: Vec<ListedUpload>,
    pub common_prefixes: Vec<String>,
    pub next_marker: Option<UploadMarker>,
    pub owner: OwnerInfo,
}
