//! Authenticated multipart adapters; all durable state belongs to the core worker.

mod completion;
mod listing;

use mikrio_core::integrity::{
    ChecksumAlgorithm, ChecksumType, ExpectedDigests, PutIntegrity, PutIntegrityInput,
};
use mikrio_core::multipart::{MultipartChecksum, PartNumber, UploadRequest};
use mikrio_core::object::{BucketName, ObjectKey, UserMetadata};
use s3s::dto;
use s3s::{S3Request, S3Response, S3Result, s3_error};

use super::{
    Storage, body, copy as object_copy, error, identity, input, integrity::Integrity, output,
};
pub(super) use completion::complete;
pub(super) use listing::{parts, uploads};

fn algorithm(headers: &http::HeaderMap) -> S3Result<Option<ChecksumAlgorithm>> {
    input::header(headers, "x-amz-checksum-algorithm")?
        .as_deref()
        .map(ChecksumAlgorithm::parse)
        .transpose()
        .map_err(error::integrity_error)
}

fn checksum_type(headers: &http::HeaderMap) -> S3Result<Option<ChecksumType>> {
    match input::header(headers, "x-amz-checksum-type")?.as_deref() {
        None => Ok(None),
        Some("FULL_OBJECT") => Ok(Some(ChecksumType::FullObject)),
        Some("COMPOSITE") => Ok(Some(ChecksumType::Composite)),
        Some(_) => Err(s3_error!(InvalidArgument)),
    }
}

fn wire_type(value: ChecksumType) -> dto::ChecksumType {
    dto::ChecksumType::from_static(match value {
        ChecksumType::FullObject => "FULL_OBJECT",
        ChecksumType::Composite => "COMPOSITE",
    })
}

fn number(value: i32) -> S3Result<PartNumber> {
    PartNumber::new(u32::try_from(value).map_err(|_| s3_error!(InvalidArgument))?)
        .map_err(|_| s3_error!(InvalidArgument))
}

fn upload(
    bucket: &str,
    key: &str,
    upload_id: &str,
    headers: &http::HeaderMap,
) -> S3Result<UploadRequest> {
    Ok(UploadRequest {
        bucket: BucketName::parse(bucket).map_err(|_| s3_error!(InvalidBucketName))?,
        key: ObjectKey::parse(key).map_err(|_| s3_error!(InvalidArgument))?,
        upload_id: mikrio_core::UploadId::decode(upload_id).map_err(|_| s3_error!(NoSuchUpload))?,
        expected_owner: crate::access::expected_owner(headers)?.map(str::to_owned),
    })
}

fn active_options(headers: &http::HeaderMap) -> S3Result<()> {
    input::write_options(headers)?;

    if input::has_any(
        headers,
        &[
            "x-amz-request-payer",
            "x-amz-server-side-encryption-customer-algorithm",
            "x-amz-server-side-encryption-customer-key",
            "x-amz-server-side-encryption-customer-key-md5",
            "x-amz-copy-source-server-side-encryption-customer-algorithm",
            "x-amz-copy-source-server-side-encryption-customer-key",
            "x-amz-copy-source-server-side-encryption-customer-key-md5",
            "x-amz-tagging",
            "x-amz-if-match-initiated-time",
        ],
    ) {
        return Err(s3_error!(NotImplemented));
    }

    Ok(())
}

/// Content-MD5 describes the request envelope. The operation's flexible
/// checksum headers select or assert the stored object, never the XML bytes.
fn md5<T>(request: &S3Request<T>, bytes: &[u8]) -> S3Result<()> {
    let integrity = PutIntegrity::new(PutIntegrityInput {
        content_md5: input::header(&request.headers, "content-md5")?,
        ..PutIntegrityInput::default()
    })
    .map_err(error::integrity_error)?;

    let mut verifier = integrity.verifier(None).map_err(error::integrity_error)?;
    verifier.update(bytes).map_err(error::integrity_error)?;

    verifier
        .finish(ExpectedDigests::default())
        .map_err(error::integrity_error)?;

    Ok(())
}

