//! Object tag subresources validate complete envelopes before shared transactions.

use mikrio_core::object::{
    BucketName, ObjectKey, ObjectTags, ReadConditions, ReadRequest, Representation,
};
use s3s::{S3Request, S3Response, S3Result, dto, s3_error};

use super::{Storage, error, identity, integrity::Integrity};

fn request<T>(
    request: &S3Request<T>,
    bucket: &str,
    key: &str,
    version: Option<&str>,
) -> S3Result<ReadRequest> {
    if version.is_some() || request.headers.contains_key("x-amz-request-payer") {
        return Err(s3_error!(NotImplemented));
    }

    // Tagging has no supported conditional-write or range contract.
    if [
        "if-match",
        "if-none-match",
        "if-modified-since",
        "if-unmodified-since",
        "range",
    ]
    .iter()
    .any(|name| request.headers.contains_key(*name))
    {
        return Err(s3_error!(NotImplemented));
    }

    Ok(ReadRequest {
        bucket: BucketName::parse(bucket).map_err(|_| s3_error!(InvalidBucketName))?,
        key: ObjectKey::parse(key).map_err(|_| s3_error!(InvalidArgument))?,
        expected_owner: crate::access::expected_owner(&request.headers)?.map(str::to_owned),
        conditions: ReadConditions::default(),
        range: None,
        part_number: None,
        response: Representation::default(),
    })
}

pub(super) async fn put(
    storage: &Storage,
    request: S3Request<dto::PutObjectTaggingInput>,
) -> S3Result<S3Response<dto::PutObjectTaggingOutput>> {
    Integrity::envelope(&request)?;
    let input = &request.input;

    let target = self::request(
        &request,
        &input.bucket,
        &input.key,
        input.version_id.as_deref(),
    )?;

    let tags = configuration(&input.tagging)?;

    storage
        .0
        .replace_object_tags_http(identity(&request)?, target, tags)
        .await
        .map_err(|error| error::operation(&error))?;

    Ok(S3Response::new(dto::PutObjectTaggingOutput::default()))
}

pub(super) async fn get(
    storage: &Storage,
    request: S3Request<dto::GetObjectTaggingInput>,
) -> S3Result<S3Response<dto::GetObjectTaggingOutput>> {
    Integrity::bodyless(&request)?;
    let input = &request.input;

    let target = self::request(
        &request,
        &input.bucket,
        &input.key,
        input.version_id.as_deref(),
    )?;

    let tags = storage
        .0
        .get_object_tags_http(identity(&request)?, target)
        .await
        .map_err(|error| error::operation(&error))?;

    Ok(S3Response::new(dto::GetObjectTaggingOutput {
        tag_set: tags
            .into_entries()
            .into_iter()
            .map(|(key, value)| dto::Tag {
                key: Some(key),
                value: Some(value),
            })
            .collect(),
        version_id: None,
    }))
}

pub(super) async fn delete(
    storage: &Storage,
    request: S3Request<dto::DeleteObjectTaggingInput>,
) -> S3Result<S3Response<dto::DeleteObjectTaggingOutput>> {
    Integrity::bodyless(&request)?;
    let input = &request.input;

    let target = self::request(
        &request,
        &input.bucket,
        &input.key,
        input.version_id.as_deref(),
    )?;

    storage
        .0
        .delete_object_tags_http(identity(&request)?, target)
        .await
        .map_err(|error| error::operation(&error))?;

    Ok(S3Response::new(dto::DeleteObjectTaggingOutput::default()))
}

/// Preserve explicit empty input until the core has checked tag permissions.
pub(super) fn inline(value: &str) -> S3Result<ObjectTags> {
    if value.is_empty() {
        return Ok(ObjectTags::default());
    }

    let mut entries = Vec::new();

    for pair in value.split('&') {
        if entries.len() == 10 {
            return Err(s3_error!(InvalidTag));
        }

        let (key, value) = pair
            .split_once('=')
            .ok_or_else(|| s3_error!(InvalidArgument))?;

        entries.push((component(key)?, component(value)?));
    }

    ObjectTags::new(entries).map_err(|_| s3_error!(InvalidTag))
}

fn component(value: &str) -> S3Result<String> {
    let bytes = value.as_bytes();

    for (index, byte) in bytes.iter().enumerate() {
        if *byte != b'%' {
            continue;
        }

        if !bytes
            .get(index + 1..index + 3)
            .is_some_and(|digits| digits.iter().all(u8::is_ascii_hexdigit))
        {
            return Err(s3_error!(InvalidArgument));
        }
    }

    let value = value.replace('+', " ");

    urlencoding::decode(&value)
        .map(std::borrow::Cow::into_owned)
        .map_err(|_| s3_error!(InvalidArgument))
}

pub(super) fn configuration(value: &dto::Tagging) -> S3Result<ObjectTags> {
    // The strict codec rejects unknown/duplicate fields; required Key/Value
    // are optional in its DTO and must be checked before whole-set validation.
    let entries = value
        .tag_set
        .iter()
        .map(|tag| {
            Ok((
                tag.key.clone().ok_or_else(|| s3_error!(MalformedXML))?,
                tag.value.clone().ok_or_else(|| s3_error!(MalformedXML))?,
            ))
        })
        .collect::<S3Result<Vec<_>>>()?;

    ObjectTags::new(entries).map_err(|_| s3_error!(InvalidTag))
}
