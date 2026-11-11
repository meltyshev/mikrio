//! Attribute groups projected from one authorized metadata snapshot.

use mikrio_core::object::{
    Attribute, AttributeSelection, AttributesRequest, BucketName, ObjectKey,
};
use s3s::{S3Request, S3Response, S3Result, dto, s3_error};

use super::{Storage, error, identity, integrity::Integrity, output};

pub(super) async fn get(
    storage: &Storage,
    request: S3Request<dto::GetObjectAttributesInput>,
) -> S3Result<S3Response<dto::GetObjectAttributesOutput>> {
    Integrity::bodyless(&request)?;
    let input = &request.input;

    if input.version_id.is_some() {
        return Err(s3_error!(NotImplemented));
    }

    if [
        "if-match",
        "if-none-match",
        "if-modified-since",
        "if-unmodified-since",
        "range",
        "x-amz-request-payer",
        "x-amz-server-side-encryption-customer-algorithm",
        "x-amz-server-side-encryption-customer-key",
        "x-amz-server-side-encryption-customer-key-md5",
    ]
    .iter()
    .any(|name| request.headers.contains_key(*name))
    {
        return Err(s3_error!(NotImplemented));
    }

    if input.object_attributes.len() > 5 {
        return Err(s3_error!(InvalidArgument));
    }

    let attributes = input
        .object_attributes
        .iter()
        .map(|value| match value.as_str() {
            "ETag" => Ok(Attribute::Etag),
            "Checksum" => Ok(Attribute::Checksum),
            "ObjectParts" => Ok(Attribute::ObjectParts),
            "StorageClass" => Ok(Attribute::StorageClass),
            "ObjectSize" => Ok(Attribute::ObjectSize),
            _ => Err(s3_error!(InvalidArgument)),
        })
        .collect::<S3Result<Vec<_>>>()?;

    let max_parts =
        u32::try_from(input.max_parts.unwrap_or(1000)).map_err(|_| s3_error!(InvalidArgument))?;

    let marker = u32::try_from(input.part_number_marker.unwrap_or(0))
        .map_err(|_| s3_error!(InvalidArgument))?;

    let selected = storage
        .0
        .get_object_attributes_http(
            identity(&request)?,
            AttributesRequest {
                bucket: BucketName::parse(&input.bucket)
                    .map_err(|_| s3_error!(InvalidBucketName))?,
                key: ObjectKey::parse(&input.key).map_err(|_| s3_error!(InvalidArgument))?,
                expected_owner: crate::access::expected_owner(&request.headers)?.map(str::to_owned),
                attributes: AttributeSelection::new(attributes)
                    .map_err(|cause| error::operation(&cause))?,
                max_parts: mikrio_core::multipart::PartPageSize::new(max_parts)
                    .map_err(|_| s3_error!(InvalidArgument))?,
                part_number_marker: marker,
            },
        )
        .await
        .map_err(|cause| error::operation(&cause))?;

    Ok(S3Response::new(dto::GetObjectAttributesOutput {
        object_size: selected
            .object_size
            .map(mikrio_core::object::ByteLength::as_stored),
        e_tag: selected
            .etag
            .map(|etag| etag.parse().map_err(|_| s3_error!(InternalError)))
            .transpose()?,
        checksum: selected.checksum.map(|(mode, mut values)| dto::Checksum {
            checksum_crc32: values.remove("CRC32"),
            checksum_crc32c: values.remove("CRC32C"),
            checksum_sha1: values.remove("SHA1"),
            checksum_sha256: values.remove("SHA256"),
            checksum_crc64nvme: values.remove("CRC64NVME"),
            checksum_type: Some(dto::ChecksumType::from_static(match mode {
                mikrio_core::integrity::ChecksumType::FullObject => "FULL_OBJECT",
                mikrio_core::integrity::ChecksumType::Composite => "COMPOSITE",
            })),
            ..dto::Checksum::default()
        }),
        storage_class: selected.storage_class.map(dto::StorageClass::from_static),
        object_parts: selected
            .object_parts
            .map(|page| parts(page, max_parts, marker))
            .transpose()?,
        last_modified: Some(output::timestamp(selected.last_modified)?),
        ..dto::GetObjectAttributesOutput::default()
    }))
}

fn integer(value: u32) -> S3Result<i32> {
    i32::try_from(value).map_err(|_| s3_error!(InternalError))
}

fn parts(
    page: mikrio_core::object::StoredPartPage,
    limit: u32,
    marker: u32,
) -> S3Result<dto::GetObjectAttributesParts> {
    let mut output = dto::GetObjectAttributesParts {
        total_parts_count: Some(integer(page.total_parts_count)?),
        ..dto::GetObjectAttributesParts::default()
    };

    if !page.entries_available {
        return Ok(output);
    }

    output.is_truncated = Some(page.next_part_number_marker.is_some());
    output.max_parts = Some(integer(limit)?);
    output.part_number_marker = Some(integer(marker)?);

    output.next_part_number_marker = page
        .parts
        .last()
        .map(|part| integer(part.part_number))
        .transpose()?;

    output.parts = Some(
        page.parts
            .into_iter()
            .map(|part| {
                let mut output = dto::ObjectPart {
                    part_number: Some(integer(part.part_number)?),
                    size: Some(
                        i64::try_from(part.byte_length).map_err(|_| s3_error!(InternalError))?,
                    ),
                    ..dto::ObjectPart::default()
                };

                for checksum in part.checksums {
                    let encoded = Some(checksum.encode());

                    match checksum.algorithm() {
                        mikrio_core::integrity::ChecksumAlgorithm::Crc32 => {
                            output.checksum_crc32 = encoded;
                        }
                        mikrio_core::integrity::ChecksumAlgorithm::Crc32c => {
                            output.checksum_crc32c = encoded;
                        }
                        mikrio_core::integrity::ChecksumAlgorithm::Crc64Nvme => {
                            output.checksum_crc64nvme = encoded;
                        }
                        mikrio_core::integrity::ChecksumAlgorithm::Sha1 => {
                            output.checksum_sha1 = encoded;
                        }
                        mikrio_core::integrity::ChecksumAlgorithm::Sha256 => {
                            output.checksum_sha256 = encoded;
                        }
                    }
                }

                Ok(output)
            })
            .collect::<S3Result<Vec<_>>>()?,
    );

    Ok(output)
}
