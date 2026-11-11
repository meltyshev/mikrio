//! CORS configuration envelopes terminate and pass integrity before mutation.

use mikrio_core::cors::{CorsConfiguration, CorsRule};
use s3s::{S3Request, S3Response, S3Result, dto, s3_error};

use super::{Storage, configuration::bucket, error, identity, integrity::Integrity};

pub(super) async fn put(
    storage: &Storage,
    request: S3Request<dto::PutBucketCorsInput>,
) -> S3Result<S3Response<dto::PutBucketCorsOutput>> {
    Integrity::envelope(&request)?;
    let identity = identity(&request)?;
    let input = request.input;

    let rules = input
        .cors_configuration
        .cors_rules
        .into_iter()
        .map(|rule| CorsRule {
            id: rule.id,
            allowed_origins: rule.allowed_origins,
            allowed_methods: rule.allowed_methods,
            allowed_headers: rule.allowed_headers,
            expose_headers: rule.expose_headers,
            max_age_seconds: rule.max_age_seconds,
        })
        .collect::<Vec<_>>();

    let configuration = CorsConfiguration::new(rules).map_err(|_| s3_error!(InvalidArgument))?;

    storage
        .0
        .put_bucket_cors_http(
            identity,
            bucket(&input.bucket, input.expected_bucket_owner)?,
            configuration,
        )
        .await
        .map_err(|error| error::operation(&error))?;

    Ok(S3Response::new(dto::PutBucketCorsOutput::default()))
}

pub(super) async fn get(
    storage: &Storage,
    request: S3Request<dto::GetBucketCorsInput>,
) -> S3Result<S3Response<dto::GetBucketCorsOutput>> {
    Integrity::bodyless(&request)?;

    let configuration = storage
        .0
        .get_bucket_cors_http(
            identity(&request)?,
            bucket(&request.input.bucket, request.input.expected_bucket_owner)?,
        )
        .await
        .map_err(|error| error::operation(&error))?;

    let rules = configuration
        .into_rules()
        .into_iter()
        .map(|rule| dto::CORSRule {
            id: rule.id,
            allowed_origins: rule.allowed_origins,
            allowed_methods: rule.allowed_methods,
            allowed_headers: rule.allowed_headers,
            expose_headers: rule.expose_headers,
            max_age_seconds: rule.max_age_seconds,
        })
        .collect::<Vec<_>>();

    Ok(S3Response::new(dto::GetBucketCorsOutput {
        cors_rules: Some(rules),
    }))
}

pub(super) async fn delete(
    storage: &Storage,
    request: S3Request<dto::DeleteBucketCorsInput>,
) -> S3Result<S3Response<dto::DeleteBucketCorsOutput>> {
    Integrity::bodyless(&request)?;

    storage
        .0
        .delete_bucket_cors_http(
            identity(&request)?,
            bucket(&request.input.bucket, request.input.expected_bucket_owner)?,
        )
        .await
        .map_err(|error| error::operation(&error))?;

    Ok(S3Response::new(dto::DeleteBucketCorsOutput::default()))
}