pub(super) async fn create(
    storage: &Storage,
    request: S3Request<dto::CreateMultipartUploadInput>,
) -> S3Result<S3Response<dto::CreateMultipartUploadOutput>> {
    let identity = identity(&request)?;
    let headers = &request.headers;
    input::write_options(headers)?;
    md5(&request, &[])?;

    if headers.keys().any(|name| {
        if !name.as_str().starts_with("x-amz-checksum-") {
            return false;
        }

        !matches!(
            name.as_str(),
            "x-amz-checksum-algorithm" | "x-amz-checksum-type"
        )
    }) {
        return Err(s3_error!(NotImplemented));
    }

    if input::has_any(
        headers,
        &[
            "x-amz-sdk-checksum-algorithm",
            "x-amz-trailer",
            "if-match",
            "if-none-match",
        ],
    ) {
        return Err(s3_error!(NotImplemented));
    }

    let checksum = MultipartChecksum::resolve(algorithm(headers)?, checksum_type(headers)?)
        .map_err(error::integrity_error)?;

    let input = &request.input;

    let operation = mikrio_core::multipart::CreateRequest {
        bucket: BucketName::parse(&input.bucket).map_err(|_| s3_error!(InvalidBucketName))?,
        key: ObjectKey::parse(&input.key).map_err(|_| s3_error!(InvalidArgument))?,
        expected_owner: crate::access::expected_owner(headers)?.map(str::to_owned),
        representation: input::representation(headers)?,
        metadata: UserMetadata::new(
            input
                .metadata
                .iter()
                .flat_map(|values| values.iter())
                .map(|(key, value)| (key.clone(), value.clone())),
        )
        .map_err(|_| s3_error!(InvalidArgument))?,
        tags: input::header(headers, "x-amz-tagging")?
            .as_deref()
            .map(super::tagging::inline)
            .transpose()?,
        checksum,
    };

    let created = storage
        .0
        .create_multipart_upload_http(identity, operation)
        .await
        .map_err(|error| error::operation(&error))?;

    Ok(S3Response::new(dto::CreateMultipartUploadOutput {
        bucket: Some(input.bucket.clone()),
        key: Some(input.key.clone()),
        upload_id: Some(created.upload_id.to_string()),
        checksum_algorithm: Some(dto::ChecksumAlgorithm::from_static(
            created.checksum.algorithm().as_str(),
        )),
        checksum_type: Some(wire_type(created.checksum.checksum_type())),
        ..dto::CreateMultipartUploadOutput::default()
    }))
}

pub(super) async fn part(
    storage: &Storage,
    mut request: S3Request<dto::UploadPartInput>,
) -> S3Result<S3Response<dto::UploadPartOutput>> {
    let identity = identity(&request)?;
    active_options(&request.headers)?;

    if input::has_any(
        &request.headers,
        &[
            "if-match",
            "if-none-match",
            "x-amz-checksum-type",
            "x-amz-copy-source-range",
            "x-amz-metadata-directive",
            "x-amz-tagging-directive",
        ],
    ) {
        return Err(s3_error!(NotImplemented));
    }

    let integrity = Integrity::from_request(&request, request.input.content_length)?;
    let input = &request.input;

    let upload = upload(
        &input.bucket,
        &input.key,
        &input.upload_id,
        &request.headers,
    )?;

    let operation = mikrio_core::multipart::PartRequest {
        bucket: upload.bucket,
        key: upload.key,
        upload_id: upload.upload_id,
        expected_owner: upload.expected_owner,
        part_number: number(input.part_number)?,
        content_length: integrity.length,
        checksum_algorithm: integrity.input.fields().checksum_algorithm,
        expected: integrity.input.expected(),
    };

    let body = request
        .input
        .body
        .take()
        .ok_or_else(|| s3_error!(InvalidRequest))?;

    let facts = storage
        .0
        .upload_part_http(identity, operation, body::Upload::new(body), move || {
            integrity.finish()
        })
        .await
        .map_err(|error| error::operation(&error))?;

    let facts = output::put(&facts)?;

    Ok(S3Response::new(dto::UploadPartOutput {
        e_tag: facts.e_tag,
        checksum_crc32: facts.checksum_crc32,
        checksum_crc32c: facts.checksum_crc32c,
        checksum_crc64nvme: facts.checksum_crc64nvme,
        checksum_sha1: facts.checksum_sha1,
        checksum_sha256: facts.checksum_sha256,
        ..dto::UploadPartOutput::default()
    }))
}

