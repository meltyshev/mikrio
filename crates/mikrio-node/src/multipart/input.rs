//! Checked owned host values; shared semantic validation belongs to facade builders.

use napi::bindgen_prelude::{BigInt, Utf16String};
use napi::{Env, Result};
use napi_derive::napi;

use crate::values::{
    NativePair, try_to_checksum_algorithm, try_to_content_length, try_to_count, try_to_string,
    try_to_timestamp,
};
use crate::{host_error, native_error};

#[napi(object)]
pub struct NativeObjectRef {
    pub bucket: Utf16String,
    pub key: Utf16String,
}

impl NativeObjectRef {
    fn try_into_object_ref(self, env: Env) -> Result<mikrio::ObjectRef> {
        mikrio::ObjectRef::new(
            &try_to_string(env, &self.bucket)?,
            &try_to_string(env, &self.key)?,
        )
        .map_err(|error| native_error(env, &error))
    }
}

#[napi(object)]
pub struct NativeCompletedPart {
    pub part_number: f64,
    pub etag: Utf16String,
    pub checksum_crc32: Option<Utf16String>,
    #[napi(js_name = "checksumCrc32c")]
    pub checksum_crc32c: Option<Utf16String>,
    pub checksum_sha1: Option<Utf16String>,
    pub checksum_sha256: Option<Utf16String>,
    pub checksum_crc64_nvme: Option<Utf16String>,
}

impl NativeCompletedPart {
    fn try_into_part(self, env: Env) -> Result<mikrio::CompletedPart> {
        let mut builder = mikrio::CompletedPart::builder(
            try_to_count(env, self.part_number)?,
            try_to_string(env, &self.etag)?,
        );

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

        builder.build().map_err(|error| native_error(env, &error))
    }
}

#[napi(object)]
pub struct NativeCopyObjectInput {
    pub bucket: Utf16String,
    pub key: Utf16String,
    pub copy_source: NativeObjectRef,
    pub expected_bucket_owner: Option<Utf16String>,
    pub expected_source_bucket_owner: Option<Utf16String>,
    pub copy_source_if_match: Option<Utf16String>,
    pub copy_source_if_none_match: Option<Utf16String>,
    pub copy_source_if_modified_since: Option<i64>,
    pub copy_source_if_unmodified_since: Option<i64>,
    pub metadata_directive: Option<Utf16String>,
    pub content_type: Option<Utf16String>,
    pub content_encoding: Option<Utf16String>,
    pub content_disposition: Option<Utf16String>,
    pub content_language: Option<Utf16String>,
    pub cache_control: Option<Utf16String>,
    pub expires: Option<i64>,
    pub metadata: Option<Vec<NativePair>>,
    pub tags: Option<Vec<NativePair>>,
    pub tagging_directive: Option<Utf16String>,
    pub checksum_algorithm: Option<Utf16String>,
    pub if_match: Option<Utf16String>,
    pub if_none_match: Option<Utf16String>,
}

impl NativeCopyObjectInput {
    pub(super) fn try_into_input(self, env: Env) -> Result<mikrio::CopyObjectInput> {
        let mut builder = mikrio::CopyObjectInput::builder(
            try_to_string(env, &self.bucket)?,
            try_to_string(env, &self.key)?,
            self.copy_source.try_into_object_ref(env)?,
        );

        if let Some(value) = self.expected_bucket_owner {
            builder = builder.expected_bucket_owner(try_to_string(env, &value)?);
        }

        if let Some(value) = self.expected_source_bucket_owner {
            builder = builder.expected_source_bucket_owner(try_to_string(env, &value)?);
        }

        if let Some(value) = self.copy_source_if_match {
            builder = builder.copy_source_if_match(try_to_string(env, &value)?);
        }

        if let Some(value) = self.copy_source_if_none_match {
            builder = builder.copy_source_if_none_match(try_to_string(env, &value)?);
        }

        if let Some(value) = self.copy_source_if_modified_since {
            builder = builder.copy_source_if_modified_since(try_to_timestamp(env, value)?);
        }

        if let Some(value) = self.copy_source_if_unmodified_since {
            builder = builder.copy_source_if_unmodified_since(try_to_timestamp(env, value)?);
        }

        if let Some(value) = self.metadata_directive {
            builder = builder.metadata_directive(try_to_copy_directive(env, &value)?);
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
                    .collect::<Result<Vec<_>>>()?,
            );
        }

