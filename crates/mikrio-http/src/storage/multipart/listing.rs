//! Multipart part and upload listing projections.

use s3s::s3_error;

use super::{
    BucketName, ChecksumAlgorithm, Integrity, S3Request, S3Response, S3Result, Storage,
    active_options, dto, error, identity, output, upload, wire_type,
};

pub(in crate::storage) async fn parts(
    storage: &Storage,
    request: S3Request<dto::ListPartsInput>,
) -> S3Result<S3Response<dto::ListPartsOutput>> {
    let identity = identity(&request)?;
    active_options(&request.headers)?;
    Integrity::bodyless(&request)?;
    let input = request.input;

    let marker = u32::try_from(input.part_number_marker.unwrap_or(0))
        .map_err(|_| s3_error!(InvalidArgument))?;

    let limit =
        u32::try_from(input.max_parts.unwrap_or(1000)).map_err(|_| s3_error!(InvalidArgument))?;

    let limit =
        mikrio_core::multipart::PartPageSize::new(limit).map_err(|_| s3_error!(InvalidArgument))?;

    let page = storage
        .0
        .list_parts_http(
            identity,
            mikrio_core::multipart::ListPartsRequest {
                upload: upload(
                    &input.bucket,
                    &input.key,
                    &input.upload_id,
                    &request.headers,
                )?,
                part_number_marker: marker,
                max_parts: limit,
            },
        )
        .await
        .map_err(|error| error::operation(&error))?;

    let parts = page
        .parts
        .into_iter()
        .map(|part| {
            let mut output = dto::Part {
                part_number: Some(
                    i32::try_from(part.part_number.get()).map_err(|_| s3_error!(InternalError))?,
                ),
                e_tag: Some(part.etag.parse().map_err(|_| s3_error!(InternalError))?),
                size: Some(i64::try_from(part.size.get()).map_err(|_| s3_error!(InternalError))?),
                last_modified: Some(output::timestamp(part.last_modified)?),
                ..dto::Part::default()
            };

            let checksum = Some(part.checksum.encode());

            match part.checksum.algorithm() {
                ChecksumAlgorithm::Crc32 => output.checksum_crc32 = checksum,
                ChecksumAlgorithm::Crc32c => output.checksum_crc32c = checksum,
                ChecksumAlgorithm::Crc64Nvme => output.checksum_crc64nvme = checksum,
                ChecksumAlgorithm::Sha1 => output.checksum_sha1 = checksum,
                ChecksumAlgorithm::Sha256 => output.checksum_sha256 = checksum,
            }

            Ok(output)
        })
        .collect::<S3Result<Vec<_>>>()?;

    Ok(S3Response::new(dto::ListPartsOutput {
        bucket: Some(input.bucket),
        key: Some(input.key),
        upload_id: Some(input.upload_id),
        part_number_marker: Some(i32::try_from(marker).map_err(|_| s3_error!(InternalError))?),
        max_parts: Some(i32::try_from(limit.get()).map_err(|_| s3_error!(InternalError))?),
        is_truncated: Some(page.next_part_number_marker.is_some()),
        next_part_number_marker: page
            .next_part_number_marker
            .map(i32::try_from)
            .transpose()
            .map_err(|_| s3_error!(InternalError))?,
        checksum_algorithm: Some(dto::ChecksumAlgorithm::from_static(
            page.checksum.algorithm().as_str(),
        )),
        checksum_type: Some(wire_type(page.checksum.checksum_type())),
        initiator: Some(dto::Initiator {
            id: Some(page.initiator),
            ..dto::Initiator::default()
        }),
        owner: Some(dto::Owner {
            id: Some(page.owner.canonical_id().into()),
            ..dto::Owner::default()
        }),
        storage_class: Some(dto::StorageClass::from_static("STANDARD")),
        parts: Some(parts),
        ..dto::ListPartsOutput::default()
    }))
}

pub(in crate::storage) async fn uploads(
    storage: &Storage,
    request: S3Request<dto::ListMultipartUploadsInput>,
) -> S3Result<S3Response<dto::ListMultipartUploadsOutput>> {
    let identity = identity(&request)?;
    active_options(&request.headers)?;
    Integrity::bodyless(&request)?;
    let input = request.input;

    let encoded = match input.encoding_type.as_ref().map(dto::EncodingType::as_str) {
        None => false,
        Some("url") => true,
        Some(_) => return Err(s3_error!(InvalidArgument)),
    };

    let limit =
        u32::try_from(input.max_uploads.unwrap_or(1000)).map_err(|_| s3_error!(InvalidArgument))?;

    let limit = mikrio_core::multipart::UploadPageSize::new(limit)
        .map_err(|_| s3_error!(InvalidArgument))?;

    let page = storage
        .0
        .list_multipart_uploads_http(
            identity,
            mikrio_core::multipart::ListUploadsRequest {
                bucket: BucketName::parse(&input.bucket)
                    .map_err(|_| s3_error!(InvalidBucketName))?,
                expected_owner: crate::access::expected_owner(&request.headers)?.map(str::to_owned),
                prefix: input.prefix.clone(),
                delimiter: input.delimiter.clone(),
                key_marker: input.key_marker.clone(),
                upload_id_marker: input.upload_id_marker.clone(),
                max_uploads: limit,
            },
        )
        .await
        .map_err(|error| error::operation(&error))?;

    let encode = |value: String| {
        if encoded {
            urlencoding::encode(&value).into_owned()
        } else {
            value
        }
    };

    let owner = dto::Owner {
        id: Some(page.owner.canonical_id().into()),
        ..dto::Owner::default()
    };

    let uploads = page
        .uploads
        .into_iter()
        .map(|upload| {
            Ok(dto::MultipartUpload {
                key: Some(encode(upload.key)),
                initiated: Some(output::timestamp(upload.upload_id.initiated_at())?),
                upload_id: Some(upload.upload_id.to_string()),
                checksum_algorithm: Some(dto::ChecksumAlgorithm::from_static(
                    upload.checksum.algorithm().as_str(),
                )),
                checksum_type: Some(wire_type(upload.checksum.checksum_type())),
                owner: Some(owner.clone()),
                initiator: Some(dto::Initiator {
                    id: Some(upload.initiator),
                    ..dto::Initiator::default()
                }),
                storage_class: Some(dto::StorageClass::from_static("STANDARD")),
            })
        })
        .collect::<S3Result<Vec<_>>>()?;

    let is_truncated = page.next_marker.is_some();

    let (next_key, next_upload_id) = page.next_marker.map_or((None, None), |marker| {
        (Some(encode(marker.key)), marker.upload_id)
    });

    Ok(S3Response::new(dto::ListMultipartUploadsOutput {
        bucket: Some(input.bucket),
        prefix: Some(encode(input.prefix.unwrap_or_default())),
        delimiter: input.delimiter.map(encode),
        key_marker: Some(encode(input.key_marker.unwrap_or_default())),
        upload_id_marker: input.upload_id_marker,
        max_uploads: Some(i32::try_from(limit.get()).map_err(|_| s3_error!(InternalError))?),
        encoding_type: input.encoding_type,
        next_key_marker: next_key,
        next_upload_id_marker: next_upload_id,
        is_truncated: Some(is_truncated),
        uploads: Some(uploads),
        common_prefixes: Some(
            page.common_prefixes
                .into_iter()
                .map(|prefix| dto::CommonPrefix {
                    prefix: Some(encode(prefix)),
                })
                .collect(),
        ),
        ..dto::ListMultipartUploadsOutput::default()
    }))
}
