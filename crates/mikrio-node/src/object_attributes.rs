//! Detached Attributes values over the bounded facade dispatcher.

use napi::bindgen_prelude::{BigInt, PromiseRaw, Utf16String};
use napi::{Env, Result};
use napi_derive::napi;

use crate::values::{try_to_count, try_to_string};
use crate::{NativeCancellation, NativeStore, dispatch, host_error, native_error};

#[napi(object)]
pub struct NativeGetObjectAttributesInput {
    pub bucket: Utf16String,
    pub key: Utf16String,
    pub expected_bucket_owner: Option<Utf16String>,
    pub object_attributes: Vec<Utf16String>,
    pub max_parts: Option<f64>,
    pub part_number_marker: Option<f64>,
}

#[napi]
impl NativeStore {
    /// Select detached Attributes without a payload cursor.
    ///
    /// # Errors
    /// Preserves input, owner, lifecycle and metadata failures.
    #[napi(catch_unwind)]
    #[allow(clippy::needless_pass_by_value, reason = "NAPI owns decoded inputs")]
    pub fn get_object_attributes<'env>(
        &self,
        env: &'env Env,
        input: NativeGetObjectAttributesInput,
        cancellation: &NativeCancellation,
    ) -> Result<PromiseRaw<'env, NativeGetObjectAttributesOutput>> {
        if input.object_attributes.len() > 5 {
            return Err(host_error(
                *env,
                "InvalidArgument",
                "at most five selectors are allowed",
            ));
        }

        let attributes = input
            .object_attributes
            .iter()
            .map(|value| {
                mikrio::ObjectAttribute::parse(&try_to_string(*env, value)?)
                    .map_err(|error| native_error(*env, &error))
            })
            .collect::<Result<Vec<_>>>()?;

        let mut builder = mikrio::GetObjectAttributesInput::builder(
            try_to_string(*env, &input.bucket)?,
            try_to_string(*env, &input.key)?,
            attributes,
        );

        if let Some(owner) = &input.expected_bucket_owner {
            builder = builder.expected_bucket_owner(try_to_string(*env, owner)?);
        }

        if let Some(value) = input.max_parts {
            builder = builder.max_parts(try_to_count(*env, value)?);
        }

        if let Some(value) = input.part_number_marker {
            builder = builder.part_number_marker(try_to_count(*env, value)?);
        }

        let input = builder
            .build()
            .map_err(|error| native_error(*env, &error))?;

        let store = self.store.clone();

        dispatch(
            env,
            cancellation,
            false,
            async move { store.get_object_attributes(input).await },
            |_, output| Ok(NativeGetObjectAttributesOutput::from(&output)),
        )
    }
}

#[napi(object, object_from_js = false)]
#[allow(
    clippy::struct_field_names,
    reason = "fields match the public JavaScript schema"
)]
pub struct NativeChecksum {
    pub checksum_crc32: Option<String>,
    #[napi(js_name = "checksumCrc32c")]
    pub checksum_crc32c: Option<String>,
    pub checksum_sha1: Option<String>,
    pub checksum_sha256: Option<String>,
    pub checksum_crc64_nvme: Option<String>,
    pub checksum_type: Option<String>,
}

impl From<&mikrio::Checksum> for NativeChecksum {
    fn from(output: &mikrio::Checksum) -> Self {
        Self {
            checksum_crc32: output.checksum_crc32().map(str::to_owned),
            checksum_crc32c: output.checksum_crc32c().map(str::to_owned),
            checksum_sha1: output.checksum_sha1().map(str::to_owned),
            checksum_sha256: output.checksum_sha256().map(str::to_owned),
            checksum_crc64_nvme: output.checksum_crc64_nvme().map(str::to_owned),
            checksum_type: output.checksum_type().map(|mode| mode.as_str().to_owned()),
        }
    }
}

#[napi(object, object_from_js = false)]
pub struct NativeObjectPart {
    pub part_number: u32,
    pub size: BigInt,
    pub checksum_crc32: Option<String>,
    #[napi(js_name = "checksumCrc32c")]
    pub checksum_crc32c: Option<String>,
    pub checksum_sha1: Option<String>,
    pub checksum_sha256: Option<String>,
    pub checksum_crc64_nvme: Option<String>,
}

impl From<&mikrio::ObjectPart> for NativeObjectPart {
    fn from(output: &mikrio::ObjectPart) -> Self {
        Self {
            part_number: output.part_number(),
            size: BigInt::from(output.size()),
            checksum_crc32: output.checksum_crc32().map(str::to_owned),
            checksum_crc32c: output.checksum_crc32c().map(str::to_owned),
            checksum_sha1: output.checksum_sha1().map(str::to_owned),
            checksum_sha256: output.checksum_sha256().map(str::to_owned),
            checksum_crc64_nvme: output.checksum_crc64_nvme().map(str::to_owned),
        }
    }
}

#[napi(object, object_from_js = false)]
pub struct NativeObjectParts {
    pub total_parts_count: u32,
    pub max_parts: u32,
    pub part_number_marker: u32,
    pub parts: Vec<NativeObjectPart>,
    pub is_truncated: bool,
    pub next_part_number_marker: Option<u32>,
}

impl From<&mikrio::ObjectParts> for NativeObjectParts {
    fn from(output: &mikrio::ObjectParts) -> Self {
        Self {
            total_parts_count: output.total_parts_count(),
            max_parts: output.max_parts(),
            part_number_marker: output.part_number_marker(),
            parts: output.parts().iter().map(NativeObjectPart::from).collect(),
            is_truncated: output.is_truncated(),
            next_part_number_marker: output.next_part_number_marker(),
        }
    }
}

#[napi(object, object_from_js = false)]
pub struct NativeGetObjectAttributesOutput {
    pub object_size: Option<BigInt>,
    pub etag: Option<String>,
    pub checksum: Option<NativeChecksum>,
    pub storage_class: Option<String>,
    pub object_parts: Option<NativeObjectParts>,
    pub last_modified: i64,
}

impl From<&mikrio::GetObjectAttributesOutput> for NativeGetObjectAttributesOutput {
    fn from(output: &mikrio::GetObjectAttributesOutput) -> Self {
        Self {
            object_size: output.object_size().map(BigInt::from),
            etag: output.etag().map(str::to_owned),
            checksum: output.checksum().map(NativeChecksum::from),
            storage_class: output.storage_class().map(str::to_owned),
            object_parts: output.object_parts().map(NativeObjectParts::from),
            last_modified: output.last_modified().as_unix_millis(),
        }
    }
}
