use md5::Digest as _;

use super::{IntegrityError, decode};

/// Flexible checksum algorithms with qualified binary and protocol encodings.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ChecksumAlgorithm {
    Crc32,
    Crc32c,
    Crc64Nvme,
    Sha1,
    Sha256,
}

impl ChecksumAlgorithm {
    /// Parse the exact case-sensitive S3 spelling.
    ///
    /// # Errors
    /// Unknown algorithms are unsupported, never aliases for a default.
    pub fn parse(value: &str) -> Result<Self, IntegrityError> {
        match value {
            "CRC32" => Ok(Self::Crc32),
            "CRC32C" => Ok(Self::Crc32c),
            "CRC64NVME" => Ok(Self::Crc64Nvme),
            "SHA1" => Ok(Self::Sha1),
            "SHA256" => Ok(Self::Sha256),
            _ => Err(IntegrityError::UnsupportedAlgorithm),
        }
    }

    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Crc32 => "CRC32",
            Self::Crc32c => "CRC32C",
            Self::Crc64Nvme => "CRC64NVME",
            Self::Sha1 => "SHA1",
            Self::Sha256 => "SHA256",
        }
    }

    /// Multipart mode support; a single PUT always has a full-object checksum.
    #[must_use]
    pub const fn supports_multipart(self, mode: ChecksumType) -> bool {
        match mode {
            ChecksumType::FullObject => {
                matches!(self, Self::Crc32 | Self::Crc32c | Self::Crc64Nvme)
            }
            ChecksumType::Composite => !matches!(self, Self::Crc64Nvme),
        }
    }
}

/// Whether a protocol value hashes payload bytes or selected part digests.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ChecksumType {
    FullObject,
    Composite,
}

/// Fixed-width decoded digest, without a composite part-count suffix.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Checksum {
    Crc32([u8; 4]),
    Crc32c([u8; 4]),
    Crc64Nvme([u8; 8]),
    Sha1([u8; 20]),
    Sha256([u8; 32]),
}

impl Checksum {
    /// Decode canonical padded base64 of the algorithm's exact digest width.
    ///
    /// # Errors
    /// Rejects invalid padding, unused bits, alphabets, widths and suffixes.
    pub fn parse(algorithm: ChecksumAlgorithm, value: &str) -> Result<Self, IntegrityError> {
        Ok(match algorithm {
            ChecksumAlgorithm::Crc32 => Self::Crc32(decode(value)?),
            ChecksumAlgorithm::Crc32c => Self::Crc32c(decode(value)?),
            ChecksumAlgorithm::Crc64Nvme => Self::Crc64Nvme(decode(value)?),
            ChecksumAlgorithm::Sha1 => Self::Sha1(decode(value)?),
            ChecksumAlgorithm::Sha256 => Self::Sha256(decode(value)?),
        })
    }

    #[must_use]
    pub const fn algorithm(self) -> ChecksumAlgorithm {
        match self {
            Self::Crc32(_) => ChecksumAlgorithm::Crc32,
            Self::Crc32c(_) => ChecksumAlgorithm::Crc32c,
            Self::Crc64Nvme(_) => ChecksumAlgorithm::Crc64Nvme,
            Self::Sha1(_) => ChecksumAlgorithm::Sha1,
            Self::Sha256(_) => ChecksumAlgorithm::Sha256,
        }
    }

    #[must_use]
    pub fn as_bytes(&self) -> &[u8] {
        match self {
            Self::Crc32(bytes) | Self::Crc32c(bytes) => bytes,
            Self::Crc64Nvme(bytes) => bytes,
            Self::Sha1(bytes) => bytes,
            Self::Sha256(bytes) => bytes,
        }
    }

    #[must_use]
    pub fn encode(self) -> String {
        base64_simd::STANDARD.encode_to_string(self.as_bytes())
    }
}

/// Stored or returned checksum, retaining full/composite meaning explicitly.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ObjectChecksum {
    digest: Checksum,
    part_count: Option<u32>,
}

impl ObjectChecksum {
    #[must_use]
    pub const fn full(digest: Checksum) -> Self {
        Self {
            digest,
            part_count: None,
        }
    }

    /// Hash ordered binary part digests with constant working space.
    /// Callers separately validate part identities, ordering and size constraints.
    ///
    /// # Errors
    /// Rejects mixed algorithms, unsupported modes, empty or overlong selections.
    pub fn composite(
        algorithm: ChecksumAlgorithm,
        parts: impl IntoIterator<Item = Checksum>,
    ) -> Result<Self, IntegrityError> {
        if !algorithm.supports_multipart(ChecksumType::Composite) {
            return Err(IntegrityError::InvalidChecksumType);
        }

        let mut hasher = Hasher::new(algorithm);
        let mut count = 0_u32;

        for part in parts {
            if part.algorithm() != algorithm {
                return Err(IntegrityError::AlgorithmMismatch);
            }

            if count == 10_000 {
                return Err(IntegrityError::InvalidPartCount);
            }

            hasher.update(part.as_bytes());
            count += 1;
        }

        if count == 0 {
            return Err(IntegrityError::InvalidPartCount);
        }

        Ok(Self {
            digest: hasher.finish(),
            part_count: Some(count),
        })
    }

