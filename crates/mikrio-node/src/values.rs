//! Owned primitive input/output projections. Semantic checks use facade builders.

use napi::bindgen_prelude::{BigInt, Utf16String};
use napi::{Env, Result};
use napi_derive::napi;

use crate::{host_error, native_error, try_to_utf8};

#[napi(object)]
pub struct NativePair {
    pub key: Utf16String,
    pub value: Utf16String,
}

#[napi(object, object_from_js = false)]
pub struct NativeMetadata {
    pub key: String,
    pub value: String,
}

#[napi(object)]
pub struct NativeBucketInput {
    pub bucket: Utf16String,
    pub expected_bucket_owner: Option<Utf16String>,
}

#[napi(object)]
pub struct NativePutInput {
    pub bucket: Utf16String,
    pub key: Utf16String,
    pub expected_bucket_owner: Option<Utf16String>,
    pub content_type: Option<Utf16String>,
    pub content_encoding: Option<Utf16String>,
    pub content_disposition: Option<Utf16String>,
    pub content_language: Option<Utf16String>,
    pub cache_control: Option<Utf16String>,
    pub checksum_algorithm: Option<Utf16String>,
    pub content_md5: Option<Utf16String>,
    pub checksum_crc32: Option<Utf16String>,
    #[napi(js_name = "checksumCrc32c")]
    pub checksum_crc32c: Option<Utf16String>,
    pub checksum_sha1: Option<Utf16String>,
    pub checksum_sha256: Option<Utf16String>,
    pub checksum_crc64_nvme: Option<Utf16String>,
    pub if_match: Option<Utf16String>,
    pub if_none_match: Option<Utf16String>,
    pub content_length: Option<BigInt>,
    pub expires: Option<i64>,
    pub metadata: Option<Vec<NativePair>>,
    pub tags: Option<Vec<NativePair>>,
}

impl NativePutInput {
    pub(super) fn try_into_input(
        self,
        env: Env,
        source: mikrio::BodySource,
    ) -> Result<mikrio::PutObjectInput> {
        let mut builder = mikrio::PutObjectInput::builder(
            try_to_string(env, &self.bucket)?,
            try_to_string(env, &self.key)?,
            source,
        );

        if let Some(value) = self.expected_bucket_owner {
            builder = builder.expected_bucket_owner(try_to_string(env, &value)?);
        }

        if let Some(value) = self.content_type {
            builder = builder.content_type(try_to_string(env, &value)?);
        }

        if let Some(value) = self.content_encoding {
            builder = builder.content_encoding(try_to_string(env, &value)?);
        }

        if let Some(value) = self.content_disposition {
            builder = builder.content_disposition(try_to_string(env, &value)?);
        }

        if let Some(value) = self.content_language {
            builder = builder.content_language(try_to_string(env, &value)?);
        }

        if let Some(value) = self.cache_control {
            builder = builder.cache_control(try_to_string(env, &value)?);
        }

        if let Some(value) = self.checksum_algorithm {
            builder = builder.checksum_algorithm(try_to_checksum_algorithm(env, &value)?);
        }

        if let Some(value) = self.content_md5 {
            builder = builder.content_md5(try_to_string(env, &value)?);
        }

        if let Some(value) = self.checksum_crc32 {
            builder = builder.checksum_crc32(try_to_string(env, &value)?);
        }

        if let Some(value) = self.checksum_crc32c {
            builder = builder.checksum_crc32c(try_to_string(env, &value)?);
        }

        if let Some(value) = self.checksum_sha1 {
            builder = builder.checksum_sha1(try_to_string(env, &value)?);
        }

        if let Some(value) = self.checksum_sha256 {
            builder = builder.checksum_sha256(try_to_string(env, &value)?);
        }

        if let Some(value) = self.checksum_crc64_nvme {
            builder = builder.checksum_crc64_nvme(try_to_string(env, &value)?);
        }

        if let Some(value) = self.if_match {
            builder = builder.if_match(try_to_string(env, &value)?);
        }

        if let Some(value) = self.if_none_match {
            builder = builder.if_none_match(try_to_string(env, &value)?);
        }

        if let Some(value) = self.content_length {
            builder = builder.content_length(try_to_content_length(env, &value)?);
        }

        if let Some(value) = self.expires {
            builder = builder.expires(try_to_timestamp(env, value)?);
        }

        if let Some(entries) = self.metadata {
            builder = builder.metadata(
                entries
                    .into_iter()
                    .map(|entry| {
                        Ok((
                            try_to_string(env, &entry.key)?,
                            try_to_string(env, &entry.value)?,
                        ))
                    })
                    .collect::<Result<_>>()?,
            );
        }

        if let Some(entries) = self.tags {
            builder = builder.tags(
                entries
                    .into_iter()
                    .map(|entry| {
                        Ok(mikrio::Tag {
                            key: try_to_string(env, &entry.key)?,
                            value: try_to_string(env, &entry.value)?,
                        })
                    })
                    .collect::<Result<_>>()?,
            );
        }

        builder.build().map_err(|error| native_error(env, &error))
    }
}

