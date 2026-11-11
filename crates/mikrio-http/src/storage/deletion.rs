//! Validate the complete envelope before the shared per-entry mutation driver.

use mikrio_core::object::{BucketName, DeleteObjectsRequest, ObjectIdentifier, ObjectKey};
use s3s::dto::{DeleteObjectsInput, DeleteObjectsOutput, DeletedObject, ETag, Error};
use s3s::{S3Request, S3Response, S3Result, s3_error};

use super::{Storage, error, identity, integrity::Integrity};

pub(super) async fn delete(
    storage: &Storage,
    request: S3Request<DeleteObjectsInput>,
) -> S3Result<S3Response<DeleteObjectsOutput>> {
    let identity = identity(&request)?;

    if [
        "x-amz-mfa",
        "x-amz-request-payer",
        "x-amz-bypass-governance-retention",
        "if-match",
        "if-none-match",
    ]
    .iter()
    .any(|name| request.headers.contains_key(*name))
    {
        return Err(s3_error!(NotImplemented));
    }

    // Ordinary current SDKs send CRC32 instead of Content-MD5. Require an
    // asserted digest, then verify every supplied assertion and terminal body.
    if ![
        "content-md5",
        "x-amz-checksum-crc32",
        "x-amz-checksum-crc32c",
        "x-amz-checksum-crc64nvme",
        "x-amz-checksum-sha1",
        "x-amz-checksum-sha256",
        "x-amz-trailer",
    ]
    .iter()
    .any(|name| request.headers.contains_key(*name))
    {
        return Err(s3_error!(InvalidRequest));
    }

    Integrity::envelope(&request)?;

    let input = request.input;

    if !(1..=1000).contains(&input.delete.objects.len()) {
        return Err(s3_error!(InvalidArgument));
    }

    let mut objects = Vec::with_capacity(input.delete.objects.len());

    for object in input.delete.objects {
        if object.version_id.is_some()
            || object.size.is_some()
            || object.last_modified_time.is_some()
        {
            return Err(s3_error!(NotImplemented));
        }

        let etag = match object.e_tag {
            None => None,
            Some(ETag::Strong(value)) if value == "*" => Some(value),
            Some(ETag::Strong(value)) => Some(format!("\"{value}\"")),
            Some(ETag::Weak(_)) => return Err(s3_error!(InvalidArgument)),
        };

        let object = ObjectIdentifier {
            key: ObjectKey::parse(&object.key).map_err(|_| s3_error!(InvalidArgument))?,
            etag,
        };

        object.validate().map_err(|_| s3_error!(InvalidArgument))?;
        objects.push(object);
    }

    let batch = DeleteObjectsRequest {
        bucket: BucketName::parse(&input.bucket).map_err(|_| s3_error!(InvalidBucketName))?,
        expected_owner: input.expected_bucket_owner,
        objects,
    };

    let result = storage
        .0
        .delete_objects_http(identity, batch)
        .await
        .map_err(|failure| error::operation(&failure.error))?;

    let mut output = DeleteObjectsOutput::default();
    let mut deleted = Vec::new();
    let mut errors = Vec::new();

    for item in result {
        if let Some(failure) = item.failure {
            let wire = error::failure(failure);

            errors.push(Error {
                key: Some(item.key.as_str().to_owned()),
                code: Some(wire.code().as_str().to_owned()),
                message: Some(failure.message().to_owned()),
                version_id: None,
            });
        } else if !input.delete.quiet.unwrap_or(false) {
            deleted.push(DeletedObject {
                key: Some(item.key.as_str().to_owned()),
                ..DeletedObject::default()
            });
        }
    }

    output.deleted = (!deleted.is_empty()).then_some(deleted);
    output.errors = (!errors.is_empty()).then_some(errors);
    Ok(S3Response::new(output))
}