pub(super) async fn copy(
    storage: &Storage,
    request: S3Request<dto::UploadPartCopyInput>,
) -> S3Result<S3Response<dto::UploadPartCopyOutput>> {
    let identity = identity(&request)?;
    active_options(&request.headers)?;

    if input::has_any(
        &request.headers,
        &[
            "if-match",
            "if-none-match",
            "x-amz-checksum-type",
            "x-amz-checksum-algorithm",
            "x-amz-sdk-checksum-algorithm",
            "x-amz-metadata-directive",
            "x-amz-tagging-directive",
        ],
    ) {
        return Err(s3_error!(NotImplemented));
    }

    if request
        .headers
        .keys()
        .any(|name| name.as_str().starts_with("x-amz-checksum-"))
    {
        return Err(s3_error!(NotImplemented));
    }

    Integrity::bodyless(&request)?;
    let input = &request.input;

    let upload = upload(
        &input.bucket,
        &input.key,
        &input.upload_id,
        &request.headers,
    )?;

    let operation = mikrio_core::multipart::CopyPartRequest {
        bucket: upload.bucket,
        key: upload.key,
        upload_id: upload.upload_id,
        expected_owner: upload.expected_owner,
        part_number: number(input.part_number)?,
        source: object_copy::source(&input.copy_source)?,
        expected_source_owner: object_copy::source_owner(&request.headers)?,
        source_conditions: object_copy::source_conditions(&request.headers)?,
        source_range: input::header(&request.headers, "x-amz-copy-source-range")?
            .as_deref()
            .map(mikrio_core::multipart::CopyRange::parse)
            .transpose()
            .map_err(|_| s3_error!(InvalidArgument))?,
    };

    let connection = storage.0.clone();
    let progress = mikrio_core::WorkProgress::default();
    let worker_progress = progress.clone();

    let work = async move {
        let result = connection
            .upload_part_copy_http_with_progress(identity, operation, worker_progress)
            .await
            .map_err(|error| error::operation(&error))?;

        let facts = output::put(&result.facts)?;

        Ok(dto::UploadPartCopyOutput {
            copy_part_result: Some(dto::CopyPartResult {
                e_tag: facts.e_tag,
                last_modified: Some(output::timestamp(result.last_modified)?),
                checksum_crc32: facts.checksum_crc32,
                checksum_crc32c: facts.checksum_crc32c,
                checksum_crc64nvme: facts.checksum_crc64nvme,
                checksum_sha1: facts.checksum_sha1,
                checksum_sha256: facts.checksum_sha256,
                ..dto::CopyPartResult::default()
            }),
            ..dto::UploadPartCopyOutput::default()
        })
    };

    let output = match super::progress::admit(work, &progress).await? {
        super::progress::Admitted::Complete(output) => output,
        super::progress::Admitted::Running(future) => dto::UploadPartCopyOutput {
            future: Some(future),
            ..dto::UploadPartCopyOutput::default()
        },
    };

    Ok(S3Response::new(output))
}

pub(super) async fn abort(
    storage: &Storage,
    request: S3Request<dto::AbortMultipartUploadInput>,
) -> S3Result<S3Response<dto::AbortMultipartUploadOutput>> {
    let identity = identity(&request)?;
    active_options(&request.headers)?;
    Integrity::bodyless(&request)?;
    let input = &request.input;

    storage
        .0
        .abort_multipart_upload_http(
            identity,
            upload(
                &input.bucket,
                &input.key,
                &input.upload_id,
                &request.headers,
            )?,
        )
        .await
        .map_err(|error| error::operation(&error))?;

    Ok(S3Response::new(dto::AbortMultipartUploadOutput::default()))
}
