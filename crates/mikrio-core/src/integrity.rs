//! Shared bounded payload verification. The caller owns framing and must reach
//! validated EOF before calling `finish`; these facts alone do not prove commit.

use std::fmt;

use md5::{Digest as _, Md5};

mod checksum;
pub use checksum::{Checksum, ChecksumAlgorithm, ChecksumType, ObjectChecksum};
mod layout;
pub use layout::{LayoutError, MultipartLayout, PartLayout};
mod put;
pub use put::{PutIntegrity, PutIntegrityInput};

/// Expected protocol digests, decoded before payload work whenever available.
/// Trailers use the same constructor after the transport has validated them.
#[derive(Debug, Clone, Copy, Default)]
pub struct ExpectedDigests {
    md5: Option<[u8; 16]>,
    checksum: Option<Checksum>,
}

impl ExpectedDigests {
    /// Decode canonical padded S3 base64, with a fixed size before allocation.
    ///
    /// # Errors
    /// Rejects bad alphabets, padding, unused bits, digest sizes and suffixes.
    pub fn parse(md5: Option<&str>, checksum: Option<Checksum>) -> Result<Self, IntegrityError> {
        Ok(Self {
            md5: md5.map(decode).transpose()?,
            checksum,
        })
    }

    /// The validated flexible checksum assertion, if one was supplied.
    #[must_use]
    pub const fn checksum(self) -> Option<Checksum> {
        self.checksum
    }
}

fn decode<const N: usize>(value: &str) -> Result<[u8; N], IntegrityError> {
    if value.len() != N.div_ceil(3) * 4 {
        return Err(IntegrityError::InvalidDigest);
    }

    let bytes = base64_simd::STANDARD
        .decode_to_vec(value)
        .map_err(|_| IntegrityError::InvalidDigest)?;

    if base64_simd::STANDARD.encode_to_string(&bytes) != value {
        return Err(IntegrityError::InvalidDigest);
    }

    bytes.try_into().map_err(|_| IntegrityError::InvalidDigest)
}

/// A constant-space digest owner. Updates borrow their bytes and retain none.
/// A failed update poisons this owner, so ignoring its error cannot allow finish.
pub struct PayloadVerifier {
    declared_length: Option<u64>,
    max_length: u64,
    length: u64,
    expected: ExpectedDigests,
    md5: Md5,
    checksum: checksum::Hasher,
    transport: Option<Box<checksum::Hasher>>,
    failure: Option<IntegrityError>,
}

impl PayloadVerifier {
    /// Start a payload with an operation-specific finite size bound.
    ///
    /// # Errors
    /// Refuses a declared length beyond that bound or an expected checksum
    /// using a different algorithm, before any payload is read.
    pub fn new(
        declared_length: Option<u64>,
        max_length: u64,
        algorithm: ChecksumAlgorithm,
        expected: ExpectedDigests,
    ) -> Result<Self, IntegrityError> {
        if declared_length.is_some_and(|length| length > max_length) {
            return Err(IntegrityError::TooLarge);
        }

        if expected
            .checksum
            .is_some_and(|checksum| checksum.algorithm() != algorithm)
        {
            return Err(IntegrityError::AlgorithmMismatch);
        }

        Ok(Self {
            declared_length,
            max_length,
            length: 0,
            expected,
            md5: Md5::new(),
            checksum: checksum::Hasher::new(algorithm),
            transport: None,
            failure: None,
        })
    }

    /// Defaulted multipart uploads retain CRC64NVME storage integrity while
    /// independently verifying a client's declared request checksum in one pass.
    pub(crate) fn multipart(
        declared_length: Option<u64>,
        max_length: u64,
        algorithm: ChecksumAlgorithm,
        expected: ExpectedDigests,
        transport: Option<ChecksumAlgorithm>,
    ) -> Result<Self, IntegrityError> {
        let Some(transport) = transport.filter(|value| *value != algorithm) else {
            return Self::new(declared_length, max_length, algorithm, expected);
        };

        if expected
            .checksum
            .is_some_and(|value| value.algorithm() != transport)
        {
            return Err(IntegrityError::AlgorithmMismatch);
        }

        let mut verifier = Self::new(
            declared_length,
            max_length,
            algorithm,
            ExpectedDigests::default(),
        )?;

        verifier.expected = expected;
        verifier.transport = Some(Box::new(checksum::Hasher::new(transport)));
        Ok(verifier)
    }

    /// Account for and hash one decoded chunk, including an empty chunk.
    ///
    /// # Errors
    /// Refuses overflow, an exceeded size bound or extra declared-length bytes.
    /// Subsequent updates and finish return the same failure.
    pub fn update(&mut self, bytes: &[u8]) -> Result<(), IntegrityError> {
        if let Some(error) = self.failure {
            return Err(error);
        }

        let result = self.next_length(bytes.len());

        let next = match result {
            Ok(next) => next,
            Err(error) => {
                self.failure = Some(error);
                return Err(error);
            }
        };

        self.md5.update(bytes);
        self.checksum.update(bytes);

        if let Some(transport) = &mut self.transport {
            transport.update(bytes);
        }

        self.length = next;
        Ok(())
    }

