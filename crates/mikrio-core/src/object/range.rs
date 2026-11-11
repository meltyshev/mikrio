//! Validated single byte ranges, resolved against one authorized generation.

use super::{ByteLength, ObjectValueError};
use crate::{OperationError, OperationFailure};

/// A single closed, open-ended or suffix HTTP byte range.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ByteRange {
    value: String,
    bounds: Bounds,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Bounds {
    From { first: u64, last: Option<u64> },
    Suffix(u64),
}

/// Nonempty inclusive bounds within the selected object's full byte length.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SelectedRange {
    first: u64,
    last: u64,
}

impl ByteRange {
    /// Retain exact valid syntax without selecting or opening an object.
    ///
    /// # Errors
    /// Rejects malformed, multiple, reversed or overflowing ranges.
    pub fn parse(value: &str) -> Result<Self, ObjectValueError> {
        let (first, last) = value
            .strip_prefix("bytes=")
            .and_then(|value| value.split_once('-'))
            .ok_or(ObjectValueError::ReadRange)?;

        let bounds = if first.is_empty() {
            Bounds::Suffix(decimal(last)?)
        } else {
            let first = decimal(first)?;
            let last = (!last.is_empty()).then(|| decimal(last)).transpose()?;

            if last.is_some_and(|last| last < first) {
                return Err(ObjectValueError::ReadRange);
            }

            Bounds::From { first, last }
        };

        Ok(Self {
            value: value.to_owned(),
            bounds,
        })
    }

    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.value
    }

    /// Resolve only after authorization and read-condition evaluation.
    ///
    /// # Errors
    /// Returns `InvalidRange` when no bytes can satisfy this range.
    pub fn select(&self, length: ByteLength) -> Result<SelectedRange, OperationError> {
        let length = length.get();

        if length == 0 {
            return Err(OperationError::new(OperationFailure::InvalidRange));
        }

        let (first, last) = match self.bounds {
            Bounds::From { first, last } if first < length => {
                (first, last.unwrap_or(length - 1).min(length - 1))
            }
            Bounds::Suffix(suffix) if suffix != 0 => (length.saturating_sub(suffix), length - 1),
            _ => return Err(OperationError::new(OperationFailure::InvalidRange)),
        };

        Ok(SelectedRange { first, last })
    }
}

impl SelectedRange {
    #[must_use]
    pub const fn offset(self) -> u64 {
        self.first
    }

    #[must_use]
    pub const fn length(self) -> u64 {
        self.last - self.first + 1
    }

    #[must_use]
    pub fn content_range(self, full_length: ByteLength) -> String {
        format!("bytes {}-{}/{}", self.first, self.last, full_length.get())
    }
}

fn decimal(value: &str) -> Result<u64, ObjectValueError> {
    if value.is_empty() || !value.bytes().all(|byte| byte.is_ascii_digit()) {
        return Err(ObjectValueError::ReadRange);
    }

    value.parse().map_err(|_| ObjectValueError::ReadRange)
}

#[cfg(test)]
mod tests;