    /// Decode persisted/protocol values without repairing malformed state.
    ///
    /// # Errors
    /// Rejects mode/suffix mismatch, noncanonical counts and invalid digests.
    pub fn parse(
        algorithm: ChecksumAlgorithm,
        mode: ChecksumType,
        value: &str,
    ) -> Result<Self, IntegrityError> {
        if value.len() > 50 {
            return Err(IntegrityError::InvalidDigest);
        }

        if mode == ChecksumType::FullObject {
            return Ok(Self::full(Checksum::parse(algorithm, value)?));
        }

        if !algorithm.supports_multipart(mode) {
            return Err(IntegrityError::InvalidChecksumType);
        }

        let (digest, count) = value
            .split_once('-')
            .ok_or(IntegrityError::InvalidPartCount)?;

        if count.starts_with('0') || count.is_empty() || count.len() > 5 {
            return Err(IntegrityError::InvalidPartCount);
        }

        if !count.bytes().all(|byte| byte.is_ascii_digit()) {
            return Err(IntegrityError::InvalidPartCount);
        }

        let part_count = count
            .parse::<u32>()
            .map_err(|_| IntegrityError::InvalidPartCount)?;

        if !(1..=10_000).contains(&part_count) {
            return Err(IntegrityError::InvalidPartCount);
        }

        Ok(Self {
            digest: Checksum::parse(algorithm, digest)?,
            part_count: Some(part_count),
        })
    }

    #[must_use]
    pub const fn digest(self) -> Checksum {
        self.digest
    }

    #[must_use]
    pub const fn checksum_type(self) -> ChecksumType {
        if self.part_count.is_some() {
            ChecksumType::Composite
        } else {
            ChecksumType::FullObject
        }
    }

    #[must_use]
    pub const fn part_count(self) -> Option<u32> {
        self.part_count
    }

    #[must_use]
    pub fn encode(self) -> String {
        let digest = self.digest.encode();

        match self.part_count {
            Some(count) => format!("{digest}-{count}"),
            None => digest,
        }
    }
}

pub(super) enum Hasher {
    Crc32(crc_fast::Digest),
    Crc32c(crc_fast::Digest),
    Crc64Nvme(crc_fast::Digest),
    Sha1(sha1::Sha1),
    Sha256(sha2::Sha256),
}

impl Hasher {
    pub(super) fn new(algorithm: ChecksumAlgorithm) -> Self {
        use crc_fast::{CrcAlgorithm, Digest};

        match algorithm {
            ChecksumAlgorithm::Crc32 => Self::Crc32(Digest::new(CrcAlgorithm::Crc32IsoHdlc)),
            ChecksumAlgorithm::Crc32c => Self::Crc32c(Digest::new(CrcAlgorithm::Crc32Iscsi)),
            ChecksumAlgorithm::Crc64Nvme => Self::Crc64Nvme(Digest::new(CrcAlgorithm::Crc64Nvme)),
            ChecksumAlgorithm::Sha1 => Self::Sha1(sha1::Sha1::new()),
            ChecksumAlgorithm::Sha256 => Self::Sha256(sha2::Sha256::new()),
        }
    }

    pub(super) fn update(&mut self, bytes: &[u8]) {
        match self {
            Self::Crc32(hasher) | Self::Crc32c(hasher) | Self::Crc64Nvme(hasher) => {
                hasher.update(bytes);
            }
            Self::Sha1(hasher) => hasher.update(bytes),
            Self::Sha256(hasher) => hasher.update(bytes),
        }
    }

    pub(super) fn finish(self) -> Checksum {
        match self {
            Self::Crc32(hasher) => Checksum::Crc32(crc32_bytes(&hasher)),
            Self::Crc32c(hasher) => Checksum::Crc32c(crc32_bytes(&hasher)),
            Self::Crc64Nvme(hasher) => Checksum::Crc64Nvme(hasher.finalize().to_be_bytes()),
            Self::Sha1(hasher) => Checksum::Sha1(hasher.finalize().into()),
            Self::Sha256(hasher) => Checksum::Sha256(hasher.finalize().into()),
        }
    }
}

fn crc32_bytes(hasher: &crc_fast::Digest) -> [u8; 4] {
    let bytes = hasher.finalize().to_be_bytes();
    [bytes[4], bytes[5], bytes[6], bytes[7]]
}
