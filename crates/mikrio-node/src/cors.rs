//! Owned CORS projections; the shared facade owns semantic validation.

use napi::bindgen_prelude::{PromiseRaw, Utf16String};
use napi::{Env, Result};
use napi_derive::napi;

use crate::access_configuration::NativeBucketAccessInput;
use crate::values::try_to_string;
use crate::{NativeCancellation, NativeStore, dispatch, host_error, native_error, with_outcome};

#[napi(object)]
pub struct NativePutBucketCorsInput {
    pub bucket: Utf16String,
    pub expected_bucket_owner: Option<Utf16String>,
    pub cors_rules: Vec<NativeCorsRule>,
}

#[napi(object)]
pub struct NativeCorsRule {
    pub id: Option<Utf16String>,
    pub allowed_origins: Vec<Utf16String>,
    pub allowed_methods: Vec<Utf16String>,
    pub allowed_headers: Option<Vec<Utf16String>>,
    pub expose_headers: Option<Vec<Utf16String>>,
    pub max_age_seconds: Option<f64>,
}

impl NativeCorsRule {
    fn try_to_rule(&self, env: Env) -> Result<mikrio::CorsRule> {
        let try_to_strings = |values: &[Utf16String]| {
            values
                .iter()
                .map(|value| try_to_string(env, value))
                .collect::<Result<Vec<_>>>()
        };

        let mut builder = mikrio::CorsRule::builder(
            try_to_strings(&self.allowed_origins)?,
            try_to_strings(&self.allowed_methods)?,
        );

        if let Some(id) = &self.id {
            builder = builder.id(try_to_string(env, id)?);
        }

        if let Some(headers) = &self.allowed_headers {
            builder = builder.allowed_headers(try_to_strings(headers)?);
        }

        if let Some(headers) = &self.expose_headers {
            builder = builder.expose_headers(try_to_strings(headers)?);
        }

        if let Some(seconds) = self.max_age_seconds {
            if !seconds.is_finite()
                || seconds.fract() != 0.0
                || !(f64::from(i32::MIN)..=f64::from(i32::MAX)).contains(&seconds)
            {
                return Err(host_error(env, "InvalidArgument", "invalid CORS max age"));
            }

            #[expect(
                clippy::cast_possible_truncation,
                clippy::as_conversions,
                reason = "finite integral i32 range checked above"
            )]
            let seconds = seconds as i32;

            builder = builder.max_age_seconds(seconds);
        }

        builder.build().map_err(|error| native_error(env, &error))
    }
}

#[napi(object, object_from_js = false)]
pub struct NativeCorsRuleOutput {
    pub id: Option<String>,
    pub allowed_origins: Vec<String>,
    pub allowed_methods: Vec<String>,
    pub allowed_headers: Option<Vec<String>>,
    pub expose_headers: Option<Vec<String>>,
    pub max_age_seconds: Option<i32>,
}

impl From<&mikrio::CorsRule> for NativeCorsRuleOutput {
    fn from(value: &mikrio::CorsRule) -> Self {
        Self {
            id: value.id().map(str::to_owned),
            allowed_origins: value.allowed_origins().to_vec(),
            allowed_methods: value.allowed_methods().to_vec(),
            allowed_headers: value.allowed_headers().map(<[String]>::to_vec),
            expose_headers: value.expose_headers().map(<[String]>::to_vec),
            max_age_seconds: value.max_age_seconds(),
        }
    }
}

#[napi(object, object_from_js = false)]
pub struct NativeGetBucketCorsOutput {
    pub cors_rules: Vec<NativeCorsRuleOutput>,
}

#[napi]
impl NativeStore {
    /// Validate and replace the ordered bucket CORS rules.
    ///
    /// # Errors
    /// Preserves validation, ownership, lifecycle and transaction failures.
    #[napi(catch_unwind)]
    #[allow(
        clippy::needless_pass_by_value,
        reason = "NAPI owns the decoded operation input"
    )]
    pub fn put_bucket_cors<'env>(
        &self,
        env: &'env Env,
        input: NativePutBucketCorsInput,
        cancellation: &NativeCancellation,
    ) -> Result<PromiseRaw<'env, ()>> {
        let input = (|| {
            let mut builder = mikrio::PutBucketCorsInput::builder(
                try_to_string(*env, &input.bucket)?,
                input
                    .cors_rules
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
            async move { store.put_bucket_cors(input).await },
            |_, _| Ok(()),
        )
    }

    /// Read detached CORS rules, preserving optional header lists.
    ///
    /// # Errors
    /// Preserves validation, ownership, lifecycle and transaction failures.
    #[napi(catch_unwind)]
    #[allow(
        clippy::needless_pass_by_value,
        reason = "NAPI owns the decoded operation input"
    )]
    pub fn get_bucket_cors<'env>(
        &self,
        env: &'env Env,
        input: NativeBucketAccessInput,
        cancellation: &NativeCancellation,
    ) -> Result<PromiseRaw<'env, NativeGetBucketCorsOutput>> {
        let mut builder = mikrio::GetBucketCorsInput::builder(try_to_string(*env, &input.bucket)?);

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
            async move { store.get_bucket_cors(input).await },
            |_, output| {
                Ok(NativeGetBucketCorsOutput {
                    cors_rules: output
                        .cors_rules()
                        .iter()
                        .map(NativeCorsRuleOutput::from)
                        .collect(),
                })
            },
        )
    }

    /// Remove the bucket CORS configuration through the shared transaction.
    ///
    /// # Errors
    /// Preserves validation, ownership, lifecycle and transaction failures.
    #[napi(catch_unwind)]
    #[allow(
        clippy::needless_pass_by_value,
        reason = "NAPI owns the decoded operation input"
    )]
    pub fn delete_bucket_cors<'env>(
        &self,
        env: &'env Env,
        input: NativeBucketAccessInput,
        cancellation: &NativeCancellation,
    ) -> Result<PromiseRaw<'env, ()>> {
        let input = (|| {
            let mut builder =
                mikrio::DeleteBucketCorsInput::builder(try_to_string(*env, &input.bucket)?);

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
            async move { store.delete_bucket_cors(input).await },
            |_, _| Ok(()),
        )
    }
}
