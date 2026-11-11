//! Basic operations translate owned values and execute on the managed runtime.

use mikrio_embed::body::Body;
use napi::bindgen_prelude::{PromiseRaw, Utf16String};
use napi::{Env, Result};
use napi_derive::napi;

use crate::body::NativeBody;
use crate::upload::NativeUpload;
use crate::values::{
    NativeBucketInput, NativeDeleteInput, NativePutInput, NativePutOutput, NativeReadInput,
    NativeReadOutput, try_to_string,
};
use crate::{
    NativeCancellation, NativeStore, dispatch, mutation_input_error, native_error, with_outcome,
};

#[napi(object, object_from_js = false)]
pub struct NativeGetOutput {
    pub facts: NativeReadOutput,
    pub body: NativeBody,
}

#[napi]
impl NativeStore {
    /// Create a bucket with the shared regional repeat-create semantics.
    ///
    /// # Errors
    /// Returns typed validation, namespace and native mutation failures.
    #[napi(catch_unwind)]
    #[allow(
        clippy::needless_pass_by_value,
        reason = "NAPI owns the decoded UTF-16 argument"
    )]
    pub fn create_bucket<'env>(
        &self,
        env: &'env Env,
        bucket: Utf16String,
        cancellation: &NativeCancellation,
    ) -> Result<PromiseRaw<'env, ()>> {
        let bucket = try_to_string(*env, &bucket)
            .map_err(|error| with_outcome(*env, error, "NotCommitted"))?;

        let input = mikrio::CreateBucketInput::builder(bucket)
            .build()
            .map_err(|error| mutation_input_error(*env, &error))?;

        let store = self.store.clone();

        dispatch(
            env,
            cancellation,
            true,
            async move { store.create_bucket(input).await },
            |_, _| Ok(()),
        )
    }

    /// Inspect a bucket from one native snapshot.
    ///
    /// # Errors
    /// Returns namespace, assertion and native read failures.
    #[napi(catch_unwind)]
    pub fn head_bucket<'env>(
        &self,
        env: &'env Env,
        input: NativeBucketInput,
        cancellation: &NativeCancellation,
    ) -> Result<PromiseRaw<'env, String>> {
        let mut builder = mikrio::HeadBucketInput::builder(try_to_string(*env, &input.bucket)?);

        if let Some(owner) = input.expected_bucket_owner {
            builder = builder.expected_bucket_owner(try_to_string(*env, &owner)?);
        }

        let input = builder
            .build()
            .map_err(|error| native_error(*env, &error))?;

        let store = self.store.clone();

        dispatch(
            env,
            cancellation,
            false,
            async move { store.head_bucket(input).await },
            |_, output| Ok(output.bucket_region().to_owned()),
        )
    }

    /// Read the bucket location, preserving the us-east-1 absence convention.
    ///
    /// # Errors
    /// Returns namespace, assertion and native read failures.
    #[napi(catch_unwind)]
    pub fn get_bucket_location<'env>(
        &self,
        env: &'env Env,
        input: NativeBucketInput,
        cancellation: &NativeCancellation,
    ) -> Result<PromiseRaw<'env, Option<String>>> {
        let mut builder =
            mikrio::GetBucketLocationInput::builder(try_to_string(*env, &input.bucket)?);

        if let Some(owner) = input.expected_bucket_owner {
            builder = builder.expected_bucket_owner(try_to_string(*env, &owner)?);
        }

        let input = builder
            .build()
            .map_err(|error| native_error(*env, &error))?;

        let store = self.store.clone();

        dispatch(
            env,
            cancellation,
            false,
            async move { store.get_bucket_location(input).await },
            |_, output| Ok(output.location_constraint().map(str::to_owned)),
        )
    }

    /// Retire an empty bucket without deleting another namespace generation.
    ///
    /// # Errors
    /// Returns typed validation, nonempty, namespace and mutation failures.
    #[napi(catch_unwind)]
    pub fn delete_bucket<'env>(
        &self,
        env: &'env Env,
        input: NativeBucketInput,
        cancellation: &NativeCancellation,
    ) -> Result<PromiseRaw<'env, ()>> {
        let input = (|| {
            let mut builder =
                mikrio::DeleteBucketInput::builder(try_to_string(*env, &input.bucket)?);

            if let Some(owner) = input.expected_bucket_owner {
                builder = builder.expected_bucket_owner(try_to_string(*env, &owner)?);
            }

            builder.build().map_err(|error| native_error(*env, &error))
        })()
        .map_err(|error| with_outcome(*env, error, "NotCommitted"))?;

        let store = self.store.clone();

        dispatch(
            env,
            cancellation,
            true,
            async move { store.delete_bucket(input).await },
            |_, _| Ok(()),
        )
    }

    /// Stream through native verification and durable publication, without replay.
    ///
    /// # Errors
    /// Returns validation, source, integrity, condition and mutation outcomes.
    #[napi(catch_unwind)]
    pub fn put_object<'env>(
        &self,
        env: &'env Env,
        input: NativePutInput,
        upload: &NativeUpload,
        cancellation: &NativeCancellation,
    ) -> Result<PromiseRaw<'env, NativePutOutput>> {
        let input = upload
            .take_source(*env)
            .and_then(|source| input.try_into_input(*env, source))
            .map_err(|error| with_outcome(*env, error, "NotCommitted"))?;

        let store = self.store.clone();

        dispatch(
            env,
            cancellation,
            true,
            async move { store.put_object(input).await },
            |_, output| Ok(output.into()),
        )
    }

    /// Select a protected body and coherent detached facts.
    ///
    /// # Errors
    /// Returns condition, namespace, assertion and native selection failures.
    #[napi(catch_unwind)]
    pub fn get_object<'env>(
        &self,
        env: &'env Env,
        input: NativeReadInput,
        cancellation: &NativeCancellation,
    ) -> Result<PromiseRaw<'env, NativeGetOutput>> {
        let input = input.try_into_get_input(*env)?;
        let store = self.store.clone();

        dispatch(
            env,
            cancellation,
            false,
            async move {
                let output = store.get_object(input).await?;
                let facts = NativeReadOutput::from(&output);
                let length = output.content_length();

                let body = NativeBody {
                    body: Body::new(output.into_body(), length),
                };

                Ok(NativeGetOutput { facts, body })
            },
            |_, output| Ok(output),
        )
    }

    /// Read coherent metadata without opening a payload file.
    ///
    /// # Errors
    /// Returns condition, namespace, assertion and native selection failures.
    #[napi(catch_unwind)]
    pub fn head_object<'env>(
        &self,
        env: &'env Env,
        input: NativeReadInput,
        cancellation: &NativeCancellation,
    ) -> Result<PromiseRaw<'env, NativeReadOutput>> {
        let input = input.try_into_head_input(*env)?;
        let store = self.store.clone();

        dispatch(
            env,
            cancellation,
            false,
            async move { store.head_object(input).await },
            |_, output| Ok(NativeReadOutput::from(&output)),
        )
    }

    /// Delete atomically under the requested entity-tag condition.
    ///
    /// # Errors
    /// Returns typed validation, condition, namespace and mutation failures.
    #[napi(catch_unwind)]
    pub fn delete_object<'env>(
        &self,
        env: &'env Env,
        input: NativeDeleteInput,
        cancellation: &NativeCancellation,
    ) -> Result<PromiseRaw<'env, ()>> {
        let input = (|| {
            let mut builder = mikrio::DeleteObjectInput::builder(
                try_to_string(*env, &input.bucket)?,
                try_to_string(*env, &input.key)?,
            );

            if let Some(owner) = input.expected_bucket_owner {
                builder = builder.expected_bucket_owner(try_to_string(*env, &owner)?);
            }

            if let Some(value) = input.if_match {
                builder = builder.if_match(try_to_string(*env, &value)?);
            }

            builder.build().map_err(|error| native_error(*env, &error))
        })()
        .map_err(|error| with_outcome(*env, error, "NotCommitted"))?;

        let store = self.store.clone();

        dispatch(
            env,
            cancellation,
            true,
            async move { store.delete_object(input).await },
            |_, _| Ok(()),
        )
    }
}