        if let Some(tags) = self.tags {
            builder = builder.tags(
                tags.into_iter()
                    .map(|entry| {
                        Ok(mikrio::Tag {
                            key: try_to_string(env, &entry.key)?,
                            value: try_to_string(env, &entry.value)?,
                        })
                    })
                    .collect::<Result<Vec<_>>>()?,
            );
        }

        if let Some(value) = self.tagging_directive {
            builder = builder.tagging_directive(try_to_copy_directive(env, &value)?);
        }

        if let Some(value) = self.checksum_algorithm {
            builder = builder.checksum_algorithm(try_to_checksum_algorithm(env, &value)?);
        }

        if let Some(value) = self.if_match {
            builder = builder.if_match(try_to_string(env, &value)?);
        }

        if let Some(value) = self.if_none_match {
            builder = builder.if_none_match(try_to_string(env, &value)?);
        }

        builder.build().map_err(|error| native_error(env, &error))
    }
}

#[napi(object)]
pub struct NativeCreateMultipartUploadInput {
    pub bucket: Utf16String,
    pub key: Utf16String,
    pub expected_bucket_owner: Option<Utf16String>,
    pub content_type: Option<Utf16String>,
    pub content_encoding: Option<Utf16String>,
    pub content_disposition: Option<Utf16String>,
    pub content_language: Option<Utf16String>,
    pub cache_control: Option<Utf16String>,
    pub expires: Option<i64>,
    pub metadata: Option<Vec<NativePair>>,
    pub tags: Option<Vec<NativePair>>,
    pub checksum_algorithm: Option<Utf16String>,
    pub checksum_type: Option<Utf16String>,
}

impl NativeCreateMultipartUploadInput {
    pub(super) fn try_into_input(self, env: Env) -> Result<mikrio::CreateMultipartUploadInput> {
        let mut builder = mikrio::CreateMultipartUploadInput::builder(
            try_to_string(env, &self.bucket)?,
            try_to_string(env, &self.key)?,
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
                    .collect::<Result<Vec<_>>>()?,
            );
        }

        if let Some(tags) = self.tags {
            builder = builder.tags(
                tags.into_iter()
                    .map(|entry| {
                        Ok(mikrio::Tag {
                            key: try_to_string(env, &entry.key)?,
                            value: try_to_string(env, &entry.value)?,
                        })
                    })
                    .collect::<Result<Vec<_>>>()?,
            );
        }

        if let Some(value) = self.checksum_algorithm {
            builder = builder.checksum_algorithm(try_to_checksum_algorithm(env, &value)?);
        }

        if let Some(value) = self.checksum_type {
            builder = builder.checksum_type(try_to_checksum_type(env, &value)?);
        }

        builder.build().map_err(|error| native_error(env, &error))
    }
}

#[napi(object)]
pub struct NativeUploadPartInput {
    pub bucket: Utf16String,
    pub key: Utf16String,
    pub upload_id: Utf16String,
    pub part_number: f64,
    pub expected_bucket_owner: Option<Utf16String>,
    pub content_length: Option<BigInt>,
    pub checksum_algorithm: Option<Utf16String>,
    pub content_md5: Option<Utf16String>,
    pub checksum_crc32: Option<Utf16String>,
    #[napi(js_name = "checksumCrc32c")]
    pub checksum_crc32c: Option<Utf16String>,
    pub checksum_sha1: Option<Utf16String>,
    pub checksum_sha256: Option<Utf16String>,
    pub checksum_crc64_nvme: Option<Utf16String>,
}