#[napi(object)]
pub struct NativeReadInput {
    pub bucket: Utf16String,
    pub key: Utf16String,
    pub expected_bucket_owner: Option<Utf16String>,
    pub checksum_mode: Option<Utf16String>,
    pub range: Option<Utf16String>,
    pub part_number: Option<f64>,
    pub if_match: Option<Utf16String>,
    pub if_none_match: Option<Utf16String>,
    pub if_modified_since: Option<i64>,
    pub if_unmodified_since: Option<i64>,
    pub response_content_type: Option<Utf16String>,
    pub response_content_encoding: Option<Utf16String>,
    pub response_content_disposition: Option<Utf16String>,
    pub response_content_language: Option<Utf16String>,
    pub response_cache_control: Option<Utf16String>,
    pub response_expires: Option<i64>,
}

impl NativeReadInput {
    pub(super) fn try_into_get_input(self, env: Env) -> Result<mikrio::GetObjectInput> {
        let mut builder = mikrio::GetObjectInput::builder(
            try_to_string(env, &self.bucket)?,
            try_to_string(env, &self.key)?,
        );

        if let Some(value) = self.expected_bucket_owner {
            builder = builder.expected_bucket_owner(try_to_string(env, &value)?);
        }

        if let Some(value) = self.range {
            builder = builder.range(try_to_string(env, &value)?);
        }

        if let Some(value) = self.part_number {
            builder = builder.part_number(try_to_count(env, value)?);
        }

        if let Some(value) = self.response_content_type {
            builder = builder.response_content_type(try_to_string(env, &value)?);
        }

        if let Some(value) = self.response_content_encoding {
            builder = builder.response_content_encoding(try_to_string(env, &value)?);
        }

        if let Some(value) = self.response_content_disposition {
            builder = builder.response_content_disposition(try_to_string(env, &value)?);
        }

        if let Some(value) = self.response_content_language {
            builder = builder.response_content_language(try_to_string(env, &value)?);
        }

        if let Some(value) = self.response_cache_control {
            builder = builder.response_cache_control(try_to_string(env, &value)?);
        }

        if let Some(value) = self.response_expires {
            builder = builder.response_expires(try_to_timestamp(env, value)?);
        }

        if let Some(value) = self.if_match {
            builder = builder.if_match(try_to_string(env, &value)?);
        }

        if let Some(value) = self.if_none_match {
            builder = builder.if_none_match(try_to_string(env, &value)?);
        }

        if let Some(value) = self.if_modified_since {
            builder = builder.if_modified_since(try_to_timestamp(env, value)?);
        }

        if let Some(value) = self.if_unmodified_since {
            builder = builder.if_unmodified_since(try_to_timestamp(env, value)?);
        }

        if let Some(value) = self.checksum_mode {
            builder = builder.checksum_mode(try_to_checksum_mode(env, &value)?);
        }

        builder.build().map_err(|error| native_error(env, &error))
    }

