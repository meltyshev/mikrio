//! Owned tag projections over the shared facade and bounded dispatcher.

use napi::bindgen_prelude::{PromiseRaw, Utf16String};
use napi::{Env, Result};
use napi_derive::napi;

use crate::values::{NativeMetadata, NativePair, try_to_string};
use crate::{NativeCancellation, NativeStore, dispatch, native_error, with_outcome};

#[napi(object)]
pub struct NativeObjectTaggingInput {
    pub bucket: Utf16String,
    pub key: Utf16String,
    pub expected_bucket_owner: Option<Utf16String>,
}

#[napi(object)]
pub struct NativePutObjectTaggingInput {
    pub bucket: Utf16String,
    pub key: Utf16String,
    pub expected_bucket_owner: Option<Utf16String>,
    pub tags: Vec<NativePair>,
}

#[napi(object, object_from_js = false)]
pub struct NativeGetObjectTaggingOutput {
    pub tags: Vec<NativeMetadata>,
}

#[napi]
impl NativeStore {
    /// Read the complete detached tag set.
    ///
    /// # Errors
    /// Preserves validation, owner, lifecycle and transaction failures.
    #[napi(catch_unwind)]
    #[allow(
        clippy::needless_pass_by_value,
        reason = "NAPI owns the decoded operation input"
    )]
    pub fn get_object_tagging<'env>(
        &self,
        env: &'env Env,
        input: NativeObjectTaggingInput,
        cancellation: &NativeCancellation,
    ) -> Result<PromiseRaw<'env, NativeGetObjectTaggingOutput>> {
        let mut builder = mikrio::GetObjectTaggingInput::builder(
            try_to_string(*env, &input.bucket)?,
            try_to_string(*env, &input.key)?,
        );

        if let Some(owner) = &input.expected_bucket_owner {
            builder = builder.expected_bucket_owner(try_to_string(*env, owner)?);
        }

        let input = builder
            .build()
            .map_err(|error| native_error(*env, &error))?;

        let store = self.store.clone();

        dispatch(
            env,
            cancellation,
            false,
            async move { store.get_object_tagging(input).await },
            |_, output| {
                Ok(NativeGetObjectTaggingOutput {
                    tags: output
                        .tags()
                        .iter()
                        .map(|tag| NativeMetadata {
                            key: tag.key.clone(),
                            value: tag.value.clone(),
                        })
                        .collect(),
                })
            },
        )
    }

    /// Replace the complete tag set through the shared transaction.
    ///
    /// # Errors
    /// Preserves validation, owner, lifecycle and transaction failures.
    #[napi(catch_unwind)]
    #[allow(
        clippy::needless_pass_by_value,
        reason = "NAPI owns the decoded operation input"
    )]
    pub fn put_object_tagging<'env>(
        &self,
        env: &'env Env,
        input: NativePutObjectTaggingInput,
        cancellation: &NativeCancellation,
    ) -> Result<PromiseRaw<'env, ()>> {
        let input = (|| {
            let mut builder = mikrio::PutObjectTaggingInput::builder(
                try_to_string(*env, &input.bucket)?,
                try_to_string(*env, &input.key)?,
                input
                    .tags
                    .iter()
                    .map(|tag| {
                        Ok(mikrio::Tag {
                            key: try_to_string(*env, &tag.key)?,
                            value: try_to_string(*env, &tag.value)?,
                        })
                    })
                    .collect::<Result<Vec<_>>>()?,
            );

            if let Some(owner) = &input.expected_bucket_owner {
                builder = builder.expected_bucket_owner(try_to_string(*env, owner)?);
            }

            builder.build().map_err(|error| native_error(*env, &error))
        })()
        .map_err(|error| with_outcome(*env, error, "NotCommitted"))?;

        let store = self.store.clone();

        dispatch(
            env,
            cancellation,
            true,
            async move { store.put_object_tagging(input).await },
            |_, _| Ok(()),
        )
    }

    /// Clear tags on the current object through the shared transaction.
    ///
    /// # Errors
    /// Preserves validation, owner, lifecycle and transaction failures.
    #[napi(catch_unwind)]
    #[allow(
        clippy::needless_pass_by_value,
        reason = "NAPI owns the decoded operation input"
    )]
    pub fn delete_object_tagging<'env>(
        &self,
        env: &'env Env,
        input: NativeObjectTaggingInput,
        cancellation: &NativeCancellation,
    ) -> Result<PromiseRaw<'env, ()>> {
        let input = (|| {
            let mut builder = mikrio::DeleteObjectTaggingInput::builder(
                try_to_string(*env, &input.bucket)?,
                try_to_string(*env, &input.key)?,
            );

            if let Some(owner) = &input.expected_bucket_owner {
                builder = builder.expected_bucket_owner(try_to_string(*env, owner)?);
            }

            builder.build().map_err(|error| native_error(*env, &error))
        })()
        .map_err(|error| with_outcome(*env, error, "NotCommitted"))?;

        let store = self.store.clone();

        dispatch(
            env,
            cancellation,
            true,
            async move { store.delete_object_tagging(input).await },
            |_, _| Ok(()),
        )
    }
}
