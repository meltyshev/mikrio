//! COPY and multipart calls use the managed dispatcher and the same facade.

mod input;
mod output;

use napi::bindgen_prelude::PromiseRaw;
use napi::{Env, Result};
use napi_derive::napi;

use crate::upload::NativeUpload;
use crate::{NativeCancellation, NativeStore, dispatch, with_outcome};
use input::{
    NativeAbortMultipartUploadInput, NativeCompleteMultipartUploadInput, NativeCopyObjectInput,
    NativeCreateMultipartUploadInput, NativeListMultipartUploadsInput, NativeListPartsInput,
    NativeUploadPartCopyInput, NativeUploadPartInput,
};
use output::{
    NativeCompleteMultipartUploadOutput, NativeCopyObjectOutput, NativeCreateMultipartUploadOutput,
    NativeListMultipartUploadsOutput, NativeListPartsOutput, NativeUploadPartCopyOutput,
    NativeUploadPartOutput,
};

#[napi]
impl NativeStore {
    /// Copy a selected source object into the destination through shared publication.
    ///
    /// # Errors
    /// Preserves host validation, cancellation and storage outcomes.
    #[napi(catch_unwind)]
    pub fn copy_object<'env>(
        &self,
        env: &'env Env,
        input: NativeCopyObjectInput,
        cancellation: &NativeCancellation,
    ) -> Result<PromiseRaw<'env, NativeCopyObjectOutput>> {
        let input = input
            .try_into_input(*env)
            .map_err(|error| with_outcome(*env, error, "NotCommitted"))?;

        let store = self.store.clone();

        dispatch(
            env,
            cancellation,
            true,
            async move { store.copy_object(input).await },
            |_, output| Ok((&output).into()),
        )
    }

    /// Create a durable multipart upload with the requested object metadata.
    ///
    /// # Errors
    /// Preserves host validation, cancellation and storage outcomes.
    #[napi(catch_unwind)]
    pub fn create_multipart_upload<'env>(
        &self,
        env: &'env Env,
        input: NativeCreateMultipartUploadInput,
        cancellation: &NativeCancellation,
    ) -> Result<PromiseRaw<'env, NativeCreateMultipartUploadOutput>> {
        let input = input
            .try_into_input(*env)
            .map_err(|error| with_outcome(*env, error, "NotCommitted"))?;

        let store = self.store.clone();

        dispatch(
            env,
            cancellation,
            true,
            async move { store.create_multipart_upload(input).await },
            |_, output| Ok((&output).into()),
        )
    }

    /// Publish one upload part from the bounded host stream.
    ///
    /// # Errors
    /// Preserves host validation, cancellation and storage outcomes.
    #[napi(catch_unwind)]
    pub fn upload_part<'env>(
        &self,
        env: &'env Env,
        input: NativeUploadPartInput,
        upload: &NativeUpload,
        cancellation: &NativeCancellation,
    ) -> Result<PromiseRaw<'env, NativeUploadPartOutput>> {
        let input = upload
            .take_source(*env)
            .and_then(|source| input.try_into_input(*env, source))
            .map_err(|error| with_outcome(*env, error, "NotCommitted"))?;

        let store = self.store.clone();

        dispatch(
            env,
            cancellation,
            true,
            async move { store.upload_part(input).await },
            |_, output| Ok((&output).into()),
        )
    }

    /// Copy selected source bytes into one upload part.
    ///
    /// # Errors
    /// Preserves host validation, cancellation and storage outcomes.
    #[napi(catch_unwind)]
    pub fn upload_part_copy<'env>(
        &self,
        env: &'env Env,
        input: NativeUploadPartCopyInput,
        cancellation: &NativeCancellation,
    ) -> Result<PromiseRaw<'env, NativeUploadPartCopyOutput>> {
        let input = input
            .try_into_input(*env)
            .map_err(|error| with_outcome(*env, error, "NotCommitted"))?;

        let store = self.store.clone();

        dispatch(
            env,
            cancellation,
            true,
            async move { store.upload_part_copy(input).await },
            |_, output| Ok((&output).into()),
        )
    }

    /// Validate the ordered part manifest and publish the completed object.
    ///
    /// # Errors
    /// Preserves host validation, cancellation and storage outcomes.
    #[napi(catch_unwind)]
    pub fn complete_multipart_upload<'env>(
        &self,
        env: &'env Env,
        input: NativeCompleteMultipartUploadInput,
        cancellation: &NativeCancellation,
    ) -> Result<PromiseRaw<'env, NativeCompleteMultipartUploadOutput>> {
        let input = input
            .try_into_input(*env)
            .map_err(|error| with_outcome(*env, error, "NotCommitted"))?;

        let store = self.store.clone();

        dispatch(
            env,
            cancellation,
            true,
            async move { store.complete_multipart_upload(input).await },
            |_, output| Ok((&output).into()),
        )
    }

    /// Retire an incomplete upload and its parts through the shared transaction.
    ///
    /// # Errors
    /// Preserves host validation, cancellation and storage outcomes.
    #[napi(catch_unwind)]
    pub fn abort_multipart_upload<'env>(
        &self,
        env: &'env Env,
        input: NativeAbortMultipartUploadInput,
        cancellation: &NativeCancellation,
    ) -> Result<PromiseRaw<'env, ()>> {
        let input = input
            .try_into_input(*env)
            .map_err(|error| with_outcome(*env, error, "NotCommitted"))?;

        let store = self.store.clone();

        dispatch(
            env,
            cancellation,
            true,
            async move { store.abort_multipart_upload(input).await },
            |_, _| Ok(()),
        )
    }

    /// Read a bounded page of detached part facts for one upload.
    ///
    /// # Errors
    /// Preserves host validation, cancellation and storage outcomes.
    #[napi(catch_unwind)]
    pub fn list_parts<'env>(
        &self,
        env: &'env Env,
        input: NativeListPartsInput,
        cancellation: &NativeCancellation,
    ) -> Result<PromiseRaw<'env, NativeListPartsOutput>> {
        let input = input.try_into_input(*env)?;
        let store = self.store.clone();

        dispatch(
            env,
            cancellation,
            false,
            async move { store.list_parts(input).await },
            |_, output| Ok((&output).into()),
        )
    }

    /// Read a bounded page of pending uploads and common prefixes.
    ///
    /// # Errors
    /// Preserves host validation, cancellation and storage outcomes.
    #[napi(catch_unwind)]
    pub fn list_multipart_uploads<'env>(
        &self,
        env: &'env Env,
        input: NativeListMultipartUploadsInput,
        cancellation: &NativeCancellation,
    ) -> Result<PromiseRaw<'env, NativeListMultipartUploadsOutput>> {
        let input = input.try_into_input(*env)?;
        let store = self.store.clone();

        dispatch(
            env,
            cancellation,
            false,
            async move { store.list_multipart_uploads(input).await },
            |_, output| Ok((&output).into()),
        )
    }
}
