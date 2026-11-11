use super::{Checksum, ChecksumAlgorithm, ExpectedDigests, IntegrityError, PayloadVerifier};

/// Supplied single-object integrity values, before payload consumption.
#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct PutIntegrityInput {
    pub content_md5: Option<String>,
    pub checksum_algorithm: Option<ChecksumAlgorithm>,
    pub checksum_crc32: Option<String>,
    pub checksum_crc32c: Option<String>,
    pub checksum_crc64_nvme: Option<String>,
    pub checksum_sha1: Option<String>,
    pub checksum_sha256: Option<String>,
}

/// One selected flexible algorithm and an independent optional Content-MD5.
#[derive(Debug, Clone)]
pub struct PutIntegrity {
    input: PutIntegrityInput,
    algorithm: ChecksumAlgorithm,
    expected: ExpectedDigests,
}

impl PutIntegrity {
    /// Validate every supplied digest before reading the source.
    ///
    /// # Errors
    /// Rejects multiple flexible digests, a conflicting selection, or malformed
    /// digest encodings. An omitted selection uses the digest's algorithm, or
    /// CRC64NVME when no flexible digest was supplied.
    pub fn new(input: PutIntegrityInput) -> Result<Self, IntegrityError> {
        let mut checksum = None;

        for (algorithm, value) in [
            (ChecksumAlgorithm::Crc32, &input.checksum_crc32),
            (ChecksumAlgorithm::Crc32c, &input.checksum_crc32c),
            (ChecksumAlgorithm::Crc64Nvme, &input.checksum_crc64_nvme),
            (ChecksumAlgorithm::Sha1, &input.checksum_sha1),
            (ChecksumAlgorithm::Sha256, &input.checksum_sha256),
        ] {
            let Some(value) = value else { continue };

            if checksum.is_some() {
                return Err(IntegrityError::AlgorithmMismatch);
            }

            if input
                .checksum_algorithm
                .is_some_and(|selected| selected != algorithm)
            {
                return Err(IntegrityError::AlgorithmMismatch);
            }

            checksum = Some(Checksum::parse(algorithm, value)?);
        }

        let algorithm = input
            .checksum_algorithm
            .or_else(|| checksum.map(Checksum::algorithm))
            .unwrap_or(ChecksumAlgorithm::Crc64Nvme);

        let expected = ExpectedDigests::parse(input.content_md5.as_deref(), checksum)?;

        Ok(Self {
            input,
            algorithm,
            expected,
        })
    }

    #[must_use]
    pub const fn fields(&self) -> &PutIntegrityInput {
        &self.input
    }

    #[must_use]
    pub const fn algorithm(&self) -> ChecksumAlgorithm {
        self.algorithm
    }

    #[must_use]
    pub const fn expected(&self) -> ExpectedDigests {
        self.expected
    }

    /// Verify decoded PUT bytes with the shared single-operation size ceiling.
    ///
    /// # Errors
    /// Rejects an oversized declaration before payload consumption. The returned
    /// verifier also bounds streams whose length is unknown until EOF.
    pub fn verifier(
        &self,
        declared_length: Option<u64>,
    ) -> Result<PayloadVerifier, IntegrityError> {
        PayloadVerifier::new(
            declared_length,
            crate::object::PUT_MAX_BYTES,
            self.algorithm,
            self.expected,
        )
    }
}
