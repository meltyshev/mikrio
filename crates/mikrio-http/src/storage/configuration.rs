//! Configuration envelopes are complete and verified before core transactions.

use mikrio_core::access::BucketAccessRequest;
use mikrio_core::object::{BucketName, ObjectKey, ReadConditions, ReadRequest};
use mikrio_core::{MutationOutcome, OperationFailure, PublicAccessBlock};
use s3s::dto;
use s3s::{S3Request, S3Response, S3Result, s3_error};

use super::{Storage, error, identity, integrity::Integrity};

pub(super) async fn put_policy(
    storage: &Storage,
    request: S3Request<dto::PutBucketPolicyInput>,
) -> S3Result<S3Response<dto::PutBucketPolicyOutput>> {
    Integrity::envelope(&request)?;

    if request
        .headers
        .contains_key("x-amz-confirm-remove-self-bucket-access")
    {
        return Err(s3_error!(NotImplemented));
    }

    let identity = identity(&request)?;
    let input = request.input;

    storage
        .0
        .put_bucket_policy_http(
            identity,
            bucket(&input.bucket, input.expected_bucket_owner)?,
            input.policy,
        )
        .await
        .map_err(|error| {
            if error.failure() == OperationFailure::InvalidArgument
                && error.outcome() != Some(MutationOutcome::Unknown)
            {
                return s3_error!(MalformedPolicy);
            }

            error::operation(&error)
        })?;

    Ok(S3Response::new(dto::PutBucketPolicyOutput::default()))
}

pub(super) async fn get_policy(
    storage: &Storage,
    request: S3Request<dto::GetBucketPolicyInput>,
) -> S3Result<S3Response<dto::GetBucketPolicyOutput>> {
    Integrity::bodyless(&request)?;

    let value = storage
        .0
        .get_bucket_policy_http(
            identity(&request)?,
            bucket(&request.input.bucket, request.input.expected_bucket_owner)?,
        )
        .await
        .map_err(|error| error::operation(&error))?;

    Ok(S3Response::new(dto::GetBucketPolicyOutput {
        policy: Some(value.document),
    }))
}

pub(super) async fn delete_policy(
    storage: &Storage,
    request: S3Request<dto::DeleteBucketPolicyInput>,
) -> S3Result<S3Response<dto::DeleteBucketPolicyOutput>> {
    Integrity::bodyless(&request)?;

    storage
        .0
        .delete_bucket_policy_http(
            identity(&request)?,
            bucket(&request.input.bucket, request.input.expected_bucket_owner)?,
        )
        .await
        .map_err(|error| error::operation(&error))?;

    Ok(S3Response::new(dto::DeleteBucketPolicyOutput::default()))
}

pub(super) async fn policy_status(
    storage: &Storage,
    request: S3Request<dto::GetBucketPolicyStatusInput>,
) -> S3Result<S3Response<dto::GetBucketPolicyStatusOutput>> {
    Integrity::bodyless(&request)?;

    let is_public = storage
        .0
        .get_bucket_policy_status_http(
            identity(&request)?,
            bucket(&request.input.bucket, request.input.expected_bucket_owner)?,
        )
        .await
        .map_err(|error| error::operation(&error))?;

    Ok(S3Response::new(dto::GetBucketPolicyStatusOutput {
        policy_status: Some(dto::PolicyStatus {
            is_public: Some(is_public),
        }),
    }))
}

pub(super) async fn put_block(
    storage: &Storage,
    request: S3Request<dto::PutPublicAccessBlockInput>,
) -> S3Result<S3Response<dto::PutPublicAccessBlockOutput>> {
    Integrity::envelope(&request)?;
    let identity = identity(&request)?;
    let input = request.input;
    let configuration = input.public_access_block_configuration;

    let flags = PublicAccessBlock {
        block_public_acls: configuration.block_public_acls.unwrap_or(false),
        ignore_public_acls: configuration.ignore_public_acls.unwrap_or(false),
        block_public_policy: configuration.block_public_policy.unwrap_or(false),
        restrict_public_buckets: configuration.restrict_public_buckets.unwrap_or(false),
    };

    storage
        .0
        .put_public_access_block_http(
            identity,
            bucket(&input.bucket, input.expected_bucket_owner)?,
            flags,
        )
        .await
        .map_err(|error| error::operation(&error))?;

    Ok(S3Response::new(dto::PutPublicAccessBlockOutput::default()))
}

pub(super) async fn get_block(
    storage: &Storage,
    request: S3Request<dto::GetPublicAccessBlockInput>,
) -> S3Result<S3Response<dto::GetPublicAccessBlockOutput>> {
    Integrity::bodyless(&request)?;

    let flags = storage
        .0
        .get_public_access_block_http(
            identity(&request)?,
            bucket(&request.input.bucket, request.input.expected_bucket_owner)?,
        )
        .await
        .map_err(|error| error::operation(&error))?;

    Ok(S3Response::new(dto::GetPublicAccessBlockOutput {
        public_access_block_configuration: Some(dto::PublicAccessBlockConfiguration {
            block_public_acls: Some(flags.block_public_acls),
            ignore_public_acls: Some(flags.ignore_public_acls),
            block_public_policy: Some(flags.block_public_policy),
            restrict_public_buckets: Some(flags.restrict_public_buckets),
        }),
    }))
}

