//! Convert the qualified HTTP digest envelope into the single core verifier.

use mikrio_core::integrity::{
    Checksum, ChecksumAlgorithm, ExpectedDigests, PutIntegrity, PutIntegrityInput,
};
use mikrio_core::object::ByteLength;
use s3s::dto::PutObjectInput;
use s3s::{S3Request, S3Result, s3_error};

use super::error::{integrity_error, upload};

pub(super) struct Integrity {
    pub(super) input: PutIntegrity,
    pub(super) length: Option<ByteLength>,
    trailer: Option<(&'static str, ChecksumAlgorithm)>,
    headers: Option<s3s::TrailingHeaders>,
}

impl Integrity {
    pub(super) fn parse(request: &S3Request<PutObjectInput>) -> S3Result<Self> {
        Self::from_request(request, request.input.content_length)
    }

    pub(super) fn from_request<T>(
        request: &S3Request<T>,
        content_length: Option<i64>,
    ) -> S3Result<Self> {
        let headers = &request.headers;

        // Digest assertions must retain explicit empty values for validation.
        // The codec's optional strings erase them and omit the SDK declaration.
        if headers.keys().any(|name| {
            if !name.as_str().starts_with("x-amz-checksum-") {
                return false;
            }

            !matches!(
                name.as_str(),
                "x-amz-checksum-algorithm"
                    | "x-amz-checksum-mode"
                    | "x-amz-checksum-crc32"
                    | "x-amz-checksum-crc32c"
                    | "x-amz-checksum-crc64nvme"
                    | "x-amz-checksum-sha1"
                    | "x-amz-checksum-sha256"
            )
        }) {
            return Err(s3_error!(NotImplemented));
        }

        let trailer = request
            .headers
            .get("x-amz-trailer")
            .map(
                |value| match value.to_str().map_err(|_| s3_error!(InvalidRequest))? {
                    "x-amz-checksum-crc32" => {
                        Ok(("x-amz-checksum-crc32", ChecksumAlgorithm::Crc32))
                    }
                    "x-amz-checksum-crc32c" => {
                        Ok(("x-amz-checksum-crc32c", ChecksumAlgorithm::Crc32c))
                    }
                    "x-amz-checksum-crc64nvme" => {
                        Ok(("x-amz-checksum-crc64nvme", ChecksumAlgorithm::Crc64Nvme))
                    }
                    "x-amz-checksum-sha1" => Ok(("x-amz-checksum-sha1", ChecksumAlgorithm::Sha1)),
                    "x-amz-checksum-sha256" => {
                        Ok(("x-amz-checksum-sha256", ChecksumAlgorithm::Sha256))
                    }
                    _ => Err(s3_error!(NotImplemented)),
                },
            )
            .transpose()?;

        let mut declared = None;

        for name in ["x-amz-checksum-algorithm", "x-amz-sdk-checksum-algorithm"] {
            let Some(value) = super::input::header(headers, name)? else {
                continue;
            };

            let algorithm = ChecksumAlgorithm::parse(&value).map_err(integrity_error)?;

            if declared
                .replace(algorithm)
                .is_some_and(|prior| prior != algorithm)
            {
                return Err(s3_error!(BadDigest));
            }
        }

        let expected = PutIntegrityInput {
            content_md5: super::input::header(headers, "content-md5")?,
            checksum_algorithm: declared.or(trailer.map(|(_, algorithm)| algorithm)),
            checksum_crc32: super::input::header(headers, "x-amz-checksum-crc32")?,
            checksum_crc32c: super::input::header(headers, "x-amz-checksum-crc32c")?,
            checksum_crc64_nvme: super::input::header(headers, "x-amz-checksum-crc64nvme")?,
            checksum_sha1: super::input::header(headers, "x-amz-checksum-sha1")?,
            checksum_sha256: super::input::header(headers, "x-amz-checksum-sha256")?,
        };

        let digest_count = [
            &expected.checksum_crc32,
            &expected.checksum_crc32c,
            &expected.checksum_crc64_nvme,
            &expected.checksum_sha1,
            &expected.checksum_sha256,
        ]
        .into_iter()
        .flatten()
        .count();

        if digest_count > 1 || (declared.is_some() && digest_count == 0 && trailer.is_none()) {
            return Err(s3_error!(InvalidRequest));
        }

        if declared
            .zip(trailer)
            .is_some_and(|(declared, (_, trailing))| declared != trailing)
        {
            return Err(s3_error!(BadDigest));
        }

        let integrity = PutIntegrity::new(expected).map_err(integrity_error)?;

        // s3s replaces an authenticated aws-chunked length with its decoded
        // length. Do not reinterpret unrelated decoded-length headers here.
        let length = content_length
            .map(u64::try_from)
            .transpose()
            .map_err(|_| s3_error!(InvalidArgument))?;

        integrity.verifier(length).map_err(integrity_error)?;

        let length = length
            .map(ByteLength::new)
            .transpose()
            .map_err(|_| s3_error!(InvalidArgument))?;

        Ok(Self {
            input: integrity,
            length,
            trailer,
            headers: request.trailing_headers.clone(),
        })
    }

    pub(super) fn envelope<T>(request: &S3Request<T>) -> S3Result<()> {
        let bytes = &request
            .extensions
            .get::<s3s::service::RequestBodyBytes>()
            .ok_or_else(|| s3_error!(InternalError))?
            .0;

        Self::verify(request, bytes)
    }

    /// The codec has already consumed and rejected any nonempty request body.
    pub(super) fn bodyless<T>(request: &S3Request<T>) -> S3Result<()> {
        Self::verify(request, &[])
    }

    pub(super) fn finish(self) -> std::io::Result<ExpectedDigests> {
        self.terminal().map_err(|error| upload(&error))
    }

    fn verify<T>(request: &S3Request<T>, bytes: &[u8]) -> S3Result<()> {
        let integrity = Self::from_request(request, None)?;
        let mut verifier = integrity.input.verifier(None).map_err(integrity_error)?;
        verifier.update(bytes).map_err(integrity_error)?;

        verifier
            .finish(integrity.terminal()?)
            .map_err(integrity_error)?;

        Ok(())
    }

    fn terminal(self) -> S3Result<ExpectedDigests> {
        let Some((name, algorithm)) = self.trailer else {
            return Ok(ExpectedDigests::default());
        };

        let headers = self
            .headers
            .and_then(|headers| headers.take())
            .ok_or_else(|| s3_error!(InvalidRequest))?;

        if headers.get_all(name).iter().count() != 1 {
            return Err(s3_error!(InvalidRequest));
        }

        let value = headers
            .get(name)
            .ok_or_else(|| s3_error!(InvalidRequest))?
            .to_str()
            .map_err(|_| s3_error!(InvalidDigest))?;

        let checksum = Checksum::parse(algorithm, value).map_err(integrity_error)?;
        ExpectedDigests::parse(None, Some(checksum)).map_err(integrity_error)
    }
}
