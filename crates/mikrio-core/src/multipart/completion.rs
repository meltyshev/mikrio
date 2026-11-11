//! Bounded completion requests and exact immutable input identities.

use md5::Digest as _;

use super::{MultipartChecksum, PartNumber, UploadRequest};
use crate::integrity::{Checksum, MultipartLayout, ObjectChecksum};
use crate::object::{ByteLength, WriteConditions};
use crate::{BlobId, OperationError, OperationFailure};

/// One original part number and the facts acknowledged by its producer.
#[derive(Clone, Debug)]
pub struct SelectedPart {
    pub part_number: PartNumber,
    pub etag: String,
    pub checksum: Option<Checksum>,
}

/// A nonempty, ascending selection of at most 10,000 original part numbers.
#[derive(Clone, Debug)]
pub struct CompletionParts(Box<[SelectedPart]>);

impl CompletionParts {
    /// Validate bounded request shape; stored facts and checksum-mode rules are
    /// checked in the authorized selection snapshot.
    ///
    /// # Errors
    /// Rejects empty/overlong selections, malformed `ETags` and repeated/reordered
    /// numbers. Consumes at most 10,001 iterator items.
    pub fn new(parts: impl IntoIterator<Item = SelectedPart>) -> Result<Self, OperationError> {
        let mut selected = Vec::new();
        let mut previous = 0;

        for part in parts {
            if selected.len() == 10_000 {
                return Err(OperationError::new(OperationFailure::InvalidArgument));
            }

            if part.part_number.get() <= previous {
                return Err(OperationError::new(OperationFailure::InvalidPartOrder));
            }

            let digest = part
                .etag
                .strip_prefix('"')
                .and_then(|value| value.strip_suffix('"'));

            if digest
                .and_then(|value| crate::values::decode_hex::<16>(value.as_bytes()).ok())
                .is_none()
            {
                return Err(OperationError::new(OperationFailure::InvalidPart));
            }

            previous = part.part_number.get();
            selected.push(part);
        }

        if selected.is_empty() {
            return Err(OperationError::new(OperationFailure::InvalidArgument));
        }

        Ok(Self(selected.into_boxed_slice()))
    }

    #[must_use]
    pub fn parts(&self) -> &[SelectedPart] {
        &self.0
    }
}

/// Completion assertions never replace initiation metadata or renegotiate integrity.
#[derive(Clone)]
pub struct CompleteRequest {
    pub upload: UploadRequest,
    pub parts: CompletionParts,
    pub conditions: WriteConditions,
    pub content_length: Option<ByteLength>,
    pub checksum: Option<ObjectChecksum>,
    pub checksum_type: Option<crate::integrity::ChecksumType>,
}

/// Detached confirmed publication facts; an upload ID is not an idempotency token.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CompletedUpload {
    pub length: ByteLength,
    pub etag: String,
    pub checksum: ObjectChecksum,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct InputPart {
    pub generation: [u8; 16],
    pub blob_id: BlobId,
    pub part_number: PartNumber,
    pub length: ByteLength,
    pub etag: String,
    pub md5: [u8; 16],
    pub checksum: Checksum,
}

#[derive(Debug, PartialEq, Eq)]
pub(crate) struct CompletionSelection {
    pub bucket_id: [u8; 16],
    pub checksum: MultipartChecksum,
    pub parts: Box<[InputPart]>,
    pub layout: MultipartLayout,
    pub fingerprint: [u8; 32],
}

impl CompletionSelection {
    pub(crate) fn etag(&self) -> String {
        let mut digest = md5::Md5::new();

        for part in &self.parts {
            digest.update(part.md5);
        }

        let digest: [u8; 16] = digest.finalize().into();

        format!(
            "\"{:032x}-{}\"",
            u128::from_be_bytes(digest),
            self.parts.len()
        )
    }
}

#[derive(Clone, Copy, Debug)]
pub(crate) struct CompletionClaim {
    pub claim_id: [u8; 16],
    pub blob_id: BlobId,
}
