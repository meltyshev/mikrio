//! Guarded source slices, separate from ordinary producer-backed parts.

use super::{PART_MAX_BYTES, PartNumber, PartRequest};
use crate::integrity::{ExpectedDigests, PayloadFacts};
use crate::object::{
    BucketName, ByteLength, ObjectKey, ObjectRef, ReadConditions, ReadRequest, Representation,
};
use crate::{EpochMillis, OperationError, OperationFailure, UploadId};

/// Copy one local source or checked slice into a part of an existing upload.
#[derive(Debug, Clone)]
pub struct CopyPartRequest {
    pub bucket: BucketName,
    pub key: ObjectKey,
    pub upload_id: UploadId,
    pub part_number: PartNumber,
    pub source: ObjectRef,
    pub expected_owner: Option<String>,
    pub expected_source_owner: Option<String>,
    pub source_conditions: ReadConditions,
    pub source_range: Option<CopyRange>,
}

/// A closed inclusive source range; open and suffix ranges are not copy inputs.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct CopyRange {
    first: u64,
    last: u64,
}

impl CopyRange {
    /// Parse one exact `bytes=first-last` expression without normalization.
    ///
    /// # Errors
    /// Rejects multiple, open, suffix, reversed and overflowing ranges.
    pub fn parse(value: &str) -> Result<Self, crate::object::ObjectValueError> {
        let invalid = || crate::object::ObjectValueError::ReadRange;

        let (first, last) = value
            .strip_prefix("bytes=")
            .and_then(|value| value.split_once('-'))
            .ok_or_else(invalid)?;

        if first.is_empty() || last.is_empty() {
            return Err(invalid());
        }

        if !first
            .bytes()
            .chain(last.bytes())
            .all(|byte| byte.is_ascii_digit())
        {
            return Err(invalid());
        }

        let first: u64 = first.parse().map_err(|_| invalid())?;
        let last: u64 = last.parse().map_err(|_| invalid())?;

        if first > last {
            return Err(invalid());
        }

        if last
            .checked_sub(first)
            .and_then(|value| value.checked_add(1))
            .is_none()
        {
            return Err(invalid());
        }

        Ok(Self { first, last })
    }

    #[must_use]
    pub const fn first(self) -> u64 {
        self.first
    }

    #[must_use]
    pub const fn last(self) -> u64 {
        self.last
    }

    pub(crate) fn length(self, source: ByteLength) -> Result<ByteLength, OperationError> {
        if source.get() <= 5 * 1024 * 1024 || self.last >= source.get() {
            return Err(OperationError::new(OperationFailure::InvalidRange));
        }

        ByteLength::new(self.last - self.first + 1)
            .map_err(|error| OperationError::caused(OperationFailure::InvalidArgument, error))
    }
}

/// The accepted part's new independent bytes and publication time.
#[derive(Debug)]
pub struct CopiedPart {
    pub facts: PayloadFacts,
    pub last_modified: EpochMillis,
}

impl CopyPartRequest {
    pub(crate) fn source_request(&self) -> ReadRequest {
        ReadRequest {
            bucket: self.source.bucket.clone(),
            key: self.source.key.clone(),
            expected_owner: self.expected_source_owner.clone(),
            conditions: self.source_conditions.clone(),
            range: None,
            part_number: None,
            response: Representation::default(),
        }
    }

    pub(crate) fn destination(&self, length: ByteLength) -> Result<PartRequest, OperationError> {
        let length = self
            .source_range
            .map_or(Ok(length), |range| range.length(length))?;

        if length.get() > PART_MAX_BYTES {
            return Err(OperationError::new(OperationFailure::EntityTooLarge));
        }

        Ok(PartRequest {
            bucket: self.bucket.clone(),
            key: self.key.clone(),
            upload_id: self.upload_id.clone(),
            part_number: self.part_number,
            expected_owner: self.expected_owner.clone(),
            content_length: Some(length),
            checksum_algorithm: None,
            expected: ExpectedDigests::default(),
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn closed_ranges_preserve_bounds_and_source_eligibility() {
        for invalid in [
            "bytes=0-",
            "bytes=-1",
            "bytes=2-1",
            "bytes=0-1,3-4",
            "bytes=0-18446744073709551615",
            "bytes=0-18446744073709551616",
            "bytes=+0-1",
        ] {
            assert!(CopyRange::parse(invalid).is_err(), "{invalid}");
        }

        let range = CopyRange::parse("bytes=0-0").unwrap();

        for size in [4_999_999, 5_000_000, 5_000_001, 5_242_879, 5_242_880] {
            assert_eq!(
                range
                    .length(ByteLength::new(size).unwrap())
                    .unwrap_err()
                    .failure(),
                OperationFailure::InvalidRange,
                "source_size={size}"
            );
        }

        assert_eq!(
            range
                .length(ByteLength::new(5_242_881).unwrap())
                .unwrap()
                .get(),
            1
        );

        let eof = CopyRange::parse("bytes=5242880-5242880").unwrap();

        assert_eq!(
            eof.length(ByteLength::new(5_242_881).unwrap())
                .unwrap()
                .get(),
            1
        );

        assert!(
            CopyRange::parse("bytes=5242881-5242881")
                .unwrap()
                .length(ByteLength::new(5_242_881).unwrap())
                .is_err()
        );
    }
}
