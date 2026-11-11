/// Flexible checksum algorithms supported for object payloads.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub enum ChecksumAlgorithm {
    /// IEEE CRC-32.
    Crc32,
    /// Castagnoli CRC-32C.
    Crc32c,
    /// `NVMe` CRC-64.
    Crc64Nvme,
    /// SHA-1, as a protocol checksum rather than an authenticity guarantee.
    Sha1,
    /// SHA-256.
    Sha256,
}

impl ChecksumAlgorithm {
    /// Exact S3 spelling.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        self.into_core().as_str()
    }

    pub(crate) const fn into_core(self) -> mikrio_core::integrity::ChecksumAlgorithm {
        use mikrio_core::integrity::ChecksumAlgorithm as CoreAlgorithm;

        match self {
            Self::Crc32 => CoreAlgorithm::Crc32,
            Self::Crc32c => CoreAlgorithm::Crc32c,
            Self::Crc64Nvme => CoreAlgorithm::Crc64Nvme,
            Self::Sha1 => CoreAlgorithm::Sha1,
            Self::Sha256 => CoreAlgorithm::Sha256,
        }
    }

    pub(crate) const fn from_core(value: mikrio_core::integrity::ChecksumAlgorithm) -> Self {
        use mikrio_core::integrity::ChecksumAlgorithm as CoreAlgorithm;

        match value {
            CoreAlgorithm::Crc32 => Self::Crc32,
            CoreAlgorithm::Crc32c => Self::Crc32c,
            CoreAlgorithm::Crc64Nvme => Self::Crc64Nvme,
            CoreAlgorithm::Sha1 => Self::Sha1,
            CoreAlgorithm::Sha256 => Self::Sha256,
        }
    }
}
