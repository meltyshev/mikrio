//! Explicit bucket wire values; all namespace/access decisions remain in core.

use mikrio_core::object::BucketName;
use s3s::dto::{
    CreateBucketInput, CreateBucketOutput, DeleteBucketInput, DeleteBucketOutput,
    GetBucketLocationInput, GetBucketLocationOutput, HeadBucketInput, HeadBucketOutput,
};
use s3s::{S3Request, S3Response, S3Result, s3_error};

use super::{Storage, error, identity, input, integrity};

pub(super) async fn create(
    storage: &Storage,
    request: S3Request<CreateBucketInput>,
) -> S3Result<S3Response<CreateBucketOutput>> {
    let identity = identity(&request)?;
    integrity::Integrity::envelope(&request)?;
    let input = &request.input;
    let headers = &request.headers;

    if headers
        .keys()
        .any(|name| name.as_str().starts_with("x-amz-grant-"))
    {
        return Err(s3_error!(AccessControlListNotSupported));
    }

    if input::header(headers, "x-amz-acl")?.is_some_and(|acl| acl != "private") {
        return Err(s3_error!(AccessControlListNotSupported));
    }

    if headers.contains_key("x-amz-bucket-object-lock-enabled")
        || headers.contains_key("x-amz-bucket-namespace")
        || headers.contains_key("x-amz-expected-bucket-owner")
    {
        return Err(s3_error!(NotImplemented));
    }

    if input::header(headers, "x-amz-object-ownership")?
        .is_some_and(|ownership| ownership != "BucketOwnerEnforced")
    {
        return Err(s3_error!(NotImplemented));
    }

    if input
        .create_bucket_configuration
        .as_ref()
        .is_some_and(|configuration| {
            configuration.bucket.is_some()
                || configuration.location.is_some()
                || configuration.tags.is_some()
        })
    {
        return Err(s3_error!(NotImplemented));
    }

    let constraint = input
        .create_bucket_configuration
        .as_ref()
        .and_then(|configuration| configuration.location_constraint.as_ref());

    let constraint = constraint.map(s3s::dto::BucketLocationConstraint::as_str);
    let region = &storage.0.info().record.region;

    if constraint == Some("us-east-1") || constraint == Some("") {
        return Err(s3_error!(InvalidLocationConstraint));
    }

    let requested_region = match constraint {
        None => "us-east-1",
        Some("EU") => "eu-west-1",
        Some(region) => region,
    };

    if requested_region != region {
        return Err(s3_error!(IllegalLocationConstraintException));
    }

    storage
        .0
        .create_bucket_http(identity, bucket(&input.bucket)?)
        .await
        .map_err(|error| error::operation(&error))?;

    Ok(S3Response::new(CreateBucketOutput {
        location: Some(format!("/{}", input.bucket)),
        ..CreateBucketOutput::default()
    }))
}

pub(super) async fn head(
    storage: &Storage,
    request: S3Request<HeadBucketInput>,
) -> S3Result<S3Response<HeadBucketOutput>> {
    integrity::Integrity::bodyless(&request)?;

    let region = storage
        .0
        .head_bucket_http(
            identity(&request)?,
            bucket(&request.input.bucket)?,
            request.input.expected_bucket_owner,
        )
        .await
        .map_err(|error| error::operation(&error))?;

    Ok(S3Response::new(HeadBucketOutput {
        bucket_region: Some(region),
        ..HeadBucketOutput::default()
    }))
}

pub(super) async fn location(
    storage: &Storage,
    request: S3Request<GetBucketLocationInput>,
) -> S3Result<S3Response<GetBucketLocationOutput>> {
    integrity::Integrity::bodyless(&request)?;

    let region = storage
        .0
        .get_bucket_location_http(
            identity(&request)?,
            bucket(&request.input.bucket)?,
            request.input.expected_bucket_owner,
        )
        .await
        .map_err(|error| error::operation(&error))?;

    Ok(S3Response::new(GetBucketLocationOutput {
        location_constraint: (region != "us-east-1").then(|| region.into()),
    }))
}

pub(super) async fn delete(
    storage: &Storage,
    request: S3Request<DeleteBucketInput>,
) -> S3Result<S3Response<DeleteBucketOutput>> {
    integrity::Integrity::bodyless(&request)?;

    storage
        .0
        .delete_bucket_http(
            identity(&request)?,
            bucket(&request.input.bucket)?,
            request.input.expected_bucket_owner,
        )
        .await
        .map_err(|error| error::operation(&error))?;

    Ok(S3Response::new(DeleteBucketOutput::default()))
}

fn bucket(value: &str) -> S3Result<BucketName> {
    BucketName::parse(value).map_err(|_| s3_error!(InvalidBucketName))
}
