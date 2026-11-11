//! Owned lifecycle projections; the shared facade owns semantic validation.

use napi::bindgen_prelude::{PromiseRaw, Utf16String};
use napi::{Env, Result};
use napi_derive::napi;

use crate::access_configuration::NativeBucketAccessInput;
use crate::values::{try_to_count, try_to_string};
use crate::{NativeCancellation, NativeStore, dispatch, host_error, native_error, with_outcome};

#[napi(object)]
pub struct NativePutBucketLifecycleConfigurationInput {
    pub bucket: Utf16String,
    pub expected_bucket_owner: Option<Utf16String>,
    pub lifecycle_rules: Vec<NativeLifecycleRule>,
}

#[napi(object)]
pub struct NativeLifecycleFilter {
    pub prefix: Option<Utf16String>,
}

#[napi(object)]
pub struct NativeLifecycleExpiration {
    pub days: f64,
}

#[napi(object)]
pub struct NativeAbortIncompleteMultipartUpload {
    pub days_after_initiation: f64,
}

#[napi(object)]
pub struct NativeLifecycleRule {
    pub id: Option<Utf16String>,
    pub status: Utf16String,
    pub filter: NativeLifecycleFilter,
    pub expiration: Option<NativeLifecycleExpiration>,
    pub abort_incomplete_multipart_upload: Option<NativeAbortIncompleteMultipartUpload>,
}

impl NativeLifecycleRule {
    fn try_to_rule(&self, env: Env) -> Result<mikrio::LifecycleRule> {
        let status = match try_to_string(env, &self.status)?.as_str() {
            "Enabled" => mikrio::LifecycleStatus::Enabled,
            "Disabled" => mikrio::LifecycleStatus::Disabled,
            _ => {
                return Err(host_error(
                    env,
                    "InvalidArgument",
                    "invalid lifecycle status",
                ));
            }
        };

        let filter = mikrio::LifecycleFilter::new(
            self.filter
                .prefix
                .as_ref()
                .map(|value| try_to_string(env, value))
                .transpose()?,
        )
        .map_err(|error| native_error(env, &error))?;

        let mut builder = mikrio::LifecycleRule::builder(status, filter);

        if let Some(id) = &self.id {
            builder = builder.id(try_to_string(env, id)?);
        }

        if let Some(action) = &self.expiration {
            let action = mikrio::LifecycleExpiration::new(try_to_count(env, action.days)?)
                .map_err(|error| native_error(env, &error))?;

            builder = builder.expiration(action);
        }

        if let Some(action) = &self.abort_incomplete_multipart_upload {
            let action = mikrio::AbortIncompleteMultipartUpload::new(try_to_count(
                env,
                action.days_after_initiation,
            )?)
            .map_err(|error| native_error(env, &error))?;

            builder = builder.abort_incomplete_multipart_upload(action);
        }

        builder.build().map_err(|error| native_error(env, &error))
    }
}

#[napi(object, object_from_js = false)]
pub struct NativeLifecycleFilterOutput {
    pub prefix: Option<String>,
}

#[napi(object, object_from_js = false)]
pub struct NativeLifecycleExpirationOutput {
    pub days: u32,
}

#[napi(object, object_from_js = false)]
pub struct NativeAbortIncompleteMultipartUploadOutput {
    pub days_after_initiation: u32,
}

#[napi(object, object_from_js = false)]
pub struct NativeLifecycleRuleOutput {
    pub id: Option<String>,
    pub status: String,
    pub filter: NativeLifecycleFilterOutput,
    pub expiration: Option<NativeLifecycleExpirationOutput>,
    pub abort_incomplete_multipart_upload: Option<NativeAbortIncompleteMultipartUploadOutput>,
}

impl From<&mikrio::LifecycleRule> for NativeLifecycleRuleOutput {
    fn from(value: &mikrio::LifecycleRule) -> Self {
        Self {
            id: value.id().map(str::to_owned),
            status: value.status().as_str().to_owned(),
            filter: NativeLifecycleFilterOutput {
                prefix: value.filter().prefix().map(str::to_owned),
            },
            expiration: value
                .expiration()
                .map(|action| NativeLifecycleExpirationOutput {
                    days: action.days(),
                }),
            abort_incomplete_multipart_upload: value.abort_incomplete_multipart_upload().map(
                |action| NativeAbortIncompleteMultipartUploadOutput {
                    days_after_initiation: action.days_after_initiation(),
                },
            ),
        }
    }
}

#[napi(object, object_from_js = false)]
pub struct NativeGetBucketLifecycleConfigurationOutput {
    pub lifecycle_rules: Vec<NativeLifecycleRuleOutput>,
}

#[napi]
impl NativeStore {
    /// Validate and replace the ordered bucket lifecycle rules.
    ///
    /// # Errors
    /// Preserves validation, ownership, lifecycle and transaction failures.
    #[napi(catch_unwind)]
    #[allow(
        clippy::needless_pass_by_value,
        reason = "NAPI owns the decoded operation input"
    )]
    pub fn put_bucket_lifecycle_configuration<'env>(
        &self,
        env: &'env Env,
        input: NativePutBucketLifecycleConfigurationInput,
        cancellation: &NativeCancellation,
    ) -> Result<PromiseRaw<'env, ()>> {
        let input = (|| {
            let mut builder = mikrio::PutBucketLifecycleConfigurationInput::builder(
                try_to_string(*env, &input.bucket)?,
                input
                    .lifecycle_rules
                    .iter()
                    .map(|rule| rule.try_to_rule(*env))
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
            async move { store.put_bucket_lifecycle_configuration(input).await },
            |_, _| Ok(()),
        )
    }

    /// Read detached lifecycle rules, preserving omitted filter prefixes.
    ///
    /// # Errors
    /// Preserves validation, ownership, lifecycle and transaction failures.
    #[napi(catch_unwind)]
    #[allow(
        clippy::needless_pass_by_value,
        reason = "NAPI owns the decoded operation input"
    )]
    pub fn get_bucket_lifecycle_configuration<'env>(
        &self,
        env: &'env Env,
        input: NativeBucketAccessInput,
        cancellation: &NativeCancellation,
    ) -> Result<PromiseRaw<'env, NativeGetBucketLifecycleConfigurationOutput>> {
        let mut builder = mikrio::GetBucketLifecycleConfigurationInput::builder(try_to_string(
            *env,
            &input.bucket,
        )?);

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
            async move { store.get_bucket_lifecycle_configuration(input).await },
            |_, output| {
                Ok(NativeGetBucketLifecycleConfigurationOutput {
                    lifecycle_rules: output
                        .lifecycle_rules()
                        .iter()
                        .map(NativeLifecycleRuleOutput::from)
                        .collect(),
                })
            },
        )
    }

    /// Remove the bucket lifecycle configuration through the shared transaction.
    ///
    /// # Errors
    /// Preserves validation, ownership, lifecycle and transaction failures.
    #[napi(catch_unwind)]
    #[allow(
        clippy::needless_pass_by_value,
        reason = "NAPI owns the decoded operation input"
    )]
    pub fn delete_bucket_lifecycle<'env>(
        &self,
        env: &'env Env,
        input: NativeBucketAccessInput,
        cancellation: &NativeCancellation,
    ) -> Result<PromiseRaw<'env, ()>> {
        let input = (|| {
            let mut builder =
                mikrio::DeleteBucketLifecycleInput::builder(try_to_string(*env, &input.bucket)?);

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
            async move { store.delete_bucket_lifecycle(input).await },
            |_, _| Ok(()),
        )
    }
}