    pub(super) fn try_into_head_input(self, env: Env) -> Result<mikrio::HeadObjectInput> {
        let mut builder = mikrio::HeadObjectInput::builder(
            try_to_string(env, &self.bucket)?,
            try_to_string(env, &self.key)?,
        );

        if let Some(value) = self.expected_bucket_owner {
            builder = builder.expected_bucket_owner(try_to_string(env, &value)?);
        }

        if let Some(value) = self.range {
            builder = builder.range(try_to_string(env, &value)?);
        }

        if let Some(value) = self.part_number {
            builder = builder.part_number(try_to_count(env, value)?);
        }

        if let Some(value) = self.response_content_type {
            builder = builder.response_content_type(try_to_string(env, &value)?);
        }

        if let Some(value) = self.response_content_encoding {
            builder = builder.response_content_encoding(try_to_string(env, &value)?);
        }

        if let Some(value) = self.response_content_disposition {
            builder = builder.response_content_disposition(try_to_string(env, &value)?);
        }

        if let Some(value) = self.response_content_language {
            builder = builder.response_content_language(try_to_string(env, &value)?);
        }

        if let Some(value) = self.response_cache_control {
            builder = builder.response_cache_control(try_to_string(env, &value)?);
        }

        if let Some(value) = self.response_expires {
            builder = builder.response_expires(try_to_timestamp(env, value)?);
        }

        if let Some(value) = self.if_match {
            builder = builder.if_match(try_to_string(env, &value)?);
        }

        if let Some(value) = self.if_none_match {
            builder = builder.if_none_match(try_to_string(env, &value)?);
        }

        if let Some(value) = self.if_modified_since {
            builder = builder.if_modified_since(try_to_timestamp(env, value)?);
        }

        if let Some(value) = self.if_unmodified_since {
            builder = builder.if_unmodified_since(try_to_timestamp(env, value)?);
        }

        if let Some(value) = self.checksum_mode {
            builder = builder.checksum_mode(try_to_checksum_mode(env, &value)?);
        }

        builder.build().map_err(|error| native_error(env, &error))
    }
}

#[napi(object)]
pub struct NativeDeleteInput {
    pub bucket: Utf16String,
    pub key: Utf16String,
    pub expected_bucket_owner: Option<Utf16String>,
    pub if_match: Option<Utf16String>,
}

#[napi(object, object_from_js = false)]
pub struct NativePutOutput {
    pub etag: String,
    pub checksum_crc32: Option<String>,
    #[napi(js_name = "checksumCrc32c")]
    pub checksum_crc32c: Option<String>,
    pub checksum_sha1: Option<String>,
    pub checksum_sha256: Option<String>,
    pub checksum_crc64_nvme: Option<String>,
}

impl From<mikrio::PutObjectOutput> for NativePutOutput {
    fn from(output: mikrio::PutObjectOutput) -> Self {
        Self {
            etag: output.etag().to_owned(),
            checksum_crc32: output.checksum_crc32().map(str::to_owned),
            checksum_crc32c: output.checksum_crc32c().map(str::to_owned),
            checksum_sha1: output.checksum_sha1().map(str::to_owned),
            checksum_sha256: output.checksum_sha256().map(str::to_owned),
            checksum_crc64_nvme: output.checksum_crc64_nvme().map(str::to_owned),
        }
    }
}

#[napi(object, object_from_js = false)]
pub struct NativeReadOutput {
    pub content_length: BigInt,
    pub etag: String,
    pub last_modified: i64,
    pub metadata: Vec<NativeMetadata>,
    pub tag_count: u32,
    pub content_type: Option<String>,
    pub content_encoding: Option<String>,
    pub content_disposition: Option<String>,
    pub content_language: Option<String>,
    pub cache_control: Option<String>,
    pub content_range: Option<String>,
    pub checksum_crc32: Option<String>,
    #[napi(js_name = "checksumCrc32c")]
    pub checksum_crc32c: Option<String>,
    pub checksum_sha1: Option<String>,
    pub checksum_sha256: Option<String>,
    pub checksum_crc64_nvme: Option<String>,
    pub expires: Option<i64>,
    pub parts_count: Option<u32>,
    pub checksum_type: Option<String>,
}

impl From<&mikrio::GetObjectOutput> for NativeReadOutput {
    fn from(output: &mikrio::GetObjectOutput) -> Self {
        Self {
            content_length: output.content_length().into(),
            etag: output.etag().to_owned(),
            last_modified: output.last_modified().as_unix_millis(),
            metadata: output
                .metadata()
                .iter()
                .map(|(key, value)| NativeMetadata {
                    key: key.clone(),
                    value: value.clone(),
                })
                .collect(),
            tag_count: output.tag_count(),
            content_type: output.content_type().map(str::to_owned),
            content_encoding: output.content_encoding().map(str::to_owned),
            content_disposition: output.content_disposition().map(str::to_owned),
            content_language: output.content_language().map(str::to_owned),
            cache_control: output.cache_control().map(str::to_owned),
            content_range: output.content_range().map(str::to_owned),
            checksum_crc32: output.checksum_crc32().map(str::to_owned),
            checksum_crc32c: output.checksum_crc32c().map(str::to_owned),
            checksum_sha1: output.checksum_sha1().map(str::to_owned),
            checksum_sha256: output.checksum_sha256().map(str::to_owned),
            checksum_crc64_nvme: output.checksum_crc64_nvme().map(str::to_owned),
            expires: output.expires().map(mikrio::Timestamp::as_unix_millis),
            parts_count: output.parts_count(),
            checksum_type: output
                .checksum_type()
                .map(|value| value.as_str().to_owned()),
        }
    }
}