pub(super) async fn delete_block(
    storage: &Storage,
    request: S3Request<dto::DeletePublicAccessBlockInput>,
) -> S3Result<S3Response<dto::DeletePublicAccessBlockOutput>> {
    Integrity::bodyless(&request)?;

    storage
        .0
        .delete_public_access_block_http(
            identity(&request)?,
            bucket(&request.input.bucket, request.input.expected_bucket_owner)?,
        )
        .await
        .map_err(|error| error::operation(&error))?;

    Ok(S3Response::new(
        dto::DeletePublicAccessBlockOutput::default(),
    ))
}

pub(super) async fn put_ownership(
    storage: &Storage,
    request: S3Request<dto::PutBucketOwnershipControlsInput>,
) -> S3Result<S3Response<dto::PutBucketOwnershipControlsOutput>> {
    Integrity::envelope(&request)?;
    let rules = &request.input.ownership_controls.rules;

    if rules.len() != 1 {
        return Err(s3_error!(MalformedXML));
    }

    if rules[0].object_ownership.as_str() != "BucketOwnerEnforced" {
        return Err(s3_error!(NotImplemented));
    }

    storage
        .0
        .put_bucket_ownership_controls_http(
            identity(&request)?,
            bucket(&request.input.bucket, request.input.expected_bucket_owner)?,
        )
        .await
        .map_err(|error| error::operation(&error))?;

    Ok(S3Response::new(
        dto::PutBucketOwnershipControlsOutput::default(),
    ))
}

pub(super) async fn get_ownership(
    storage: &Storage,
    request: S3Request<dto::GetBucketOwnershipControlsInput>,
) -> S3Result<S3Response<dto::GetBucketOwnershipControlsOutput>> {
    Integrity::bodyless(&request)?;

    storage
        .0
        .get_bucket_ownership_controls_http(
            identity(&request)?,
            bucket(&request.input.bucket, request.input.expected_bucket_owner)?,
        )
        .await
        .map_err(|error| error::operation(&error))?;

    Ok(S3Response::new(dto::GetBucketOwnershipControlsOutput {
        ownership_controls: Some(dto::OwnershipControls {
            rules: vec![dto::OwnershipControlsRule {
                object_ownership: dto::ObjectOwnership::from_static(
                    dto::ObjectOwnership::BUCKET_OWNER_ENFORCED,
                ),
            }],
        }),
    }))
}

pub(super) async fn bucket_acl(
    storage: &Storage,
    request: S3Request<dto::GetBucketAclInput>,
) -> S3Result<S3Response<dto::GetBucketAclOutput>> {
    Integrity::bodyless(&request)?;

    let owner = storage
        .0
        .get_bucket_acl_http(
            identity(&request)?,
            bucket(&request.input.bucket, request.input.expected_bucket_owner)?,
        )
        .await
        .map_err(|error| error::operation(&error))?;

    let (owner, grants) = acl(&owner);

    Ok(S3Response::new(dto::GetBucketAclOutput {
        owner: Some(owner),
        grants: Some(grants),
    }))
}

pub(super) async fn object_acl(
    storage: &Storage,
    request: S3Request<dto::GetObjectAclInput>,
) -> S3Result<S3Response<dto::GetObjectAclOutput>> {
    Integrity::bodyless(&request)?;

    if request.input.version_id.is_some() || request.headers.contains_key("x-amz-request-payer") {
        return Err(s3_error!(NotImplemented));
    }

    let identity = identity(&request)?;
    let input = request.input;

    let read = ReadRequest {
        bucket: BucketName::parse(&input.bucket).map_err(|_| s3_error!(InvalidBucketName))?,
        key: ObjectKey::parse(&input.key).map_err(|_| s3_error!(InvalidArgument))?,
        expected_owner: input.expected_bucket_owner,
        conditions: ReadConditions::default(),
        range: None,
        part_number: None,
        response: mikrio_core::object::Representation::default(),
    };

    let owner = storage
        .0
        .get_object_acl_http(identity, read)
        .await
        .map_err(|error| error::operation(&error))?;

    let (owner, grants) = acl(&owner);

    Ok(S3Response::new(dto::GetObjectAclOutput {
        owner: Some(owner),
        grants: Some(grants),
        request_charged: None,
    }))
}

pub(super) fn bucket(name: &str, expected_owner: Option<String>) -> S3Result<BucketAccessRequest> {
    Ok(BucketAccessRequest {
        bucket: BucketName::parse(name).map_err(|_| s3_error!(InvalidBucketName))?,
        expected_owner,
    })
}

fn acl(owner: &mikrio_core::access::OwnerInfo) -> (dto::Owner, dto::Grants) {
    let canonical_id = owner.canonical_id().to_owned();

    let owner = dto::Owner {
        id: Some(canonical_id.clone()),
        display_name: None,
    };

    let grants = vec![dto::Grant {
        grantee: Some(dto::Grantee {
            id: Some(canonical_id),
            type_: dto::Type::from_static(dto::Type::CANONICAL_USER),
            display_name: None,
            email_address: None,
            uri: None,
        }),
        permission: Some(dto::Permission::from_static(dto::Permission::FULL_CONTROL)),
    }];

    (owner, grants)
}
