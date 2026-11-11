//! Owned multipart inputs with explicit, validated checksum negotiation.

use crate::UploadId;
use crate::integrity::{ChecksumAlgorithm, ChecksumType, ExpectedDigests, IntegrityError};
use crate::object::{
    BucketName, ByteLength, ObjectKey, ObjectTags, ObjectValueError, Representation, UserMetadata,
};

mod listing;
pub use listing::{ListUploadsRequest, ListedUpload, UploadMarker, UploadPage, UploadPageSize};
mod completion;
pub use completion::{CompleteRequest, CompletedUpload, CompletionParts, SelectedPart};
pub(crate) use completion::{CompletionClaim, CompletionSelection, InputPart};
mod copy;
pub use copy::{CopiedPart, CopyPartRequest, CopyRange};

/// A supported algorithm/type pair. Omitted wire negotiation is resolved before
/// construction; it cannot silently change when a later part arrives.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct MultipartChecksum {
    algorithm: ChecksumAlgorithm,
    checksum_type: ChecksumType,
    explicit: bool,
}

impl MultipartChecksum {
    /// Resolve omitted initiation options once, before persisting the upload.
    /// CRC64NVME defaults to full-object; other selected algorithms default to
    /// composite. An explicit incompatible pair is never repaired.
    ///
    /// # Errors
    /// Rejects unsupported algorithm/type combinations.
    pub fn resolve(
        algorithm: Option<ChecksumAlgorithm>,
        checksum_type: Option<ChecksumType>,
    ) -> Result<Self, IntegrityError> {
        let explicit = algorithm.is_some() || checksum_type.is_some();
        let algorithm = algorithm.unwrap_or(ChecksumAlgorithm::Crc64Nvme);

        let checksum_type = checksum_type.unwrap_or(if algorithm == ChecksumAlgorithm::Crc64Nvme {
            ChecksumType::FullObject
        } else {
            ChecksumType::Composite
        });

        let mut selection = Self::new(algorithm, checksum_type)?;
        selection.explicit = explicit;
        Ok(selection)
    }

    /// Validate an explicit multipart checksum selection.
    ///
    /// # Errors
    /// Rejects unsupported algorithm/type combinations without a fallback.
    pub fn new(
        algorithm: ChecksumAlgorithm,
        checksum_type: ChecksumType,
    ) -> Result<Self, IntegrityError> {
        if !algorithm.supports_multipart(checksum_type) {
            return Err(IntegrityError::InvalidChecksumType);
        }

        Ok(Self {
            algorithm,
            checksum_type,
            explicit: true,
        })
    }

    pub(crate) const fn is_explicit(self) -> bool {
        self.explicit
    }

    #[must_use]
    pub const fn algorithm(self) -> ChecksumAlgorithm {
        self.algorithm
    }

    #[must_use]
    pub const fn checksum_type(self) -> ChecksumType {
        self.checksum_type
    }
}

/// Validated representation and destination facts captured at initiation.
pub struct CreateRequest {
    pub bucket: BucketName,
    pub key: ObjectKey,
    pub expected_owner: Option<String>,
    pub representation: Representation,
    pub metadata: UserMetadata,
    pub tags: Option<ObjectTags>,
    pub checksum: MultipartChecksum,
}

/// Durable upload identity and its immutable checksum selection.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CreatedUpload {
    pub upload_id: UploadId,
    pub checksum: MultipartChecksum,
}

/// Exact active upload identity; an ID never supplies authority by itself.
#[derive(Clone)]
pub struct UploadRequest {
    pub bucket: BucketName,
    pub key: ObjectKey,
    pub upload_id: UploadId,
    pub expected_owner: Option<String>,
}

/// Bounded committed-part page size, with an explicit empty-page request.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PartPageSize(u32);

impl PartPageSize {
    /// # Errors
    /// Rejects sizes above 1,000; adapters resolve their own wire defaults.
    pub fn new(value: u32) -> Result<Self, ObjectValueError> {
        if value > 1000 {
            return Err(ObjectValueError::PageSize);
        }

        Ok(Self(value))
    }

    #[must_use]
    pub const fn get(self) -> u32 {
        self.0
    }
}

impl Default for PartPageSize {
    fn default() -> Self {
        Self(1000)
    }
}

/// A numeric exclusive boundary; it need not identify a part that still exists.
#[derive(Clone)]
pub struct ListPartsRequest {
    pub upload: UploadRequest,
    pub part_number_marker: u32,
    pub max_parts: PartPageSize,
}

/// Detached facts for one currently committed part.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ListedPart {
    pub part_number: PartNumber,
    pub size: ByteLength,
    pub etag: String,
    pub last_modified: crate::EpochMillis,
    pub checksum: crate::integrity::Checksum,
}

/// One consistent metadata snapshot; later pages observe current replacements.
#[derive(Debug)]
pub struct PartPage {
    pub parts: Vec<ListedPart>,
    pub next_part_number_marker: Option<u32>,
    pub checksum: MultipartChecksum,
    pub initiator: String,
    pub owner: crate::access::OwnerInfo,
}

/// Maximum decoded bytes in one part; the minimum belongs to completion selection.
pub const PART_MAX_BYTES: u64 = 5 * 1024 * 1024 * 1024;

/// Original multipart position, independent of the order requests arrive.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub struct PartNumber(u32);

impl PartNumber {
    /// # Errors
    /// Rejects numbers outside the supported 1-10,000 interval.
    pub fn new(value: u32) -> Result<Self, ObjectValueError> {
        if !(1..=10_000).contains(&value) {
            return Err(ObjectValueError::PartNumber);
        }

        Ok(Self(value))
    }

    #[must_use]
    pub const fn get(self) -> u32 {
        self.0
    }
}

/// One independent part replacement; integrity assertions cannot renegotiate an upload.
#[derive(Clone)]
pub struct PartRequest {
    pub bucket: BucketName,
    pub key: ObjectKey,
    pub upload_id: UploadId,
    pub part_number: PartNumber,
    pub expected_owner: Option<String>,
    pub content_length: Option<ByteLength>,
    pub checksum_algorithm: Option<ChecksumAlgorithm>,
    pub expected: ExpectedDigests,
}

#[derive(Debug, Clone, Copy)]
pub(crate) struct PartReservation {
    pub bucket_id: [u8; 16],
    pub checksum: MultipartChecksum,
}

impl PartRequest {
    pub(crate) fn verifier(
        &self,
        checksum: MultipartChecksum,
    ) -> Result<crate::integrity::PayloadVerifier, IntegrityError> {
        let has_algorithm_mismatch = checksum.is_explicit()
            && self
                .checksum_algorithm
                .is_some_and(|algorithm| algorithm != checksum.algorithm());

        if has_algorithm_mismatch {
            return Err(IntegrityError::AlgorithmMismatch);
        }

        crate::integrity::PayloadVerifier::multipart(
            self.content_length.map(ByteLength::get),
            PART_MAX_BYTES,
            checksum.algorithm(),
            self.expected,
            (!checksum.is_explicit())
                .then_some(
                    self.checksum_algorithm.or(self
                        .expected
                        .checksum()
                        .map(crate::integrity::Checksum::algorithm)),
                )
                .flatten(),
        )
    }
}
