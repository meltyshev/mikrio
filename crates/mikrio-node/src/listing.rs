//! Owned namespace translations on the existing managed executor.

use napi::bindgen_prelude::{BigInt, PromiseRaw, Utf16String};
use napi::{Env, Result};
use napi_derive::napi;

use crate::values::try_to_string;
use crate::{NativeCancellation, NativeStore, dispatch, native_error};

#[napi(object)]
pub struct NativeListObjectsInput {
    pub bucket: Utf16String,
    pub expected_bucket_owner: Option<Utf16String>,
    pub prefix: Option<Utf16String>,
    pub delimiter: Option<Utf16String>,
    pub max_keys: Option<f64>,
    pub continuation_token: Option<Utf16String>,
    pub start_after: Option<Utf16String>,
}

#[napi(object, object_from_js = false)]
pub struct NativeObjectSummary {
    pub key: String,
    pub size: BigInt,
    pub etag: String,
    pub last_modified: i64,
}

#[napi(object, object_from_js = false)]
pub struct NativeListObjectsOutput {
    pub contents: Vec<NativeObjectSummary>,
    pub common_prefixes: Vec<String>,
    pub is_truncated: bool,
    pub next_continuation_token: Option<String>,
}

#[napi]
impl NativeStore {
    /// Read a detached page with shared semantic and current namespace checks.
    ///
    /// # Errors
    /// Preserves host/semantic input, scope, lifecycle and storage failures.
    #[napi(catch_unwind)]
    pub fn list_objects_v2<'env>(
        &self,
        env: &'env Env,
        input: NativeListObjectsInput,
        cancellation: &NativeCancellation,
    ) -> Result<PromiseRaw<'env, NativeListObjectsOutput>> {
        let mut builder = mikrio::ListObjectsV2Input::builder(try_to_string(*env, &input.bucket)?);

        if let Some(value) = input.expected_bucket_owner {
            builder = builder.expected_bucket_owner(try_to_string(*env, &value)?);
        }

        if let Some(value) = input.prefix {
            builder = builder.prefix(try_to_string(*env, &value)?);
        }

        if let Some(value) = input.delimiter {
            builder = builder.delimiter(try_to_string(*env, &value)?);
        }

        if let Some(value) = input.start_after {
            builder = builder.start_after(try_to_string(*env, &value)?);
        }

        if let Some(value) = input.continuation_token {
            builder = builder.continuation_token(try_to_string(*env, &value)?);
        }

        if let Some(value) = input.max_keys {
            let value = value
                .to_string()
                .parse::<u32>()
                .map_err(|_| crate::host_error(*env, "InvalidArgument", "invalid page size"))?;

            builder = builder.max_keys(value);
        }

        let input = builder
            .build()
            .map_err(|error| native_error(*env, &error))?;

        let store = self.store.clone();

        dispatch(
            env,
            cancellation,
            false,
            async move { store.list_objects_v2(input).await },
            |_, output| {
                Ok(NativeListObjectsOutput {
                    contents: output
                        .contents()
                        .iter()
                        .map(|object| NativeObjectSummary {
                            key: object.key().to_owned(),
                            size: object.size().into(),
                            etag: object.etag().to_owned(),
                            last_modified: object.last_modified().as_unix_millis(),
                        })
                        .collect(),
                    common_prefixes: output
                        .common_prefixes()
                        .iter()
                        .map(|prefix| prefix.prefix().to_owned())
                        .collect(),
                    is_truncated: output.is_truncated(),
                    next_continuation_token: output.next_continuation_token().map(str::to_owned),
                })
            },
        )
    }
}

#[napi(object)]
pub struct NativeListBucketsInput {
    pub prefix: Option<Utf16String>,
    pub max_buckets: Option<f64>,
    pub continuation_token: Option<Utf16String>,
}

#[napi(object, object_from_js = false)]
pub struct NativeBucketSummary {
    pub name: String,
    pub creation_date: i64,
    pub bucket_region: String,
}

#[napi(object, object_from_js = false)]
pub struct NativeListBucketsOutput {
    pub buckets: Vec<NativeBucketSummary>,
    pub continuation_token: Option<String>,
}

#[napi]
impl NativeStore {
    /// Read a detached bucket page through shared validation and selection.
    ///
    /// # Errors
    /// Preserves host, semantic, lifecycle and storage failures.
    #[napi(catch_unwind)]
    pub fn list_buckets<'env>(
        &self,
        env: &'env Env,
        input: NativeListBucketsInput,
        cancellation: &NativeCancellation,
    ) -> Result<PromiseRaw<'env, NativeListBucketsOutput>> {
        let mut builder = mikrio::ListBucketsInput::builder();

        if let Some(value) = input.prefix {
            builder = builder.prefix(try_to_string(*env, &value)?);
        }

        if let Some(value) = input.continuation_token {
            builder = builder.continuation_token(try_to_string(*env, &value)?);
        }

        if let Some(value) = input.max_buckets {
            let value = value
                .to_string()
                .parse::<u32>()
                .map_err(|_| crate::host_error(*env, "InvalidArgument", "invalid page size"))?;

            builder = builder.max_buckets(value);
        }

        let input = builder
            .build()
            .map_err(|error| native_error(*env, &error))?;

        let store = self.store.clone();

        dispatch(
            env,
            cancellation,
            false,
            async move { store.list_buckets(input).await },
            |_, output| {
                Ok(NativeListBucketsOutput {
                    buckets: output
                        .buckets()
                        .iter()
                        .map(|bucket| NativeBucketSummary {
                            name: bucket.name().to_owned(),
                            creation_date: bucket.creation_date().as_unix_millis(),
                            bucket_region: bucket.bucket_region().to_owned(),
                        })
                        .collect(),
                    continuation_token: output.continuation_token().map(str::to_owned),
                })
            },
        )
    }
}
