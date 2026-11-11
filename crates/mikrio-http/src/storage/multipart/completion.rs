//! Completion verifies the XML envelope separately from object checksum assertions.

use mikrio_core::integrity::{Checksum, ObjectChecksum};
use mikrio_core::multipart::{CompleteRequest, CompletionParts, SelectedPart};
use mikrio_core::object::{ByteLength, WriteConditions};
use s3s::s3_error;

use super::{
    ChecksumAlgorithm, ChecksumType, S3Request, S3Response, S3Result, Storage, active_options,
    checksum_type, dto, error, identity, input, md5, number, upload, wire_type,
};

pub(in crate::storage) async fn complete(
    storage: &Storage,
    request: S3Request<dto::CompleteMultipartUploadInput>,
) -> S3Result<S3Response<dto::CompleteMultipartUploadOutput>> {
    let identity = identity(&request)?;
    active_options(&request.headers)?;

    let bytes = &request
        .extensions
        .get::<s3s::service::RequestBodyBytes>()
        .ok_or_else(|| s3_error!(InternalError))?
        .0;

    md5(&request, bytes)?;

    if request.headers.keys().any(|name| {
        if !name.as_str().starts_with("x-amz-checksum-") {
            return false;
        }

        !matches!(
            name.as_str(),
            "x-amz-checksum-type"
                | "x-amz-checksum-crc32"
                | "x-amz-checksum-crc32c"
                | "x-amz-checksum-crc64nvme"
                | "x-amz-checksum-sha1"
                | "x-amz-checksum-sha256"
        )
    }) {
        return Err(s3_error!(NotImplemented));
    }

    if input::has_any(
        &request.headers,
        &["x-amz-sdk-checksum-algorithm", "x-amz-trailer"],
    ) {
        return Err(s3_error!(NotImplemented));
    }

    let checksum_type = checksum_type(&request.headers)?;
    let mut checksum = None;

    for (algorithm, name) in [
        (ChecksumAlgorithm::Crc32, "x-amz-checksum-crc32"),
        (ChecksumAlgorithm::Crc32c, "x-amz-checksum-crc32c"),
        (ChecksumAlgorithm::Crc64Nvme, "x-amz-checksum-crc64nvme"),
        (ChecksumAlgorithm::Sha1, "x-amz-checksum-sha1"),
        (ChecksumAlgorithm::Sha256, "x-amz-checksum-sha256"),
    ] {
        let Some(value) = input::header(&request.headers, name)? else {
            continue;
        };

        if checksum.is_some() {
            return Err(s3_error!(InvalidRequest));
        }

        let mode = checksum_type.unwrap_or(if value.contains('-') {
            ChecksumType::Composite
        } else {
            ChecksumType::FullObject
        });

        checksum =
            Some(ObjectChecksum::parse(algorithm, mode, &value).map_err(error::integrity_error)?);
    }

    let input = request.input;

    let parts = input
        .multipart_upload
        .and_then(|value| value.parts)
        .ok_or_else(|| s3_error!(MalformedXML))?;

    // Check the bound before allocating a second representation of the XML list.
    if parts.is_empty() || parts.len() > 10_000 {
        return Err(s3_error!(InvalidRequest));
    }

    let parts = parts
        .into_iter()
        .map(selected)
        .collect::<S3Result<Vec<_>>>()?;

    let operation = CompleteRequest {
        upload: upload(
            &input.bucket,
            &input.key,
            &input.upload_id,
            &request.headers,
        )?,
        parts: CompletionParts::new(parts).map_err(|error| error::operation(&error))?,
        conditions: WriteConditions::new(
            input::header(&request.headers, "if-match")?,
            input::header(&request.headers, "if-none-match")?,
        )
        .map_err(|_| s3_error!(InvalidArgument))?,
        content_length: input::header(&request.headers, "x-amz-mp-object-size")?
            .map(|value| value.parse::<u64>().map_err(|_| s3_error!(InvalidArgument)))
            .transpose()?
            .map(ByteLength::new)
            .transpose()
            .map_err(|_| s3_error!(InvalidArgument))?,
        checksum,
        checksum_type,
    };

    let origin = request
        .extensions
        .get::<s3s::service::PostObjectOrigin>()
        .ok_or_else(|| s3_error!(InternalError))?;

    let location = format!(
        "{}/{}/{}",
        origin.0,
        urlencoding::encode(&input.bucket),
        urlencoding::encode(&input.key)
    );

    let connection = storage.0.clone();
    let progress = mikrio_core::WorkProgress::default();
    let worker_progress = progress.clone();

    let work = async move {
        let result = connection
            .complete_multipart_upload_http_with_progress(identity, operation, worker_progress)
            .await
            .map_err(|error| error::operation(&error))?;

        let mut output = dto::CompleteMultipartUploadOutput {
            bucket: Some(input.bucket),
            key: Some(input.key),
            location: Some(location),
            e_tag: Some(result.etag.parse().map_err(|_| s3_error!(InternalError))?),
            checksum_type: Some(wire_type(result.checksum.checksum_type())),
            ..dto::CompleteMultipartUploadOutput::default()
        };

        let checksum = Some(result.checksum.encode());

        match result.checksum.digest().algorithm() {
            ChecksumAlgorithm::Crc32 => output.checksum_crc32 = checksum,
            ChecksumAlgorithm::Crc32c => output.checksum_crc32c = checksum,
            ChecksumAlgorithm::Crc64Nvme => output.checksum_crc64nvme = checksum,
            ChecksumAlgorithm::Sha1 => output.checksum_sha1 = checksum,
            ChecksumAlgorithm::Sha256 => output.checksum_sha256 = checksum,
        }

        Ok(output)
    };

    let output = match super::super::progress::admit(work, &progress).await? {
        super::super::progress::Admitted::Complete(output) => output,
        super::super::progress::Admitted::Running(future) => dto::CompleteMultipartUploadOutput {
            future: Some(future),
            ..dto::CompleteMultipartUploadOutput::default()
        },
    };

    Ok(S3Response::new(output))
}

fn selected(part: dto::CompletedPart) -> S3Result<SelectedPart> {
    if part.checksum_md5.is_some()
        || part.checksum_sha512.is_some()
        || part.checksum_xxhash128.is_some()
        || part.checksum_xxhash3.is_some()
        || part.checksum_xxhash64.is_some()
    {
        return Err(s3_error!(NotImplemented));
    }

    let mut checksum = None;

    for (algorithm, value) in [
        (ChecksumAlgorithm::Crc32, part.checksum_crc32),
        (ChecksumAlgorithm::Crc32c, part.checksum_crc32c),
        (ChecksumAlgorithm::Crc64Nvme, part.checksum_crc64nvme),
        (ChecksumAlgorithm::Sha1, part.checksum_sha1),
        (ChecksumAlgorithm::Sha256, part.checksum_sha256),
    ] {
        let Some(value) = value else { continue };

        if checksum.is_some() {
            return Err(s3_error!(InvalidRequest));
        }

        checksum = Some(Checksum::parse(algorithm, &value).map_err(error::integrity_error)?);
    }

    Ok(SelectedPart {
        part_number: number(part.part_number.ok_or_else(|| s3_error!(MalformedXML))?)?,
        etag: format!(
            "\"{}\"",
            part.e_tag
                .ok_or_else(|| s3_error!(MalformedXML))?
                .into_strong()
                .ok_or_else(|| s3_error!(InvalidPart))?
        ),
        checksum,
    })
}
