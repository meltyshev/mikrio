//! Authenticated COPY uses the guarded core operation and its committed facts.

use mikrio_core::integrity::{ChecksumAlgorithm, ExpectedDigests, PutIntegrity, PutIntegrityInput};
use mikrio_core::object::{
    BucketName, CopyDirective, CopyRequest, ObjectKey, ObjectRef, ReadConditions,
    ReadConditionsInput, UserMetadata, WriteConditions,
};
use s3s::dto::{CopyObjectInput, CopyObjectOutput, CopyObjectResult, CopySource};
use s3s::{S3Request, S3Response, S3Result, s3_error};

use super::{Storage, error, identity, input, output, progress};

pub(super) async fn copy(
    storage: &Storage,
    request: S3Request<CopyObjectInput>,
) -> S3Result<S3Response<CopyObjectOutput>> {
    let identity = identity(&request)?;
    let input = convert(&request)?;
    let connection = storage.0.clone();
    let progress = mikrio_core::WorkProgress::default();
    let worker_progress = progress.clone();

    let work = async move {
        let result = connection
            .copy_object_http_with_progress(identity, input, worker_progress)
            .await
            .map_err(|error| error::operation(&error))?;

        let facts = output::put(&result.facts)?;

        Ok(CopyObjectOutput {
            copy_object_result: Some(CopyObjectResult {
                e_tag: facts.e_tag,
                last_modified: Some(output::timestamp(result.last_modified)?),
                checksum_crc32: facts.checksum_crc32,
                checksum_crc32c: facts.checksum_crc32c,
                checksum_crc64nvme: facts.checksum_crc64nvme,
                checksum_sha1: facts.checksum_sha1,
                checksum_sha256: facts.checksum_sha256,
                checksum_type: facts.checksum_type,
                ..CopyObjectResult::default()
            }),
            ..CopyObjectOutput::default()
        })
    };

    let output = match progress::admit(work, &progress).await? {
        progress::Admitted::Complete(output) => output,
        progress::Admitted::Running(future) => CopyObjectOutput {
            future: Some(future),
            ..CopyObjectOutput::default()
        },
    };

    Ok(S3Response::new(output))
}

fn directive(headers: &http::HeaderMap, name: &str) -> S3Result<CopyDirective> {
    match input::header(headers, name)?.as_deref() {
        None | Some("COPY") => Ok(CopyDirective::Copy),
        Some("REPLACE") => Ok(CopyDirective::Replace),
        Some(_) => Err(s3_error!(InvalidArgument)),
    }
}

pub(super) fn source(source: &CopySource) -> S3Result<ObjectRef> {
    let CopySource::Bucket {
        bucket,
        key,
        version_id,
    } = source
    else {
        return Err(s3_error!(NotImplemented));
    };

    if version_id.is_some() {
        return Err(s3_error!(NotImplemented));
    }

    Ok(ObjectRef {
        bucket: BucketName::parse(bucket).map_err(|_| s3_error!(InvalidBucketName))?,
        key: ObjectKey::parse(key).map_err(|_| s3_error!(InvalidArgument))?,
    })
}

pub(super) fn source_owner(headers: &http::HeaderMap) -> S3Result<Option<String>> {
    let owner = input::header(headers, "x-amz-source-expected-bucket-owner")?;

    if let Some(owner) = &owner {
        mikrio_core::access::validate_account_id(owner).map_err(|_| s3_error!(InvalidArgument))?;
    }

    Ok(owner)
}

pub(super) fn source_conditions(headers: &http::HeaderMap) -> S3Result<ReadConditions> {
    ReadConditions::new(ReadConditionsInput {
        if_match: input::header(headers, "x-amz-copy-source-if-match")?,
        if_none_match: input::header(headers, "x-amz-copy-source-if-none-match")?,
        if_modified_since: crate::access::parse_expires(
            input::header(headers, "x-amz-copy-source-if-modified-since")?.as_deref(),
        )?,
        if_unmodified_since: crate::access::parse_expires(
            input::header(headers, "x-amz-copy-source-if-unmodified-since")?.as_deref(),
        )?,
    })
    .map_err(|_| s3_error!(InvalidArgument))
}

fn convert(request: &S3Request<CopyObjectInput>) -> S3Result<CopyRequest> {
    let headers = &request.headers;
    input::write_options(headers)?;

    if input::has_any(
        headers,
        &[
            "x-amz-copy-source-server-side-encryption-customer-algorithm",
            "x-amz-copy-source-server-side-encryption-customer-key",
            "x-amz-copy-source-server-side-encryption-customer-key-md5",
            "x-amz-copy-source-range",
        ],
    ) {
        return Err(s3_error!(NotImplemented));
    }

    // COPY's algorithm selects destination integrity; it is not a checksum of
    // the empty request envelope. Never silently accept payload assertions.
    if headers.keys().any(|name| {
        name.as_str().starts_with("x-amz-checksum-") && name != "x-amz-checksum-algorithm"
    }) {
        return Err(s3_error!(NotImplemented));
    }

    if input::has_any(headers, &["x-amz-sdk-checksum-algorithm", "x-amz-trailer"]) {
        return Err(s3_error!(NotImplemented));
    }

    let envelope = PutIntegrity::new(PutIntegrityInput {
        content_md5: input::header(headers, "content-md5")?,
        ..PutIntegrityInput::default()
    })
    .map_err(error::integrity_error)?;

    envelope
        .verifier(Some(0))
        .and_then(|verifier| verifier.finish(ExpectedDigests::default()))
        .map_err(error::integrity_error)?;

    for name in [
        "x-amz-copy-source-if-modified-since",
        "x-amz-copy-source-if-unmodified-since",
    ] {
        if headers.get(name).is_some_and(http::HeaderValue::is_empty) {
            return Err(s3_error!(InvalidArgument));
        }
    }

    let input = &request.input;

    Ok(CopyRequest {
        bucket: BucketName::parse(&input.bucket).map_err(|_| s3_error!(InvalidBucketName))?,
        key: ObjectKey::parse(&input.key).map_err(|_| s3_error!(InvalidArgument))?,
        source: source(&input.copy_source)?,
        expected_owner: crate::access::expected_owner(headers)?.map(str::to_owned),
        expected_source_owner: source_owner(headers)?,
        source_conditions: source_conditions(headers)?,
        conditions: WriteConditions::new(
            input::header(headers, "if-match")?,
            input::header(headers, "if-none-match")?,
        )
        .map_err(|_| s3_error!(InvalidArgument))?,
        metadata_directive: directive(headers, "x-amz-metadata-directive")?,
        representation: input::representation(headers)?,
        metadata: UserMetadata::new(
            input
                .metadata
                .iter()
                .flat_map(|values| values.iter())
                .map(|(key, value)| (key.clone(), value.clone())),
        )
        .map_err(|_| s3_error!(InvalidArgument))?,
        tagging_directive: directive(headers, "x-amz-tagging-directive")?,
        tags: input::header(headers, "x-amz-tagging")?
            .as_deref()
            .map(super::tagging::inline)
            .transpose()?,
        checksum_algorithm: input::header(headers, "x-amz-checksum-algorithm")?
            .as_deref()
            .map(ChecksumAlgorithm::parse)
            .transpose()
            .map_err(error::integrity_error)?,
    })
}
