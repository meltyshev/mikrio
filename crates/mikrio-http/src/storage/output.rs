//! Wire projections of the already authorized, detached storage snapshot.

use mikrio_core::integrity::{ChecksumAlgorithm, ChecksumType, PayloadFacts};
use mikrio_core::object::StoredObject;
use mikrio_core::{CacheValidators, EpochMillis};
use s3s::dto::{
    GetObjectOutput, HeadObjectOutput, PutObjectOutput, StreamingBlob, Timestamp, TimestampFormat,
};
use s3s::{S3Error, S3Result, s3_error};

pub(super) fn timestamp(value: EpochMillis) -> S3Result<Timestamp> {
    time::OffsetDateTime::from_unix_timestamp_nanos(i128::from(value.get()) * 1_000_000)
        .map(Timestamp::from)
        .map_err(|_| s3_error!(InternalError))
}

fn expires(value: EpochMillis) -> S3Result<String> {
    let mut bytes = Vec::new();

    timestamp(value)?
        .format(TimestampFormat::HttpDate, &mut bytes)
        .map_err(|_| s3_error!(InternalError))?;

    String::from_utf8(bytes).map_err(|_| s3_error!(InternalError))
}

pub(super) fn not_modified(validators: &CacheValidators) -> S3Result<S3Error> {
    let mut headers = http::HeaderMap::new();

    headers.insert(
        http::header::ETAG,
        validators
            .etag
            .parse()
            .map_err(|_| s3_error!(InternalError))?,
    );

    if let Some(value) = &validators.cache_control {
        headers.insert(
            http::header::CACHE_CONTROL,
            value.parse().map_err(|_| s3_error!(InternalError))?,
        );
    }

    if let Some(value) = validators.expires {
        headers.insert(
            http::header::EXPIRES,
            expires(value)?
                .parse()
                .map_err(|_| s3_error!(InternalError))?,
        );
    }

    let mut error = s3_error!(NotModified);
    error.set_headers(headers);
    Ok(error)
}

pub(super) fn head(selected: StoredObject, checksums: bool) -> S3Result<HeadObjectOutput> {
    let representation = selected.representation.fields();

    let mut output = HeadObjectOutput {
        content_length: Some(
            i64::try_from(selected.content_length()).map_err(|_| s3_error!(InternalError))?,
        ),
        content_range: selected.content_range(),
        parts_count: selected
            .parts_count()
            .map(i32::try_from)
            .transpose()
            .map_err(|_| s3_error!(InternalError))?,
        accept_ranges: Some("bytes".into()),
        e_tag: Some(
            selected
                .etag
                .parse()
                .map_err(|_| s3_error!(InternalError))?,
        ),
        last_modified: Some(timestamp(selected.last_modified)?),
        content_type: Some(selected.representation.effective_content_type().to_owned()),
        content_encoding: representation.content_encoding.clone(),
        content_disposition: representation.content_disposition.clone(),
        content_language: representation.content_language.clone(),
        cache_control: representation.cache_control.clone(),
        expires: representation.expires.map(expires).transpose()?,
        metadata: Some(selected.metadata.into_entries().into_iter().collect()),
        tag_count: selected
            .tag_count
            .filter(|count| *count != 0)
            .map(i32::try_from)
            .transpose()
            .map_err(|_| s3_error!(InternalError))?,
        ..HeadObjectOutput::default()
    };

    if checksums {
        output.checksum_crc32 = selected.checksums.get("CRC32").cloned();
        output.checksum_crc32c = selected.checksums.get("CRC32C").cloned();
        output.checksum_crc64nvme = selected.checksums.get("CRC64NVME").cloned();
        output.checksum_sha1 = selected.checksums.get("SHA1").cloned();
        output.checksum_sha256 = selected.checksums.get("SHA256").cloned();

        output.checksum_type = selected.checksum_type.map(|mode| match mode {
            ChecksumType::FullObject => s3s::dto::ChecksumType::from_static("FULL_OBJECT"),
            ChecksumType::Composite => s3s::dto::ChecksumType::from_static("COMPOSITE"),
        });
    }

    Ok(output)
}

pub(super) fn get(metadata: HeadObjectOutput, body: StreamingBlob) -> GetObjectOutput {
    GetObjectOutput {
        body: Some(body),
        content_length: metadata.content_length,
        content_range: metadata.content_range,
        parts_count: metadata.parts_count,
        accept_ranges: metadata.accept_ranges,
        e_tag: metadata.e_tag,
        last_modified: metadata.last_modified,
        content_type: metadata.content_type,
        content_encoding: metadata.content_encoding,
        content_disposition: metadata.content_disposition,
        content_language: metadata.content_language,
        cache_control: metadata.cache_control,
        expires: metadata.expires,
        metadata: metadata.metadata,
        tag_count: metadata.tag_count,
        checksum_crc32: metadata.checksum_crc32,
        checksum_crc32c: metadata.checksum_crc32c,
        checksum_crc64nvme: metadata.checksum_crc64nvme,
        checksum_sha1: metadata.checksum_sha1,
        checksum_sha256: metadata.checksum_sha256,
        checksum_type: metadata.checksum_type,
        ..GetObjectOutput::default()
    }
}

pub(super) fn put(facts: &PayloadFacts) -> S3Result<PutObjectOutput> {
    let mut output = PutObjectOutput {
        e_tag: Some(facts.etag().parse().map_err(|_| s3_error!(InternalError))?),
        checksum_type: Some(s3s::dto::ChecksumType::from_static("FULL_OBJECT")),
        ..PutObjectOutput::default()
    };

    let checksum = Some(facts.checksum().encode());

    match facts.checksum().algorithm() {
        ChecksumAlgorithm::Crc32 => output.checksum_crc32 = checksum,
        ChecksumAlgorithm::Crc32c => output.checksum_crc32c = checksum,
        ChecksumAlgorithm::Crc64Nvme => output.checksum_crc64nvme = checksum,
        ChecksumAlgorithm::Sha1 => output.checksum_sha1 = checksum,
        ChecksumAlgorithm::Sha256 => output.checksum_sha256 = checksum,
    }

    Ok(output)
}
