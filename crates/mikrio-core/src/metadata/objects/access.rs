//! Current object authorization lives in the operation's own SQL transaction.

use rusqlite::Transaction;

use crate::access::{Action, Context, HttpIdentity};
use crate::object::{
    Authority, BucketName, CopyDirective, CopyRequest, ObjectTags, PutRequest, ReadRequest,
    StoredObject,
};
use crate::{AccessRequest, OperationError, OperationFailure};

pub(in crate::metadata) fn bucket(
    tx: &Transaction<'_>,
    authority: &Authority,
    name: &BucketName,
    owner: Option<&str>,
) -> Result<[u8; 16], OperationError> {
    super::require_bucket(tx, name, owner).map_err(|error| {
        if matches!(authority, Authority::Http(_))
            && error.failure() == OperationFailure::NoSuchBucket
        {
            return OperationError::new(OperationFailure::AccessDenied);
        }

        error
    })
}

pub(super) fn put(
    tx: &Transaction<'_>,
    authority: &Authority,
    request: &PutRequest,
    bucket_id: [u8; 16],
) -> Result<(), OperationError> {
    let Authority::Http(identity) = authority else {
        return Ok(());
    };

    let mut context = Context::default();
    insert(&mut context, "s3:objectcreationoperation", "true")?;

    if let Some(value) = request.conditions.if_match() {
        insert(&mut context, "s3:if-match", value)?;
    }

    if let Some(value) = request.conditions.if_none_match() {
        insert(&mut context, "s3:if-none-match", value)?;
    }

    if let Some(tags) = &request.tags {
        tag_context(&mut context, "s3:requestobjecttag/", tags)?;

        context
            .insert(
                "s3:requestobjecttagkeys",
                tags.entries().keys().cloned().collect(),
            )
            .map_err(invalid_context)?;
    }

    let target = Target {
        bucket: request.bucket.as_str(),
        bucket_id,
        key: request.key.as_str(),
        owner: request.expected_owner.as_deref(),
    };

    check(tx, identity, &target, &context, "s3:PutObject")?;

    if request.conditions.if_match().is_some() {
        check(tx, identity, &target, &context, "s3:GetObject")?;
    }

    if request.tags.is_some() {
        check(tx, identity, &target, &context, "s3:PutObjectTagging")?;
    }

    Ok(())
}

pub(super) fn copy(
    tx: &Transaction<'_>,
    authority: &Authority,
    request: &CopyRequest,
    selected: &PutRequest,
    bucket_id: [u8; 16],
) -> Result<(), OperationError> {
    let Authority::Http(identity) = authority else {
        return Ok(());
    };

    if matches!(identity.0, crate::access::Identity::Anonymous) {
        return Err(OperationError::new(OperationFailure::AccessDenied));
    }

    let mut context = Context::default();
    insert(&mut context, "s3:objectcreationoperation", "true")?;

    for (name, value) in [
        ("s3:if-match", request.conditions.if_match()),
        ("s3:if-none-match", request.conditions.if_none_match()),
    ] {
        if let Some(value) = value {
            insert(&mut context, name, value)?;
        }
    }

    // Inherited source tags are selected metadata, not request tag headers.
    if request.tagging_directive == CopyDirective::Replace
        && let Some(tags) = &request.tags
    {
        tag_context(&mut context, "s3:requestobjecttag/", tags)?;

        context
            .insert(
                "s3:requestobjecttagkeys",
                tags.entries().keys().cloned().collect(),
            )
            .map_err(invalid_context)?;
    }

    let target = Target {
        bucket: request.bucket.as_str(),
        bucket_id,
        key: request.key.as_str(),
        owner: request.expected_owner.as_deref(),
    };

    check(tx, identity, &target, &context, "s3:PutObject")?;

    if request.conditions.if_match().is_some() {
        check(tx, identity, &target, &context, "s3:GetObject")?;
    }

    if request.tags.is_some() || selected.tags.as_ref().is_some_and(|tags| tags.count() != 0) {
        check(tx, identity, &target, &context, "s3:PutObjectTagging")?;
    }

    Ok(())
}

pub(super) fn read(
    tx: &Transaction<'_>,
    authority: &Authority,
    request: &ReadRequest,
    bucket_id: [u8; 16],
    selected: Option<&StoredObject>,
) -> Result<Option<u32>, OperationError> {
    let Authority::Http(identity) = authority else {
        return Ok(selected.map(|object| object.tags.count()));
    };

    let mut context = Context::default();

    if let Some(object) = selected {
        tag_context(&mut context, "s3:existingobjecttag/", &object.tags)?;
    }

    let target = Target::read(request, bucket_id);
    check(tx, identity, &target, &context, "s3:GetObject")?;

    let Some(selected) = selected else {
        check(tx, identity, &target, &Context::default(), "s3:ListBucket")?;
        return Ok(None);
    };

    match check(tx, identity, &target, &context, "s3:GetObjectTagging") {
        Ok(()) => Ok(Some(selected.tags.count())),
        Err(error) if error.failure() == OperationFailure::AccessDenied => Ok(None),
        Err(error) => Err(error),
    }
}

