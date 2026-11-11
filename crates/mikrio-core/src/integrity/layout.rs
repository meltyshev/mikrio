//! Complete immutable part facts validated before final-blob publication.

use std::fmt;

use super::Checksum;

const MAX_PARTS: u32 = 10_000;
const MIN_PART_BYTES: u64 = 5 * 1024 * 1024;
const MAX_PART_BYTES: u64 = 5 * 1024 * 1024 * 1024;

/// Proposed retained facts for one selected part, independent of its old blob.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PartLayout {
    pub part_number: u32,
    pub byte_offset: u64,
    pub byte_length: u64,
    /// Verified full-part digests only; no composite suffix or `ETag` inference.
    pub checksums: Vec<Checksum>,
}

/// An ordered, bounded, contiguous layout covering exactly the assembled bytes.
/// This validates representation facts, not selected-part authority or commit.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct MultipartLayout {
    parts: Box<[PartLayout]>,
    part_count: u32,
    content_length: u64,
}

/// Invalid completed-part facts, without retaining identities or checksum bytes.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum LayoutError {
    PartCount,
    PartNumber,
    PartSize,
    ByteLayout,
    Checksums,
}

impl fmt::Display for LayoutError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(match self {
            Self::PartCount => "invalid completed part count",
            Self::PartNumber => "invalid completed part number or order",
            Self::PartSize => "invalid completed part length",
            Self::ByteLayout => "completed parts do not cover the assembled bytes",
            Self::Checksums => "duplicate or excessive completed part checksums",
        })
    }
}

impl std::error::Error for LayoutError {}

impl MultipartLayout {
    /// Validate the entire proposed layout before a publication transaction.
    /// Original numbers may have gaps; byte coverage must have none. At most
    /// 10,001 iterator items are consumed, including the rejecting lookahead.
    ///
    /// # Errors
    /// Rejects empty/overlong selections, duplicate/unordered/out-of-range part
    /// numbers, invalid part sizes, duplicate checksums and inconsistent byte
    /// boundaries or final length. Only the final part may be shorter than 5 MiB.
    pub fn new(
        parts: impl IntoIterator<Item = PartLayout>,
        content_length: u64,
    ) -> Result<Self, LayoutError> {
        let mut retained = Vec::new();
        let mut part_count = 0;
        let mut previous_number = 0;
        let mut previous_length = None;
        let mut end = 0_u64;

        for part in parts {
            if part_count == MAX_PARTS {
                return Err(LayoutError::PartCount);
            }

            if part.part_number <= previous_number || part.part_number > MAX_PARTS {
                return Err(LayoutError::PartNumber);
            }

            if part.byte_length > MAX_PART_BYTES
                || previous_length.is_some_and(|length| length < MIN_PART_BYTES)
            {
                return Err(LayoutError::PartSize);
            }

            if part.byte_offset != end {
                return Err(LayoutError::ByteLayout);
            }

            end = end
                .checked_add(part.byte_length)
                .ok_or(LayoutError::ByteLayout)?;

            if end > content_length {
                return Err(LayoutError::ByteLayout);
            }

            if part.checksums.len() > 5 {
                return Err(LayoutError::Checksums);
            }

            for (index, checksum) in part.checksums.iter().enumerate() {
                if part.checksums[..index]
                    .iter()
                    .any(|previous| previous.algorithm() == checksum.algorithm())
                {
                    return Err(LayoutError::Checksums);
                }
            }

            part_count += 1;
            previous_number = part.part_number;
            previous_length = Some(part.byte_length);
            retained.push(part);
        }

        if part_count == 0 {
            return Err(LayoutError::PartCount);
        }

        if end != content_length {
            return Err(LayoutError::ByteLayout);
        }

        Ok(Self {
            parts: retained.into_boxed_slice(),
            part_count,
            content_length,
        })
    }

    #[must_use]
    pub fn parts(&self) -> &[PartLayout] {
        &self.parts
    }

    #[must_use]
    pub const fn part_count(&self) -> u32 {
        self.part_count
    }

    #[must_use]
    pub const fn content_length(&self) -> u64 {
        self.content_length
    }
}

#[cfg(test)]
mod tests;
