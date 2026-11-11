//! Strict lifecycle projections; the worker owns validation and atomic replacement.

use mikrio_core::lifecycle::{LifecycleConfiguration, LifecycleFilter, LifecycleRule};
use s3s::{S3Request, S3Response, S3Result, dto, s3_error};

use super::{Storage, configuration::bucket, error, identity, integrity::Integrity};

pub(super) async fn put(
    storage: &Storage,
    request: S3Request<dto::PutBucketLifecycleConfigurationInput>,
) -> S3Result<S3Response<dto::PutBucketLifecycleConfigurationOutput>> {
    Integrity::envelope(&request)?;

    if request
        .headers
        .contains_key("x-amz-transition-default-minimum-object-size")
    {
        return Err(s3_error!(NotImplemented));
    }

    let identity = identity(&request)?;
    let input = request.input;

    let configuration = input
        .lifecycle_configuration
        .ok_or_else(|| s3_error!(MalformedXML))?;

    let rules = configuration
        .rules
        .into_iter()
        .map(rule)
        .collect::<S3Result<Vec<_>>>()?;

    let configuration =
        LifecycleConfiguration::new(rules).map_err(|_| s3_error!(InvalidArgument))?;

    storage
        .0
        .put_bucket_lifecycle_http(
            identity,
            bucket(&input.bucket, input.expected_bucket_owner)?,
            configuration,
        )
        .await
        .map_err(|error| error::operation(&error))?;

    Ok(S3Response::new(
        dto::PutBucketLifecycleConfigurationOutput::default(),
    ))
}

fn rule(rule: dto::LifecycleRule) -> S3Result<LifecycleRule> {
    if rule.noncurrent_version_expiration.is_some()
        || rule.noncurrent_version_transitions.is_some()
        || rule.transitions.is_some()
    {
        return Err(s3_error!(NotImplemented));
    }

    let is_enabled = match rule.status.as_str() {
        "Enabled" => true,
        "Disabled" => false,
        _ => return Err(s3_error!(InvalidArgument)),
    };

    let filter = match (rule.filter, rule.prefix) {
        (Some(filter), None) => {
            if filter.and.is_some()
                || filter.tag.is_some()
                || filter.object_size_greater_than.is_some()
                || filter.object_size_less_than.is_some()
            {
                return Err(s3_error!(NotImplemented));
            }

            filter
                .prefix
                .map_or(LifecycleFilter::All, LifecycleFilter::Prefix)
        }
        (None, Some(prefix)) => LifecycleFilter::LegacyPrefix(prefix),
        _ => return Err(s3_error!(InvalidArgument)),
    };

    let expiration_days = rule
        .expiration
        .map(|action| {
            if action.date.is_some() || action.expired_object_delete_marker.is_some() {
                return Err(s3_error!(NotImplemented));
            }

            days(action.days)
        })
        .transpose()?;

    let abort_incomplete_multipart_days = rule
        .abort_incomplete_multipart_upload
        .map(|action| days(action.days_after_initiation))
        .transpose()?;

    Ok(LifecycleRule {
        id: rule.id,
        is_enabled,
        filter,
        expiration_days,
        abort_incomplete_multipart_days,
    })
}

fn days(value: Option<i32>) -> S3Result<u32> {
    let value = value
        .filter(|value| *value > 0)
        .ok_or_else(|| s3_error!(InvalidArgument))?;

    u32::try_from(value).map_err(|_| s3_error!(InvalidArgument))
}

pub(super) async fn get(
    storage: &Storage,
    request: S3Request<dto::GetBucketLifecycleConfigurationInput>,
) -> S3Result<S3Response<dto::GetBucketLifecycleConfigurationOutput>> {
    Integrity::bodyless(&request)?;

    let configuration = storage
        .0
        .get_bucket_lifecycle_http(
            identity(&request)?,
            bucket(&request.input.bucket, request.input.expected_bucket_owner)?,
        )
        .await
        .map_err(|error| error::operation(&error))?
        .ok_or_else(|| s3_error!(NoSuchLifecycleConfiguration))?;

    let rules = configuration
        .into_rules()
        .into_iter()
        .map(output)
        .collect::<S3Result<Vec<_>>>()?;

    Ok(S3Response::new(
        dto::GetBucketLifecycleConfigurationOutput {
            rules: Some(rules),
            ..Default::default()
        },
    ))
}

fn output(rule: LifecycleRule) -> S3Result<dto::LifecycleRule> {
    let (filter, prefix) = match rule.filter {
        LifecycleFilter::All => (Some(dto::LifecycleRuleFilter::default()), None),
        LifecycleFilter::Prefix(prefix) => (
            Some(dto::LifecycleRuleFilter {
                prefix: Some(prefix),
                ..Default::default()
            }),
            None,
        ),
        LifecycleFilter::LegacyPrefix(prefix) => (None, Some(prefix)),
    };

    let expiration = rule
        .expiration_days
        .map(|days| -> S3Result<_> {
            Ok(dto::LifecycleExpiration {
                days: Some(i32::try_from(days).map_err(|_| s3_error!(InternalError))?),
                ..Default::default()
            })
        })
        .transpose()?;

    let abort_incomplete_multipart_upload = rule
        .abort_incomplete_multipart_days
        .map(|days| -> S3Result<_> {
            Ok(dto::AbortIncompleteMultipartUpload {
                days_after_initiation: Some(
                    i32::try_from(days).map_err(|_| s3_error!(InternalError))?,
                ),
            })
        })
        .transpose()?;

    Ok(dto::LifecycleRule {
        id: rule.id,
        status: dto::ExpirationStatus::from(
            if rule.is_enabled {
                "Enabled"
            } else {
                "Disabled"
            }
            .to_owned(),
        ),
        filter,
        prefix,
        expiration,
        abort_incomplete_multipart_upload,
        noncurrent_version_expiration: None,
        noncurrent_version_transitions: None,
        transitions: None,
    })
}

pub(super) async fn delete(
    storage: &Storage,
    request: S3Request<dto::DeleteBucketLifecycleInput>,
) -> S3Result<S3Response<dto::DeleteBucketLifecycleOutput>> {
    Integrity::bodyless(&request)?;

    storage
        .0
        .delete_bucket_lifecycle_http(
            identity(&request)?,
            bucket(&request.input.bucket, request.input.expected_bucket_owner)?,
        )
        .await
        .map_err(|error| error::operation(&error))?;

    Ok(S3Response::new(dto::DeleteBucketLifecycleOutput::default()))
}
