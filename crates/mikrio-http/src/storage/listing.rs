//! Listing wire projection follows logical grouping and token selection in core.

use mikrio_core::object::{BucketName, ListObjectsRequest, ObjectPageSize};
use s3s::dto::{CommonPrefix, ListObjectsV2Input, ListObjectsV2Output, Object, ObjectStorageClass};
use s3s::{S3Request, S3Response, S3Result, s3_error};

use super::{Storage, error, identity, integrity, output};

pub(super) async fn objects(
    storage: &Storage,
    request: S3Request<ListObjectsV2Input>,
) -> S3Result<S3Response<ListObjectsV2Output>> {
    let identity = identity(&request)?;
    integrity::Integrity::bodyless(&request)?;
    let input = request.input;

    if input.fetch_owner == Some(true)
        // The codec collapses an empty optional header to absence.
        || request.headers.contains_key("x-amz-request-payer")
        || input.optional_object_attributes.is_some()
    {
        return Err(s3_error!(NotImplemented));
    }

    let encoded = match input
        .encoding_type
        .as_ref()
        .map(s3s::dto::EncodingType::as_str)
    {
        None => false,
        Some("url") => true,
        Some(_) => return Err(s3_error!(InvalidArgument)),
    };

    let max_keys =
        u32::try_from(input.max_keys.unwrap_or(1000)).map_err(|_| s3_error!(InvalidArgument))?;

    let max_keys = ObjectPageSize::new(max_keys).map_err(|_| s3_error!(InvalidArgument))?;

    let page = storage
        .0
        .list_objects_v2_http(
            identity,
            ListObjectsRequest {
                bucket: BucketName::parse(&input.bucket)
                    .map_err(|_| s3_error!(InvalidBucketName))?,
                expected_owner: input.expected_bucket_owner,
                prefix: input.prefix.clone(),
                delimiter: input.delimiter.clone(),
                start_after: input.start_after.clone(),
                continuation_token: input.continuation_token.clone(),
                max_keys,
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

    let key_count = i32::try_from(page.contents.len() + page.common_prefixes.len())
        .map_err(|_| s3_error!(InternalError))?;

    let contents = page
        .contents
        .into_iter()
        .map(|object| {
            Ok(Object {
                key: Some(encode(object.key)),
                size: Some(i64::try_from(object.size).map_err(|_| s3_error!(InternalError))?),
                e_tag: Some(object.etag.parse().map_err(|_| s3_error!(InternalError))?),
                last_modified: Some(output::timestamp(object.last_modified)?),
                storage_class: Some(ObjectStorageClass::from_static(
                    ObjectStorageClass::STANDARD,
                )),
                ..Object::default()
            })
        })
        .collect::<S3Result<Vec<_>>>()?;

    Ok(S3Response::new(ListObjectsV2Output {
        name: Some(input.bucket),
        prefix: Some(encode(input.prefix.unwrap_or_default())),
        delimiter: input.delimiter.map(encode),
        start_after: input.start_after.map(encode),
        encoding_type: input.encoding_type,
        max_keys: Some(i32::try_from(max_keys.get()).map_err(|_| s3_error!(InternalError))?),
        key_count: Some(key_count),
        contents: Some(contents),
        common_prefixes: Some(
            page.common_prefixes
                .into_iter()
                .map(|prefix| CommonPrefix {
                    prefix: Some(encode(prefix)),
                })
                .collect(),
        ),
        continuation_token: input.continuation_token,
        is_truncated: Some(page.next_continuation_token.is_some()),
        next_continuation_token: page.next_continuation_token,
        ..ListObjectsV2Output::default()
    }))
}

pub(super) async fn buckets(
    storage: &Storage,
    request: S3Request<s3s::dto::ListBucketsInput>,
) -> S3Result<S3Response<s3s::dto::ListBucketsOutput>> {
    use mikrio_core::object::{BucketPageSize, ListBucketsRequest};
    use s3s::dto::{Bucket, ListBucketsOutput, Owner};

    let identity = identity(&request)?;
    integrity::Integrity::bodyless(&request)?;

    if request.headers.contains_key("x-amz-expected-bucket-owner")
        || request.headers.contains_key("x-amz-request-payer")
    {
        return Err(s3_error!(NotImplemented));
    }

    let input = request.input;

    let paginated = input.max_buckets.is_some()
        || input.prefix.is_some()
        || input.continuation_token.is_some()
        || input.bucket_region.is_some();

    let max_buckets = if paginated {
        let value = u32::try_from(input.max_buckets.unwrap_or(10_000))
            .map_err(|_| s3_error!(InvalidArgument))?;

        Some(BucketPageSize::new(value).map_err(|_| s3_error!(InvalidArgument))?)
    } else {
        None
    };

    let page = storage
        .0
        .list_buckets_http(
            identity,
            ListBucketsRequest {
                prefix: input.prefix.clone(),
                continuation_token: input.continuation_token,
                max_buckets,
                bucket_region: input.bucket_region,
            },
        )
        .await
        .map_err(|error| error::operation(&error))?;

    let buckets = page
        .buckets
        .into_iter()
        .map(|bucket| {
            Ok(Bucket {
                name: Some(bucket.name),
                creation_date: Some(output::timestamp(bucket.creation_date)?),
                bucket_region: paginated.then(|| page.bucket_region.clone()),
                ..Bucket::default()
            })
        })
        .collect::<S3Result<Vec<_>>>()?;

    Ok(S3Response::new(ListBucketsOutput {
        buckets: Some(buckets),
        owner: Some(Owner {
            id: Some(page.canonical_owner_id),
            ..Owner::default()
        }),
        continuation_token: page.continuation_token,
        prefix: input.prefix,
    }))
}
