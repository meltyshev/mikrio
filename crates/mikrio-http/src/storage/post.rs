//! Form values enter the same authenticated PUT reservation/publication worker.

use mikrio_core::integrity::{ChecksumAlgorithm, ExpectedDigests, PutIntegrity, PutIntegrityInput};
use mikrio_core::object::{
    BucketName, ObjectKey, PutRequest, Representation, RepresentationInput, UserMetadata,
    WriteConditions,
};
use s3s::xml::Deserialize;
use s3s::{S3Request, S3Response, S3Result, dto, s3_error};

use super::{Storage, body, error, identity, tagging};

pub(super) async fn post(
    storage: &Storage,
    mut request: S3Request<dto::PostObjectInput>,
) -> S3Result<S3Response<dto::PostObjectOutput>> {
    let identity = identity(&request)?;

    // POST integrity belongs to the signed form and covers only file bytes.
    // Outer checksum assertions cannot be verified as either form or PUT data.
    if request
        .headers
        .keys()
        .any(|name| name.as_str().starts_with("x-amz-checksum-"))
    {
        return Err(s3_error!(NotImplemented));
    }

    if super::input::has_any(
        &request.headers,
        &[
            "content-md5",
            "x-amz-sdk-checksum-algorithm",
            "x-amz-trailer",
        ],
    ) {
        return Err(s3_error!(NotImplemented));
    }

    let input = &request.input;

    if input
        .acl
        .as_ref()
        .is_some_and(|value| value.as_str() != "bucket-owner-full-control")
    {
        return Err(s3_error!(AccessControlListNotSupported));
    }

    if input
        .storage_class
        .as_ref()
        .is_some_and(|value| value.as_str() != "STANDARD")
    {
        return Err(s3_error!(NotImplemented));
    }

    let integrity = integrity(input)?;

    let requested_checksum = input.checksum_algorithm.is_some()
        || input.checksum_crc32.is_some()
        || input.checksum_crc32c.is_some()
        || input.checksum_sha1.is_some()
        || input.checksum_sha256.is_some();

    let tags = input
        .tagging
        .as_deref()
        .map(|xml| {
            let mut decoder = s3s::xml::Deserializer::new_strict(xml.as_bytes());

            let value =
                dto::Tagging::deserialize(&mut decoder).map_err(|_| s3_error!(MalformedXML))?;

            decoder.expect_eof().map_err(|_| s3_error!(MalformedXML))?;
            tagging::configuration(&value)
        })
        .transpose()?;

    let put = PutRequest {
        bucket: BucketName::parse(&input.bucket).map_err(|_| s3_error!(InvalidBucketName))?,
        key: ObjectKey::parse(&input.key).map_err(|_| s3_error!(InvalidArgument))?,
        expected_owner: None,
        // The outer Content-Length includes the complete multipart envelope.
        content_length: None,
        representation: Representation::new(RepresentationInput {
            content_type: input.content_type.clone(),
            content_encoding: input.content_encoding.clone(),
            content_disposition: input.content_disposition.clone(),
            content_language: None,
            cache_control: input.cache_control.clone(),
            expires: crate::access::parse_expires(input.expires.as_deref())?,
        })
        .map_err(|_| s3_error!(InvalidArgument))?,
        metadata: UserMetadata::new(
            input
                .metadata
                .iter()
                .flat_map(|values| values.iter())
                .map(|(key, value)| (key.clone(), value.clone())),
        )
        .map_err(|_| s3_error!(InvalidArgument))?,
        tags,
        integrity,
        conditions: WriteConditions::default(),
    };

    let body = request
        .input
        .body
        .take()
        .ok_or_else(|| s3_error!(MalformedPOSTRequest))?;

    // The codec withholds EOF until the ignored tail and transport terminate.
    let facts = storage
        .0
        .put_object_http(identity, put, body::Upload::post(body), || {
            Ok(ExpectedDigests::default())
        })
        .await
        .map_err(|error| error::operation(&error))?;

    let mut response = S3Response::new(dto::PostObjectOutput {
        e_tag: Some(facts.etag().parse().map_err(|_| s3_error!(InternalError))?),
        ..Default::default()
    });

    if requested_checksum {
        let name = match facts.checksum().algorithm() {
            ChecksumAlgorithm::Crc32 => "x-amz-checksum-crc32",
            ChecksumAlgorithm::Crc32c => "x-amz-checksum-crc32c",
            ChecksumAlgorithm::Sha1 => "x-amz-checksum-sha1",
            ChecksumAlgorithm::Sha256 => "x-amz-checksum-sha256",
            ChecksumAlgorithm::Crc64Nvme => return Err(s3_error!(InternalError)),
        };

        response.headers.insert(
            name,
            facts
                .checksum()
                .encode()
                .parse()
                .map_err(|_| s3_error!(InternalError))?,
        );
    }

    Ok(response)
}

fn integrity(input: &dto::PostObjectInput) -> S3Result<PutIntegrity> {
    let algorithm = input
        .checksum_algorithm
        .as_ref()
        .map(|value| match value.as_str() {
            "CRC32" => Ok(ChecksumAlgorithm::Crc32),
            "CRC32C" => Ok(ChecksumAlgorithm::Crc32c),
            "SHA1" => Ok(ChecksumAlgorithm::Sha1),
            "SHA256" => Ok(ChecksumAlgorithm::Sha256),
            _ => Err(s3_error!(NotImplemented)),
        })
        .transpose()?;

    let count = [
        &input.checksum_crc32,
        &input.checksum_crc32c,
        &input.checksum_sha1,
        &input.checksum_sha256,
    ]
    .into_iter()
    .flatten()
    .count();

    if count > 1 || (algorithm.is_some() && count != 1) {
        return Err(s3_error!(InvalidRequest));
    }

    PutIntegrity::new(PutIntegrityInput {
        content_md5: input.content_md5.clone(),
        checksum_algorithm: algorithm,
        checksum_crc32: input.checksum_crc32.clone(),
        checksum_crc32c: input.checksum_crc32c.clone(),
        checksum_sha1: input.checksum_sha1.clone(),
        checksum_sha256: input.checksum_sha256.clone(),
        ..Default::default()
    })
    .map_err(error::integrity_error)
}
