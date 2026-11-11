//! Explicitly reject options outside the supported private object routes.

use mikrio_core::EpochMillis;
use mikrio_core::integrity::PutIntegrity;
use mikrio_core::object::{
    BucketName, ByteLength, ByteRange, ObjectKey, PutRequest, ReadConditions, ReadConditionsInput,
    ReadRequest, Representation, RepresentationInput, UserMetadata, WriteConditions,
};
use s3s::dto::{DeleteObjectInput, GetObjectInput, HeadObjectInput, PutObjectInput, Timestamp};
use s3s::{S3Request, S3Result, s3_error};

pub(super) fn header(headers: &http::HeaderMap, name: &str) -> S3Result<Option<String>> {
    headers
        .get(name)
        .map(|value| {
            std::str::from_utf8(value.as_bytes())
                .map(str::to_owned)
                .map_err(|_| s3_error!(InvalidArgument))
        })
        .transpose()
}

pub(super) fn instant(value: Option<Timestamp>) -> S3Result<Option<EpochMillis>> {
    value
        .map(|value| {
            let instant = time::OffsetDateTime::from(value);

            let milliseconds = i64::try_from(instant.unix_timestamp_nanos().div_euclid(1_000_000))
                .map_err(|_| s3_error!(InvalidArgument))?;

            EpochMillis::new(milliseconds).map_err(|_| s3_error!(InvalidArgument))
        })
        .transpose()
}

pub(super) fn has_any(headers: &http::HeaderMap, names: &[&str]) -> bool {
    names.iter().any(|name| headers.contains_key(*name))
}

pub(super) fn checksums(headers: &http::HeaderMap) -> S3Result<bool> {
    match header(headers, "x-amz-checksum-mode")?.as_deref() {
        None => Ok(false),
        Some("ENABLED") => Ok(true),
        Some(_) => Err(s3_error!(InvalidArgument)),
    }
}

pub(super) fn representation(headers: &http::HeaderMap) -> S3Result<Representation> {
    Representation::new(RepresentationInput {
        content_type: header(headers, "content-type")?,
        content_encoding: header(headers, "content-encoding")?,
        content_disposition: header(headers, "content-disposition")?,
        content_language: header(headers, "content-language")?,
        cache_control: header(headers, "cache-control")?,
        expires: crate::access::parse_expires(header(headers, "expires")?.as_deref())?,
    })
    .map_err(|_| s3_error!(InvalidArgument))
}