pub(super) fn object_acl(
    tx: &Transaction<'_>,
    authority: &Authority,
    request: &ReadRequest,
    bucket_id: [u8; 16],
    selected: Option<&StoredObject>,
) -> Result<(), OperationError> {
    let Authority::Http(identity) = authority else {
        return Ok(());
    };

    let mut context = Context::default();

    if let Some(object) = selected {
        tag_context(&mut context, "s3:existingobjecttag/", &object.tags)?;
    }

    let target = Target::read(request, bucket_id);
    check(tx, identity, &target, &context, "s3:GetObjectAcl")?;

    if selected.is_none() {
        check(tx, identity, &target, &Context::default(), "s3:ListBucket")?;
    }

    Ok(())
}

#[derive(Clone, Copy)]
pub(super) enum Tagging<'a> {
    Get,
    Put(&'a ObjectTags),
    Delete,
}

pub(super) fn tagging(
    tx: &Transaction<'_>,
    authority: &Authority,
    request: &ReadRequest,
    bucket_id: [u8; 16],
    selected: Option<&StoredObject>,
    operation: Tagging<'_>,
) -> Result<(), OperationError> {
    let Authority::Http(identity) = authority else {
        return Ok(());
    };

    let mut context = Context::default();

    if let Some(object) = selected {
        tag_context(&mut context, "s3:existingobjecttag/", &object.tags)?;
    }

    if let Tagging::Put(tags) = operation {
        tag_context(&mut context, "s3:requestobjecttag/", tags)?;

        context
            .insert(
                "s3:requestobjecttagkeys",
                tags.entries().keys().cloned().collect(),
            )
            .map_err(invalid_context)?;
    }

    let action = match operation {
        Tagging::Get => "s3:GetObjectTagging",
        Tagging::Put(_) => "s3:PutObjectTagging",
        Tagging::Delete => "s3:DeleteObjectTagging",
    };

    let target = Target::read(request, bucket_id);
    check(tx, identity, &target, &context, action)?;

    if selected.is_none() {
        check(tx, identity, &target, &Context::default(), "s3:ListBucket")?;
    }

    Ok(())
}

pub(super) fn attributes(
    tx: &Transaction<'_>,
    authority: &Authority,
    request: &ReadRequest,
    bucket_id: [u8; 16],
    selected: Option<&StoredObject>,
) -> Result<(), OperationError> {
    let Authority::Http(identity) = authority else {
        return Ok(());
    };

    let mut context = Context::default();

    if let Some(object) = selected {
        tag_context(&mut context, "s3:existingobjecttag/", &object.tags)?;
    }

    let target = Target::read(request, bucket_id);
    // Attribute selection requires both object-read and attribute permissions.
    check(tx, identity, &target, &context, "s3:GetObject")?;
    check(tx, identity, &target, &context, "s3:GetObjectAttributes")?;

    if selected.is_none() {
        check(tx, identity, &target, &Context::default(), "s3:ListBucket")?;
    }

    Ok(())
}

pub(super) fn delete(
    tx: &Transaction<'_>,
    authority: &Authority,
    request: &ReadRequest,
    bucket_id: [u8; 16],
    if_match: Option<&str>,
) -> Result<(), OperationError> {
    let Authority::Http(identity) = authority else {
        return Ok(());
    };

    let mut context = Context::default();

    if let Some(value) = if_match {
        insert(&mut context, "s3:if-match", value)?;
    }

    let target = Target::read(request, bucket_id);
    check(tx, identity, &target, &context, "s3:DeleteObject")?;

    if if_match.is_some_and(|value| value != "*") {
        check(tx, identity, &target, &context, "s3:GetObject")?;
    }

    Ok(())
}

struct Target<'a> {
    bucket: &'a str,
    bucket_id: [u8; 16],
    key: &'a str,
    owner: Option<&'a str>,
}

impl<'a> Target<'a> {
    fn read(request: &'a ReadRequest, bucket_id: [u8; 16]) -> Self {
        Self {
            bucket: request.bucket.as_str(),
            bucket_id,
            key: request.key.as_str(),
            owner: request.expected_owner.as_deref(),
        }
    }
}

fn check(
    tx: &Transaction<'_>,
    identity: &HttpIdentity,
    target: &Target<'_>,
    context: &Context,
    action: &str,
) -> Result<(), OperationError> {
    let action = Action::parse(action).map_err(invalid_context)?;

    super::super::access::authorize(
        tx,
        identity,
        &AccessRequest {
            bucket: target.bucket,
            expected_bucket_id: Some(target.bucket_id),
            key: action.object_resource().then_some(target.key),
            action,
            context,
            expected_owner: target.owner,
        },
    )
    .map_err(OperationError::from)
}

pub(in crate::metadata) fn tag_context(
    context: &mut Context,
    prefix: &str,
    tags: &ObjectTags,
) -> Result<(), OperationError> {
    let mut keys = std::collections::BTreeSet::new();

    for (key, value) in tags.entries() {
        // Reject ambiguous authorization before evaluating either Allow or Deny.
        // Ignoring one case alias could otherwise bypass an explicit denial.
        // Unicode suffixes cannot appear in an accepted policy yet.
        if !key.is_ascii() {
            continue;
        }

        if !keys.insert(key.to_ascii_lowercase()) {
            return Err(OperationError::new(OperationFailure::AccessDenied));
        }

        insert(context, &format!("{prefix}{key}"), value)?;
    }

    Ok(())
}

fn insert(context: &mut Context, key: &str, value: &str) -> Result<(), OperationError> {
    context
        .insert(key, vec![value.into()])
        .map_err(invalid_context)
}

fn invalid_context(error: crate::access::PolicyError) -> OperationError {
    OperationError::caused(OperationFailure::IoError, error)
}