impl From<&mikrio::HeadObjectOutput> for NativeReadOutput {
    fn from(output: &mikrio::HeadObjectOutput) -> Self {
        Self {
            content_length: output.content_length().into(),
            etag: output.etag().to_owned(),
            last_modified: output.last_modified().as_unix_millis(),
            metadata: output
                .metadata()
                .iter()
                .map(|(key, value)| NativeMetadata {
                    key: key.clone(),
                    value: value.clone(),
                })
                .collect(),
            tag_count: output.tag_count(),
            content_type: output.content_type().map(str::to_owned),
            content_encoding: output.content_encoding().map(str::to_owned),
            content_disposition: output.content_disposition().map(str::to_owned),
            content_language: output.content_language().map(str::to_owned),
            cache_control: output.cache_control().map(str::to_owned),
            content_range: output.content_range().map(str::to_owned),
            checksum_crc32: output.checksum_crc32().map(str::to_owned),
            checksum_crc32c: output.checksum_crc32c().map(str::to_owned),
            checksum_sha1: output.checksum_sha1().map(str::to_owned),
            checksum_sha256: output.checksum_sha256().map(str::to_owned),
            checksum_crc64_nvme: output.checksum_crc64_nvme().map(str::to_owned),
            expires: output.expires().map(mikrio::Timestamp::as_unix_millis),
            parts_count: output.parts_count(),
            checksum_type: output
                .checksum_type()
                .map(|value| value.as_str().to_owned()),
        }
    }
}

pub(super) fn try_to_string(env: Env, value: &[u16]) -> Result<String> {
    try_to_utf8(env, value, "string must contain Unicode scalar values")
}

pub(crate) fn try_to_timestamp(env: Env, value: i64) -> Result<mikrio::Timestamp> {
    mikrio::Timestamp::from_unix_millis(value).map_err(|error| native_error(env, &error))
}

pub(crate) fn try_to_checksum_algorithm(
    env: Env,
    value: &[u16],
) -> Result<mikrio::ChecksumAlgorithm> {
    match try_to_string(env, value)?.as_str() {
        "CRC32" => Ok(mikrio::ChecksumAlgorithm::Crc32),
        "CRC32C" => Ok(mikrio::ChecksumAlgorithm::Crc32c),
        "CRC64NVME" => Ok(mikrio::ChecksumAlgorithm::Crc64Nvme),
        "SHA1" => Ok(mikrio::ChecksumAlgorithm::Sha1),
        "SHA256" => Ok(mikrio::ChecksumAlgorithm::Sha256),
        _ => Err(host_error(
            env,
            "InvalidArgument",
            "unsupported checksum algorithm",
        )),
    }
}

fn try_to_checksum_mode(env: Env, value: &[u16]) -> Result<mikrio::ChecksumMode> {
    if try_to_string(env, value)? != "ENABLED" {
        return Err(host_error(
            env,
            "InvalidArgument",
            "unsupported checksum mode",
        ));
    }

    Ok(mikrio::ChecksumMode::Enabled)
}

pub(crate) fn try_to_count(env: Env, value: f64) -> Result<u32> {
    value
        .to_string()
        .parse()
        .map_err(|_| host_error(env, "InvalidArgument", "expected an unsigned integer"))
}

pub(crate) fn try_to_content_length(env: Env, value: &BigInt) -> Result<u64> {
    let (negative, value, lossless) = value.get_u64();

    if negative || !lossless {
        return Err(host_error(
            env,
            "InvalidArgument",
            "contentLength must fit an unsigned 64-bit integer",
        ));
    }

    Ok(value)
}
