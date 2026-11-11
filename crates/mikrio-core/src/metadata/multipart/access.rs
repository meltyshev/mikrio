//! Initiation context and immutable historical identity, checked with current authority.

use rusqlite::Transaction;

use crate::access::{Action, Context, Identity};
use crate::multipart::CreateRequest;
use crate::object::Authority;
use crate::{AccessRequest, OperationError, OperationFailure};

pub(super) fn initiation(
    transaction: &Transaction<'_>,
    authority: &Authority,
    request: &CreateRequest,
) -> Result<([u8; 16], String), OperationError> {
    let bucket_id = super::super::objects::access::bucket(
        transaction,
        authority,
        &request.bucket,
        request.expected_owner.as_deref(),
    )?;

    let owner = super::super::admin::owner_info(transaction)?;

    let Authority::Http(identity) = authority else {
        return Ok((bucket_id, owner.canonical_id().into()));
    };

    let initiator = match &identity.0 {
        Identity::Anonymous => return Err(OperationError::new(OperationFailure::AccessDenied)),
        Identity::Bootstrap(_) => owner.canonical_id().into(),
        Identity::Application { principal, .. } => principal
            .arn(owner.account_id())
            .map_err(|error| OperationError::caused(OperationFailure::AccessDenied, error))?,
    };

    let mut context = Context::default();

    context
        .insert("s3:objectcreationoperation", vec!["false".into()])
        .map_err(invalid)?;

    if let Some(tags) = &request.tags {
        super::super::objects::access::tag_context(&mut context, "s3:requestobjecttag/", tags)?;

        context
            .insert(
                "s3:requestobjecttagkeys",
                tags.entries().keys().cloned().collect(),
            )
            .map_err(invalid)?;
    }

    for action in [
        Some("s3:PutObject"),
        request.tags.as_ref().map(|_| "s3:PutObjectTagging"),
    ]
    .into_iter()
    .flatten()
    {
        super::super::access::authorize(
            transaction,
            identity,
            &AccessRequest {
                bucket: request.bucket.as_str(),
                expected_bucket_id: Some(bucket_id),
                key: Some(request.key.as_str()),
                action: Action::parse(action).map_err(invalid)?,
                context: &context,
                expected_owner: request.expected_owner.as_deref(),
            },
        )?;
    }

    Ok((bucket_id, initiator))
}

fn invalid(error: crate::access::PolicyError) -> OperationError {
    OperationError::caused(OperationFailure::InvalidArgument, error)
}

pub(super) fn part(
    transaction: &Transaction<'_>,
    authority: &Authority,
    request: &crate::multipart::PartRequest,
) -> Result<[u8; 16], OperationError> {
    let bucket_id = super::super::objects::access::bucket(
        transaction,
        authority,
        &request.bucket,
        request.expected_owner.as_deref(),
    )?;

    let Authority::Http(identity) = authority else {
        return Ok(bucket_id);
    };

    let mut context = Context::default();

    context
        .insert("s3:objectcreationoperation", vec!["false".into()])
        .map_err(invalid)?;

    super::super::access::authorize(
        transaction,
        identity,
        &AccessRequest {
            bucket: request.bucket.as_str(),
            expected_bucket_id: Some(bucket_id),
            key: Some(request.key.as_str()),
            action: Action::parse("s3:PutObject").map_err(invalid)?,
            context: &context,
            expected_owner: request.expected_owner.as_deref(),
        },
    )?;

    Ok(bucket_id)
}

pub(super) fn active(
    transaction: &Transaction<'_>,
    authority: &Authority,
    request: &crate::multipart::UploadRequest,
    action: &str,
) -> Result<([u8; 16], super::upload::SelectedUpload), OperationError> {
    let bucket_id = super::super::objects::access::bucket(
        transaction,
        authority,
        &request.bucket,
        request.expected_owner.as_deref(),
    )?;

    let selected = super::upload::select(transaction, &request.upload_id, &request.key, bucket_id)?;

    if let Some(selected) = &selected {
        let owner = super::super::admin::owner_info(transaction)?;

        validate_initiator(&selected.initiator, &owner)?;
    }

    if let Authority::Http(identity) = authority {
        super::super::access::authorize_multipart(
            transaction,
            identity,
            &AccessRequest {
                bucket: request.bucket.as_str(),
                expected_bucket_id: Some(bucket_id),
                key: Some(request.key.as_str()),
                action: Action::parse(action).map_err(invalid)?,
                context: &Context::default(),
                expected_owner: request.expected_owner.as_deref(),
            },
            selected.as_ref().map(|upload| upload.initiator.as_str()),
        )?;
    }

    let selected = selected.ok_or_else(|| OperationError::new(OperationFailure::NoSuchUpload))?;

    Ok((bucket_id, selected))
}

pub(super) fn validate_initiator(
    initiator: &str,
    owner: &crate::access::OwnerInfo,
) -> Result<(), super::super::MetadataError> {
    use super::super::MetadataError;

    if initiator == owner.canonical_id() {
        return Ok(());
    }

    let prefix = format!("arn:aws:iam::{}:user/", owner.account_id());

    let principal = initiator
        .strip_prefix(&prefix)
        .ok_or(MetadataError::Corrupt("invalid upload initiator identity"))?;

    crate::access::PrincipalId::parse(principal)
        .map_err(|_| MetadataError::Corrupt("invalid upload initiator identity"))?;

    Ok(())
}

pub(super) fn completion(
    transaction: &Transaction<'_>,
    authority: &Authority,
    request: &crate::multipart::CompleteRequest,
    bucket_id: [u8; 16],
) -> Result<(), OperationError> {
    let Authority::Http(identity) = authority else {
        return Ok(());
    };

    let mut context = Context::default();

    context
        .insert("s3:objectcreationoperation", vec!["true".into()])
        .map_err(invalid)?;

    for (name, value) in [
        ("s3:if-match", request.conditions.if_match()),
        ("s3:if-none-match", request.conditions.if_none_match()),
    ] {
        if let Some(value) = value {
            context.insert(name, vec![value.into()]).map_err(invalid)?;
        }
    }

    // Initiation tags are persisted metadata, not submitted completion headers.
    for action in [
        Some("s3:PutObject"),
        request.conditions.if_match().map(|_| "s3:GetObject"),
    ]
    .into_iter()
    .flatten()
    {
        super::super::access::authorize(
            transaction,
            identity,
            &AccessRequest {
                bucket: request.upload.bucket.as_str(),
                expected_bucket_id: Some(bucket_id),
                key: Some(request.upload.key.as_str()),
                action: Action::parse(action).map_err(invalid)?,
                context: &context,
                expected_owner: request.upload.expected_owner.as_deref(),
            },
        )?;
    }

    Ok(())
}