pub(super) fn put(
    request: &S3Request<PutObjectInput>,
    integrity: PutIntegrity,
    length: Option<ByteLength>,
) -> S3Result<PutRequest> {
    let input = &request.input;
    let headers = &request.headers;
    write_options(headers)?;

    Ok(PutRequest {
        bucket: BucketName::parse(&input.bucket).map_err(|_| s3_error!(InvalidBucketName))?,
        key: ObjectKey::parse(&input.key).map_err(|_| s3_error!(InvalidArgument))?,
        expected_owner: crate::access::expected_owner(headers)?.map(str::to_owned),
        content_length: length,
        representation: Representation::new(RepresentationInput {
            content_type: header(headers, "content-type")?,
            content_encoding: encoding(
                header(headers, "content-encoding")?.as_deref(),
                request.trailing_headers.is_some(),
            ),
            content_disposition: header(headers, "content-disposition")?,
            content_language: header(headers, "content-language")?,
            cache_control: header(headers, "cache-control")?,
            expires: crate::access::parse_expires(header(headers, "expires")?.as_deref())?,
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
        tags: header(headers, "x-amz-tagging")?
            .as_deref()
            .map(super::tagging::inline)
            .transpose()?,
        integrity,
        conditions: WriteConditions::new(
            header(&request.headers, "if-match")?,
            header(&request.headers, "if-none-match")?,
        )
        .map_err(|_| s3_error!(InvalidArgument))?,
    })
}

pub(super) fn write_options(headers: &http::HeaderMap) -> S3Result<()> {
    // Optional codec fields collapse empty strings to absence. Use raw header
    // presence for rejection, and raw values for the shared representation.
    if header(headers, "x-amz-acl")?
        .as_deref()
        .is_some_and(|acl| acl != "bucket-owner-full-control")
    {
        return Err(s3_error!(AccessControlListNotSupported));
    }

    if has_any(
        headers,
        &[
            "x-amz-grant-full-control",
            "x-amz-grant-read",
            "x-amz-grant-read-acp",
            "x-amz-grant-write-acp",
        ],
    ) {
        return Err(s3_error!(AccessControlListNotSupported));
    }

    let storage_class = header(headers, "x-amz-storage-class")?;

    if storage_class.as_deref() == Some("") {
        return Err(s3_error!(InvalidArgument));
    }

    if has_any(
        headers,
        &[
            "x-amz-server-side-encryption-bucket-key-enabled",
            "x-amz-object-lock-legal-hold",
            "x-amz-object-lock-mode",
            "x-amz-object-lock-retain-until-date",
            "x-amz-request-payer",
            "x-amz-server-side-encryption-customer-algorithm",
            "x-amz-server-side-encryption-customer-key",
            "x-amz-server-side-encryption-customer-key-md5",
            "x-amz-server-side-encryption-context",
            "x-amz-server-side-encryption-aws-kms-key-id",
            "x-amz-server-side-encryption",
            "x-amz-website-redirect-location",
            "x-amz-write-offset-bytes",
        ],
    ) {
        return Err(s3_error!(NotImplemented));
    }

    if storage_class
        .as_deref()
        .is_some_and(|value| value != "STANDARD")
    {
        return Err(s3_error!(NotImplemented));
    }

    Ok(())
}

pub(super) fn read(request: &S3Request<GetObjectInput>) -> S3Result<ReadRequest> {
    let input = &request.input;
    let headers = &request.headers;

    if has_any(
        headers,
        &[
            "x-amz-request-payer",
            "x-amz-server-side-encryption-customer-algorithm",
            "x-amz-server-side-encryption-customer-key",
            "x-amz-server-side-encryption-customer-key-md5",
        ],
    ) {
        return Err(s3_error!(NotImplemented));
    }

    if input.version_id.is_some() {
        return Err(s3_error!(NotImplemented));
    }

    for name in ["if-modified-since", "if-unmodified-since"] {
        if headers.get(name).is_some_and(http::HeaderValue::is_empty) {
            return Err(s3_error!(InvalidArgument));
        }
    }

    Ok(ReadRequest {
        bucket: BucketName::parse(&input.bucket).map_err(|_| s3_error!(InvalidBucketName))?,
        key: ObjectKey::parse(&input.key).map_err(|_| s3_error!(InvalidArgument))?,
        expected_owner: input.expected_bucket_owner.clone(),
        range: header(headers, "range")?
            .as_deref()
            .map(ByteRange::parse)
            .transpose()
            .map_err(|_| s3_error!(InvalidArgument))?,
        part_number: input
            .part_number
            .map(|value| {
                let number = u32::try_from(value).map_err(|_| s3_error!(InvalidArgument))?;

                mikrio_core::multipart::PartNumber::new(number)
                    .map_err(|_| s3_error!(InvalidArgument))
            })
            .transpose()?,
        response: Representation::new(RepresentationInput {
            content_type: input.response_content_type.clone(),
            content_encoding: input.response_content_encoding.clone(),
            content_disposition: input.response_content_disposition.clone(),
            content_language: input.response_content_language.clone(),
            cache_control: input.response_cache_control.clone(),
            expires: instant(input.response_expires.clone())?,
        })
        .map_err(|_| s3_error!(InvalidArgument))?,
        conditions: ReadConditions::new(ReadConditionsInput {
            if_match: header(&request.headers, "if-match")?,
            if_none_match: header(&request.headers, "if-none-match")?,
            if_modified_since: instant(input.if_modified_since.clone())?,
            if_unmodified_since: instant(input.if_unmodified_since.clone())?,
        })
        .map_err(|_| s3_error!(InvalidArgument))?,
    })
}

pub(super) fn delete(
    request: &S3Request<DeleteObjectInput>,
) -> S3Result<(ReadRequest, Option<String>)> {
    let input = &request.input;

    if has_any(
        &request.headers,
        &[
            "x-amz-bypass-governance-retention",
            "x-amz-if-match-last-modified-time",
            "x-amz-if-match-size",
            "x-amz-mfa",
            "x-amz-request-payer",
        ],
    ) {
        return Err(s3_error!(NotImplemented));
    }

    if input.version_id.is_some() {
        return Err(s3_error!(NotImplemented));
    }

    if request
        .headers
        .keys()
        .any(|name| name.as_str().starts_with("x-amz-checksum-"))
    {
        return Err(s3_error!(NotImplemented));
    }

    if request.headers.contains_key("x-amz-sdk-checksum-algorithm")
        || request.headers.contains_key("x-amz-trailer")
        || request.headers.contains_key("if-none-match")
    {
        return Err(s3_error!(NotImplemented));
    }

    if let Some(md5) = header(&request.headers, "content-md5")? {
        // The protocol barrier has established empty decoded EOF. Validate an
        // explicitly supplied digest before asking SQL to apply the mutation.
        let integrity = PutIntegrity::new(mikrio_core::integrity::PutIntegrityInput {
            content_md5: Some(md5),
            ..mikrio_core::integrity::PutIntegrityInput::default()
        })
        .map_err(super::error::integrity_error)?;

        integrity
            .verifier(Some(0))
            .and_then(|verifier| {
                verifier.finish(mikrio_core::integrity::ExpectedDigests::default())
            })
            .map_err(super::error::integrity_error)?;
    }

    let condition = header(&request.headers, "if-match")?;

    if let Some(condition) = &condition {
        mikrio_core::object::validate_entity_tag_condition(condition)
            .map_err(|_| s3_error!(InvalidArgument))?;
    }

    Ok((
        ReadRequest {
            bucket: BucketName::parse(&input.bucket).map_err(|_| s3_error!(InvalidBucketName))?,
            key: ObjectKey::parse(&input.key).map_err(|_| s3_error!(InvalidArgument))?,
            expected_owner: input.expected_bucket_owner.clone(),
            conditions: ReadConditions::default(),
            range: None,
            part_number: None,
            response: mikrio_core::object::Representation::default(),
        },
        condition,
    ))
}

fn encoding(value: Option<&str>, decoded: bool) -> Option<String> {
    let value = value?;

    if !decoded {
        return Some(value.to_owned());
    }

    if !value
        .split(',')
        .any(|part| part.trim().eq_ignore_ascii_case("aws-chunked"))
    {
        return Some(value.to_owned());
    }

    let remaining = value
        .split(',')
        .map(str::trim)
        .filter(|part| !part.eq_ignore_ascii_case("aws-chunked"))
        .collect::<Vec<_>>()
        .join(",");

    (!remaining.is_empty()).then_some(remaining)
}

pub(super) fn head(input: HeadObjectInput) -> GetObjectInput {
    // Keep this exhaustive: a new codec option must receive an explicit decision.
    let HeadObjectInput {
        bucket,
        key,
        checksum_mode,
        expected_bucket_owner,
        if_match,
        if_none_match,
        if_modified_since,
        if_unmodified_since,
        part_number,
        range,
        request_payer,
        response_cache_control,
        response_content_disposition,
        response_content_encoding,
        response_content_language,
        response_content_type,
        response_expires,
        sse_customer_algorithm,
        sse_customer_key,
        sse_customer_key_md5,
        version_id,
    } = input;

    GetObjectInput {
        bucket,
        key,
        checksum_mode,
        expected_bucket_owner,
        if_match,
        if_none_match,
        if_modified_since,
        if_unmodified_since,
        part_number,
        range,
        request_payer,
        response_cache_control,
        response_content_disposition,
        response_content_encoding,
        response_content_language,
        response_content_type,
        response_expires,
        sse_customer_algorithm,
        sse_customer_key,
        sse_customer_key_md5,
        version_id,
    }
}