impl NativeUploadPartInput {
    pub(super) fn try_into_input(
        self,
        env: Env,
        source: mikrio::BodySource,
    ) -> Result<mikrio::UploadPartInput> {
        let mut builder = mikrio::UploadPartInput::builder(
            try_to_string(env, &self.bucket)?,
            try_to_string(env, &self.key)?,
            try_to_string(env, &self.upload_id)?,
            try_to_count(env, self.part_number)?,
            source,
        );

        if let Some(value) = self.expected_bucket_owner {
            builder = builder.expected_bucket_owner(try_to_string(env, &value)?);
        }

        if let Some(value) = self.content_length {
            builder = builder.content_length(try_to_content_length(env, &value)?);
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

        builder.build().map_err(|error| native_error(env, &error))
    }
}

#[napi(object)]
pub struct NativeUploadPartCopyInput {
    pub bucket: Utf16String,
    pub key: Utf16String,
    pub upload_id: Utf16String,
    pub part_number: f64,
    pub copy_source: NativeObjectRef,
    pub expected_bucket_owner: Option<Utf16String>,
    pub expected_source_bucket_owner: Option<Utf16String>,
    pub copy_source_if_match: Option<Utf16String>,
    pub copy_source_if_none_match: Option<Utf16String>,
    pub copy_source_if_modified_since: Option<i64>,
    pub copy_source_if_unmodified_since: Option<i64>,
    pub copy_source_range: Option<Utf16String>,
}

impl NativeUploadPartCopyInput {
    pub(super) fn try_into_input(self, env: Env) -> Result<mikrio::UploadPartCopyInput> {
        let mut builder = mikrio::UploadPartCopyInput::builder(
            try_to_string(env, &self.bucket)?,
            try_to_string(env, &self.key)?,
            try_to_string(env, &self.upload_id)?,
            try_to_count(env, self.part_number)?,
            self.copy_source.try_into_object_ref(env)?,
        );

        if let Some(value) = self.expected_bucket_owner {
            builder = builder.expected_bucket_owner(try_to_string(env, &value)?);
        }

        if let Some(value) = self.expected_source_bucket_owner {
            builder = builder.expected_source_bucket_owner(try_to_string(env, &value)?);
        }

        if let Some(value) = self.copy_source_if_match {
            builder = builder.copy_source_if_match(try_to_string(env, &value)?);
        }

        if let Some(value) = self.copy_source_if_none_match {
            builder = builder.copy_source_if_none_match(try_to_string(env, &value)?);
        }

        if let Some(value) = self.copy_source_if_modified_since {
            builder = builder.copy_source_if_modified_since(try_to_timestamp(env, value)?);
        }

        if let Some(value) = self.copy_source_if_unmodified_since {
            builder = builder.copy_source_if_unmodified_since(try_to_timestamp(env, value)?);
        }

        if let Some(value) = self.copy_source_range {
            builder = builder.copy_source_range(try_to_string(env, &value)?);
        }

        builder.build().map_err(|error| native_error(env, &error))
    }
}

#[napi(object)]
pub struct NativeCompleteMultipartUploadInput {
    pub bucket: Utf16String,
    pub key: Utf16String,
    pub upload_id: Utf16String,
    pub parts: Vec<NativeCompletedPart>,
    pub expected_bucket_owner: Option<Utf16String>,
    pub if_match: Option<Utf16String>,
    pub if_none_match: Option<Utf16String>,
    pub content_length: Option<BigInt>,
    pub checksum_type: Option<Utf16String>,
    pub checksum_crc32: Option<Utf16String>,
    #[napi(js_name = "checksumCrc32c")]
    pub checksum_crc32c: Option<Utf16String>,
    pub checksum_sha1: Option<Utf16String>,
    pub checksum_sha256: Option<Utf16String>,
    pub checksum_crc64_nvme: Option<Utf16String>,
}

impl NativeCompleteMultipartUploadInput {
    pub(super) fn try_into_input(self, env: Env) -> Result<mikrio::CompleteMultipartUploadInput> {
        if self.parts.len() > 10_000 {
            return Err(host_error(
                env,
                "InvalidArgument",
                "at most 10000 parts are accepted",
            ));
        }

        let mut builder = mikrio::CompleteMultipartUploadInput::builder(
            try_to_string(env, &self.bucket)?,
            try_to_string(env, &self.key)?,
            try_to_string(env, &self.upload_id)?,
            self.parts
                .into_iter()
                .map(|part| part.try_into_part(env))
                .collect::<Result<Vec<_>>>()?,
        );

        if let Some(value) = self.expected_bucket_owner {
            builder = builder.expected_bucket_owner(try_to_string(env, &value)?);
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

        if let Some(value) = self.checksum_type {
            builder = builder.checksum_type(try_to_checksum_type(env, &value)?);
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

        builder.build().map_err(|error| native_error(env, &error))
    }
}

#[napi(object)]
pub struct NativeAbortMultipartUploadInput {
    pub bucket: Utf16String,
    pub key: Utf16String,
    pub upload_id: Utf16String,
    pub expected_bucket_owner: Option<Utf16String>,
}

impl NativeAbortMultipartUploadInput {
    pub(super) fn try_into_input(self, env: Env) -> Result<mikrio::AbortMultipartUploadInput> {
        let mut builder = mikrio::AbortMultipartUploadInput::builder(
            try_to_string(env, &self.bucket)?,
            try_to_string(env, &self.key)?,
            try_to_string(env, &self.upload_id)?,
        );

        if let Some(value) = self.expected_bucket_owner {
            builder = builder.expected_bucket_owner(try_to_string(env, &value)?);
        }

        builder.build().map_err(|error| native_error(env, &error))
    }
}

#[napi(object)]
pub struct NativeListPartsInput {
    pub bucket: Utf16String,
    pub key: Utf16String,
    pub upload_id: Utf16String,
    pub expected_bucket_owner: Option<Utf16String>,
    pub part_number_marker: Option<f64>,
    pub max_parts: Option<f64>,
}

impl NativeListPartsInput {
    pub(super) fn try_into_input(self, env: Env) -> Result<mikrio::ListPartsInput> {
        let mut builder = mikrio::ListPartsInput::builder(
            try_to_string(env, &self.bucket)?,
            try_to_string(env, &self.key)?,
            try_to_string(env, &self.upload_id)?,
        );

        if let Some(value) = self.expected_bucket_owner {
            builder = builder.expected_bucket_owner(try_to_string(env, &value)?);
        }

        if let Some(value) = self.part_number_marker {
            builder = builder.part_number_marker(try_to_count(env, value)?);
        }

        if let Some(value) = self.max_parts {
            builder = builder.max_parts(try_to_count(env, value)?);
        }

        builder.build().map_err(|error| native_error(env, &error))
    }
}

#[napi(object)]
pub struct NativeListMultipartUploadsInput {
    pub bucket: Utf16String,
    pub expected_bucket_owner: Option<Utf16String>,
    pub prefix: Option<Utf16String>,
    pub delimiter: Option<Utf16String>,
    pub key_marker: Option<Utf16String>,
    pub upload_id_marker: Option<Utf16String>,
    pub max_uploads: Option<f64>,
}

impl NativeListMultipartUploadsInput {
    pub(super) fn try_into_input(self, env: Env) -> Result<mikrio::ListMultipartUploadsInput> {
        let mut builder =
            mikrio::ListMultipartUploadsInput::builder(try_to_string(env, &self.bucket)?);

        if let Some(value) = self.expected_bucket_owner {
            builder = builder.expected_bucket_owner(try_to_string(env, &value)?);
        }

        if let Some(value) = self.prefix {
            builder = builder.prefix(try_to_string(env, &value)?);
        }

        if let Some(value) = self.delimiter {
            builder = builder.delimiter(try_to_string(env, &value)?);
        }

        if let Some(value) = self.key_marker {
            builder = builder.key_marker(try_to_string(env, &value)?);
        }

        if let Some(value) = self.upload_id_marker {
            builder = builder.upload_id_marker(try_to_string(env, &value)?);
        }

        if let Some(value) = self.max_uploads {
            builder = builder.max_uploads(try_to_count(env, value)?);
        }

        builder.build().map_err(|error| native_error(env, &error))
    }
}

fn try_to_checksum_type(env: Env, value: &[u16]) -> Result<mikrio::ChecksumType> {
    match try_to_string(env, value)?.as_str() {
        "FULL_OBJECT" => Ok(mikrio::ChecksumType::FullObject),
        "COMPOSITE" => Ok(mikrio::ChecksumType::Composite),
        _ => Err(host_error(
            env,
            "InvalidArgument",
            "unsupported checksum type",
        )),
    }
}

fn try_to_copy_directive(env: Env, value: &[u16]) -> Result<mikrio::CopyDirective> {
    match try_to_string(env, value)?.as_str() {
        "COPY" => Ok(mikrio::CopyDirective::Copy),
        "REPLACE" => Ok(mikrio::CopyDirective::Replace),
        _ => Err(host_error(
            env,
            "InvalidArgument",
            "unsupported copy directive",
        )),
    }
}
