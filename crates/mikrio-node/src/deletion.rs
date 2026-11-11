//! Batch delivery retains cancellation settlement and original input positions.

use napi::bindgen_prelude::{PromiseRaw, Utf16String};
use napi::{Env, Result};
use napi_derive::napi;

use crate::values::try_to_string;
use crate::{
    NativeCancellation, NativeStore, PENDING, contain, host_error, native_error, with_outcome,
};

#[napi(object)]
pub struct NativeObjectIdentifier {
    pub key: Utf16String,
    pub etag: Option<Utf16String>,
}

#[napi(object)]
pub struct NativeDeleteObjectsInput {
    pub bucket: Utf16String,
    pub expected_bucket_owner: Option<Utf16String>,
    pub objects: Vec<NativeObjectIdentifier>,
    pub quiet: Option<bool>,
}

#[napi(object, object_from_js = false)]
pub struct NativeDeletedObject {
    pub key: String,
}

#[napi(object, object_from_js = false)]
pub(crate) struct NativeDeleteError {
    pub code: String,
    pub message: String,
}

#[napi(object, object_from_js = false)]
pub struct NativeDeleteObjectError {
    pub key: String,
    pub code: String,
    pub message: String,
}

#[napi(object, object_from_js = false)]
pub struct NativeDeleteObjectsOutput {
    pub deleted: Vec<NativeDeletedObject>,
    pub errors: Vec<NativeDeleteObjectError>,
}

#[napi(object, object_from_js = false)]
pub(crate) struct NativeDeleteObjectProgress {
    pub index: u32,
    pub key: String,
    pub mutation_outcome: String,
    pub error: Option<NativeDeleteError>,
}

#[napi(object, object_from_js = false)]
pub(crate) struct NativeDeleteObjectsProgress {
    pub items: Vec<NativeDeleteObjectProgress>,
}

impl TryFrom<&mikrio::DeleteObjectsProgress> for NativeDeleteObjectsProgress {
    type Error = napi::Error;

    fn try_from(progress: &mikrio::DeleteObjectsProgress) -> Result<Self> {
        let items = progress
            .items()
            .iter()
            .map(|item| {
                Ok(NativeDeleteObjectProgress {
                    index: u32::try_from(item.index())
                        .map_err(|_| napi::Error::from_reason("invalid batch progress index"))?,
                    key: item.key().into(),
                    mutation_outcome: item.mutation_outcome().as_str().into(),
                    error: item.error().map(|error| NativeDeleteError {
                        code: error.code().as_str().into(),
                        message: error.message().into(),
                    }),
                })
            })
            .collect::<Result<Vec<_>>>()?;

        Ok(Self { items })
    }
}

#[napi]
impl NativeStore {
    /// Delete ordered entries and settle the active transaction on cancellation.
    ///
    /// # Errors
    /// Preserves validation, lifecycle, indexed progress and mutation outcomes.
    #[napi(catch_unwind)]
    pub fn delete_objects<'env>(
        &self,
        env: &'env Env,
        input: NativeDeleteObjectsInput,
        cancellation: &NativeCancellation,
    ) -> Result<PromiseRaw<'env, NativeDeleteObjectsOutput>> {
        let input = (|| {
            let objects = input
                .objects
                .into_iter()
                .map(|object| {
                    let mut builder =
                        mikrio::ObjectIdentifier::builder(try_to_string(*env, &object.key)?);

                    if let Some(etag) = object.etag {
                        builder = builder.etag(try_to_string(*env, &etag)?);
                    }

                    builder.build().map_err(|error| native_error(*env, &error))
                })
                .collect::<Result<Vec<_>>>()?;

            let mut builder =
                mikrio::DeleteObjectsInput::builder(try_to_string(*env, &input.bucket)?, objects)
                    .quiet(input.quiet.unwrap_or(false));

            if let Some(owner) = input.expected_bucket_owner {
                builder = builder.expected_bucket_owner(try_to_string(*env, &owner)?);
            }

            builder.build().map_err(|error| native_error(*env, &error))
        })()
        .map_err(|error| with_outcome(*env, error, "NotCommitted"))?;

        if *cancellation.sender.borrow() {
            return Err(with_outcome(
                *env,
                host_error(*env, "OperationCanceled", "operation was canceled"),
                "NotCommitted",
            ));
        }

        let permit = PENDING.try_acquire().map_err(|_| {
            with_outcome(
                *env,
                host_error(*env, "StoreBusy", "native host admission is busy"),
                "NotCommitted",
            )
        })?;

        let mut canceled = cancellation.sender.subscribe();
        let sender = cancellation.sender.clone();
        let store = self.store.clone();

        env.spawn_future_with_callback(
            async move {
                let _permit = permit;
                let _sender = sender;

                Ok(contain(mikrio::embedding::delete_objects_until(
                    &store,
                    input,
                    async move {
                        if !*canceled.borrow() {
                            let _changed = canceled.changed().await;
                        }
                    },
                ))
                .await)
            },
            |env, result| {
                let output = result
                    .map_err(|_| {
                        with_outcome(
                            *env,
                            host_error(*env, "IoError", "native operation panicked"),
                            "Unknown",
                        )
                    })?
                    .map_err(|error| native_error(*env, &error))?;

                Ok(NativeDeleteObjectsOutput {
                    deleted: output
                        .deleted()
                        .iter()
                        .map(|item| NativeDeletedObject {
                            key: item.key().into(),
                        })
                        .collect(),
                    errors: output
                        .errors()
                        .iter()
                        .map(|item| NativeDeleteObjectError {
                            key: item.key().into(),
                            code: item.code().as_str().into(),
                            message: item.message().into(),
                        })
                        .collect(),
                })
            },
        )
    }
}