    /// Consume this owner after validated framing/EOF. Header and terminal
    /// digests must both match; one never overrides a contradictory other value.
    ///
    /// # Errors
    /// Returns any prior update failure, a short length, a mismatched digest
    /// or a terminal checksum using a different algorithm.
    pub fn finish(self, terminal: ExpectedDigests) -> Result<PayloadFacts, IntegrityError> {
        if let Some(error) = self.failure {
            return Err(error);
        }

        if self
            .declared_length
            .is_some_and(|length| length != self.length)
        {
            return Err(IntegrityError::LengthMismatch);
        }

        let facts = PayloadFacts {
            length: self.length,
            md5: self.md5.finalize().into(),
            checksum: self.checksum.finish(),
        };

        let transport = self.transport.map(|value| value.finish());

        for expected in [self.expected, terminal] {
            let actual = transport.unwrap_or(facts.checksum);

            if expected
                .checksum
                .is_some_and(|checksum| checksum.algorithm() != actual.algorithm())
            {
                return Err(IntegrityError::AlgorithmMismatch);
            }

            if expected.md5.is_some_and(|digest| digest != facts.md5) {
                return Err(IntegrityError::DigestMismatch);
            }

            if expected.checksum.is_some_and(|digest| digest != actual) {
                return Err(IntegrityError::DigestMismatch);
            }
        }

        Ok(facts)
    }

    fn next_length(&self, length: usize) -> Result<u64, IntegrityError> {
        let length = u64::try_from(length).map_err(|_| IntegrityError::TooLarge)?;

        let next = self
            .length
            .checked_add(length)
            .ok_or(IntegrityError::TooLarge)?;

        if next > self.max_length {
            return Err(IntegrityError::TooLarge);
        }

        if self.declared_length.is_some_and(|declared| next > declared) {
            return Err(IntegrityError::LengthMismatch);
        }

        Ok(next)
    }
}

/// Verified byte facts, distinct from logical generation or mutation evidence.
#[derive(Debug, PartialEq, Eq)]
pub struct PayloadFacts {
    length: u64,
    md5: [u8; 16],
    checksum: Checksum,
}

impl PayloadFacts {
    /// Actual decoded byte count.
    #[must_use]
    pub const fn length(&self) -> u64 {
        self.length
    }

    /// Binary MD5 used for ordinary entity tags and multipart representation.
    #[must_use]
    pub const fn md5(&self) -> &[u8; 16] {
        &self.md5
    }

    /// Ordinary PUT/part entity tag, including its protocol quotes.
    #[must_use]
    pub fn etag(&self) -> String {
        const HEX: &[u8; 16] = b"0123456789abcdef";
        let mut value = String::with_capacity(34);
        value.push('"');

        for byte in self.md5 {
            value.push(char::from(HEX[usize::from(byte >> 4)]));
            value.push(char::from(HEX[usize::from(byte & 15)]));
        }

        value.push('"');
        value
    }

    /// Verified full-payload digest; this never describes a composite object.
    #[must_use]
    pub const fn checksum(&self) -> Checksum {
        self.checksum
    }
}

/// Shared semantic classification, without payload bytes or digest disclosure.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum IntegrityError {
    /// Algorithm is outside the qualified subset.
    UnsupportedAlgorithm,
    /// Expected digest or selected part uses a different algorithm.
    AlgorithmMismatch,
    /// Algorithm cannot represent the selected multipart mode.
    InvalidChecksumType,
    /// Composite count is absent, noncanonical or outside 1-10,000.
    InvalidPartCount,
    /// Expected digest is not a canonical supported encoding.
    InvalidDigest,
    /// Actual bytes disagree with an expected digest.
    DigestMismatch,
    /// Actual bytes disagree with the declared length.
    LengthMismatch,
    /// Checked byte count exceeds the operation's size bound.
    TooLarge,
}

impl fmt::Display for IntegrityError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(match self {
            Self::UnsupportedAlgorithm => "unsupported checksum algorithm",
            Self::AlgorithmMismatch => "checksum algorithm mismatch",
            Self::InvalidChecksumType => "unsupported checksum type",
            Self::InvalidPartCount => "invalid checksum part count",
            Self::InvalidDigest => "invalid checksum encoding",
            Self::DigestMismatch => "payload checksum mismatch",
            Self::LengthMismatch => "payload length mismatch",
            Self::TooLarge => "payload exceeds size bound",
        })
    }
}

impl std::error::Error for IntegrityError {}

#[cfg(test)]
mod tests;
