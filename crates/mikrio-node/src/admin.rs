//! Administration translates owned values onto the shared native worker.

use napi::bindgen_prelude::{PromiseRaw, Utf16String};
use napi::{Env, Result};
use napi_derive::napi;

use crate::values::try_to_string;
use crate::{NativeCancellation, NativeStore, dispatch, host_error, native_error, with_outcome};

#[napi(object, object_from_js = false)]
pub struct NativeOwnerInfo {
    pub account_id: String,
    pub canonical_id: String,
}

impl From<&mikrio::OwnerInfo> for NativeOwnerInfo {
    fn from(value: &mikrio::OwnerInfo) -> Self {
        Self {
            account_id: value.account_id().to_owned(),
            canonical_id: value.canonical_id().to_owned(),
        }
    }
}

#[napi(object, object_from_js = false)]
pub struct NativePrincipalInfo {
    pub principal_id: String,
    pub name: String,
    pub is_enabled: bool,
    pub created_at: i64,
}

impl From<&mikrio::PrincipalInfo> for NativePrincipalInfo {
    fn from(value: &mikrio::PrincipalInfo) -> Self {
        Self {
            principal_id: value.principal_id().to_owned(),
            name: value.name().to_owned(),
            is_enabled: value.is_enabled(),
            created_at: value.created_at().as_unix_millis(),
        }
    }
}

#[napi(object, object_from_js = false)]
pub struct NativeAccessKeyInfo {
    pub access_key_id: String,
    pub principal_id: String,
    pub created_at: i64,
}

impl From<&mikrio::AccessKeyInfo> for NativeAccessKeyInfo {
    fn from(value: &mikrio::AccessKeyInfo) -> Self {
        Self {
            access_key_id: value.access_key_id().to_owned(),
            principal_id: value.principal_id().to_owned(),
            created_at: value.created_at().as_unix_millis(),
        }
    }
}

#[napi(object, object_from_js = false)]
pub struct NativeCreateAccessKeyOutput {
    pub access_key: NativeAccessKeyInfo,
    pub secret_access_key: String,
}

impl From<&mikrio::CreateAccessKeyOutput> for NativeCreateAccessKeyOutput {
    fn from(value: &mikrio::CreateAccessKeyOutput) -> Self {
        Self {
            access_key: NativeAccessKeyInfo::from(value.access_key()),
            secret_access_key: value.secret_access_key().to_owned(),
        }
    }
}

#[napi(object, object_from_js = false)]
pub struct NativeListPrincipalsOutput {
    pub principals: Vec<NativePrincipalInfo>,
    pub next_continuation_token: Option<String>,
}

impl From<&mikrio::ListPrincipalsOutput> for NativeListPrincipalsOutput {
    fn from(value: &mikrio::ListPrincipalsOutput) -> Self {
        Self {
            principals: value
                .principals()
                .iter()
                .map(NativePrincipalInfo::from)
                .collect(),
            next_continuation_token: value.next_continuation_token().map(str::to_owned),
        }
    }
}

#[napi(object, object_from_js = false)]
pub struct NativeListAccessKeysOutput {
    pub access_keys: Vec<NativeAccessKeyInfo>,
    pub next_continuation_token: Option<String>,
}

impl From<&mikrio::ListAccessKeysOutput> for NativeListAccessKeysOutput {
    fn from(value: &mikrio::ListAccessKeysOutput) -> Self {
        Self {
            access_keys: value
                .access_keys()
                .iter()
                .map(NativeAccessKeyInfo::from)
                .collect(),
            next_continuation_token: value.next_continuation_token().map(str::to_owned),
        }
    }
}

#[napi(object, object_from_js = false)]
pub struct NativeGetPrincipalPolicyOutput {
    pub document: String,
}

impl From<&mikrio::GetPrincipalPolicyOutput> for NativeGetPrincipalPolicyOutput {
    fn from(value: &mikrio::GetPrincipalPolicyOutput) -> Self {
        Self {
            document: value.document().to_owned(),
        }
    }
}

#[napi(object)]
pub struct NativeCreatePrincipalInput {
    pub name: Utf16String,
}

#[napi(object)]
pub struct NativeGetPrincipalInput {
    pub principal_id: Utf16String,
}

#[napi(object)]
pub struct NativeSetPrincipalEnabledInput {
    pub principal_id: Utf16String,
    pub is_enabled: bool,
}

#[napi(object)]
pub struct NativeDeletePrincipalInput {
    pub principal_id: Utf16String,
}

#[napi(object)]
pub struct NativePutPrincipalPolicyInput {
    pub principal_id: Utf16String,
    pub document: Utf16String,
}

#[napi(object)]
pub struct NativeGetPrincipalPolicyInput {
    pub principal_id: Utf16String,
}

#[napi(object)]
pub struct NativeDeletePrincipalPolicyInput {
    pub principal_id: Utf16String,
}

#[napi(object)]
pub struct NativeCreateAccessKeyInput {
    pub principal_id: Utf16String,
}

#[napi(object)]
pub struct NativeDeleteAccessKeyInput {
    pub principal_id: Utf16String,
    pub access_key_id: Utf16String,
}

#[napi(object)]
pub struct NativeListPrincipalsInput {
    pub max_results: Option<f64>,
    pub continuation_token: Option<Utf16String>,
}

#[napi(object)]
pub struct NativeListAccessKeysInput {
    pub principal_id: Utf16String,
    pub max_results: Option<f64>,
    pub continuation_token: Option<Utf16String>,
}

#[napi]
impl NativeStore {
    /// Read detached identifiers for the store owner.
    ///
    /// # Errors
    /// Preserves validation, lifecycle and native transaction failures.
    #[napi(catch_unwind)]
    pub fn get_owner_info<'env>(
        &self,
        env: &'env Env,
        cancellation: &NativeCancellation,
    ) -> Result<PromiseRaw<'env, NativeOwnerInfo>> {
        let store = self.store.clone();

        dispatch(
            env,
            cancellation,
            false,
            async move { store.get_owner_info().await },
            |_, output| Ok(NativeOwnerInfo::from(&output)),
        )
    }

    /// Create an enabled principal without keys or a policy.
    ///
    /// # Errors
    /// Preserves validation, lifecycle and native transaction failures.
    #[napi(catch_unwind)]
    #[allow(
        clippy::needless_pass_by_value,
        reason = "NAPI owns the decoded operation input"
    )]
    pub fn create_principal<'env>(
        &self,
        env: &'env Env,
        input: NativeCreatePrincipalInput,
        cancellation: &NativeCancellation,
    ) -> Result<PromiseRaw<'env, NativePrincipalInfo>> {
        let input = (|| {
            let builder = mikrio::CreatePrincipalInput::builder(try_to_string(*env, &input.name)?);
            builder.build().map_err(|error| native_error(*env, &error))
        })()
        .map_err(|error| with_outcome(*env, error, "NotCommitted"))?;

        let store = self.store.clone();

        dispatch(
            env,
            cancellation,
            true,
            async move { store.create_principal(input).await },
            |_, output| Ok(NativePrincipalInfo::from(&output)),
        )
    }

    /// Read a detached principal snapshot by its identity.
    ///
    /// # Errors
    /// Preserves validation, lifecycle and native transaction failures.
    #[napi(catch_unwind)]
    #[allow(
        clippy::needless_pass_by_value,
        reason = "NAPI owns the decoded operation input"
    )]
    pub fn get_principal<'env>(
        &self,
        env: &'env Env,
        input: NativeGetPrincipalInput,
        cancellation: &NativeCancellation,
    ) -> Result<PromiseRaw<'env, NativePrincipalInfo>> {
        let builder = mikrio::GetPrincipalInput::builder(try_to_string(*env, &input.principal_id)?);

        let input = builder
            .build()
            .map_err(|error| native_error(*env, &error))?;

        let store = self.store.clone();

        dispatch(
            env,
            cancellation,
            false,
            async move { store.get_principal(input).await },
            |_, output| Ok(NativePrincipalInfo::from(&output)),
        )
    }

    /// Change principal access while retaining its keys and policy.
    ///
    /// # Errors
    /// Preserves validation, lifecycle and native transaction failures.
    #[napi(catch_unwind)]
    #[allow(
        clippy::needless_pass_by_value,
        reason = "NAPI owns the decoded operation input"
    )]
    pub fn set_principal_enabled<'env>(
        &self,
        env: &'env Env,
        input: NativeSetPrincipalEnabledInput,
        cancellation: &NativeCancellation,
    ) -> Result<PromiseRaw<'env, NativePrincipalInfo>> {
        let input = (|| {
            let builder = mikrio::SetPrincipalEnabledInput::builder(
                try_to_string(*env, &input.principal_id)?,
                input.is_enabled,
            );

            builder.build().map_err(|error| native_error(*env, &error))
        })()
        .map_err(|error| with_outcome(*env, error, "NotCommitted"))?;

        let store = self.store.clone();

        dispatch(
            env,
            cancellation,
            true,
            async move { store.set_principal_enabled(input).await },
            |_, output| Ok(NativePrincipalInfo::from(&output)),
        )
    }

    /// Delete a principal after all of its access keys are revoked.
    ///
    /// # Errors
    /// Preserves validation, lifecycle and native transaction failures.
    #[napi(catch_unwind)]
    #[allow(
        clippy::needless_pass_by_value,
        reason = "NAPI owns the decoded operation input"
    )]
    pub fn delete_principal<'env>(
        &self,
        env: &'env Env,
        input: NativeDeletePrincipalInput,
        cancellation: &NativeCancellation,
    ) -> Result<PromiseRaw<'env, ()>> {
        let input = (|| {
            let builder =
                mikrio::DeletePrincipalInput::builder(try_to_string(*env, &input.principal_id)?);

            builder.build().map_err(|error| native_error(*env, &error))
        })()
        .map_err(|error| with_outcome(*env, error, "NotCommitted"))?;

        let store = self.store.clone();

        dispatch(
            env,
            cancellation,
            true,
            async move { store.delete_principal(input).await },
            |_, _| Ok(()),
        )
    }

    /// Validate and atomically replace the principal policy.
    ///
    /// # Errors
    /// Preserves validation, lifecycle and native transaction failures.
    #[napi(catch_unwind)]
    #[allow(
        clippy::needless_pass_by_value,
        reason = "NAPI owns the decoded operation input"
    )]
    pub fn put_principal_policy<'env>(
        &self,
        env: &'env Env,
        input: NativePutPrincipalPolicyInput,
        cancellation: &NativeCancellation,
    ) -> Result<PromiseRaw<'env, ()>> {
        let input = (|| {
            let builder = mikrio::PutPrincipalPolicyInput::builder(
                try_to_string(*env, &input.principal_id)?,
                try_to_string(*env, &input.document)?,
            );

            builder.build().map_err(|error| native_error(*env, &error))
        })()
        .map_err(|error| with_outcome(*env, error, "NotCommitted"))?;

        let store = self.store.clone();

        dispatch(
            env,
            cancellation,
            true,
            async move { store.put_principal_policy(input).await },
            |_, _| Ok(()),
        )
    }

    /// Read the stored principal policy document without rewriting it.
    ///
    /// # Errors
    /// Preserves validation, lifecycle and native transaction failures.
    #[napi(catch_unwind)]
    #[allow(
        clippy::needless_pass_by_value,
        reason = "NAPI owns the decoded operation input"
    )]
    pub fn get_principal_policy<'env>(
        &self,
        env: &'env Env,
        input: NativeGetPrincipalPolicyInput,
        cancellation: &NativeCancellation,
    ) -> Result<PromiseRaw<'env, NativeGetPrincipalPolicyOutput>> {
        let builder =
            mikrio::GetPrincipalPolicyInput::builder(try_to_string(*env, &input.principal_id)?);

        let input = builder
            .build()
            .map_err(|error| native_error(*env, &error))?;

        let store = self.store.clone();

        dispatch(
            env,
            cancellation,
            false,
            async move { store.get_principal_policy(input).await },
            |_, output| Ok(NativeGetPrincipalPolicyOutput::from(&output)),
        )
    }

    /// Remove the principal policy, accepting an already absent policy.
    ///
    /// # Errors
    /// Preserves validation, lifecycle and native transaction failures.
    #[napi(catch_unwind)]
    #[allow(
        clippy::needless_pass_by_value,
        reason = "NAPI owns the decoded operation input"
    )]
    pub fn delete_principal_policy<'env>(
        &self,
        env: &'env Env,
        input: NativeDeletePrincipalPolicyInput,
        cancellation: &NativeCancellation,
    ) -> Result<PromiseRaw<'env, ()>> {
        let input = (|| {
            let builder = mikrio::DeletePrincipalPolicyInput::builder(try_to_string(
                *env,
                &input.principal_id,
            )?);

            builder.build().map_err(|error| native_error(*env, &error))
        })()
        .map_err(|error| with_outcome(*env, error, "NotCommitted"))?;

        let store = self.store.clone();

        dispatch(
            env,
            cancellation,
            true,
            async move { store.delete_principal_policy(input).await },
            |_, _| Ok(()),
        )
    }

    /// Create a signing key and return its secret with the committed result.
    ///
    /// # Errors
    /// Preserves validation, lifecycle and native transaction failures.
    #[napi(catch_unwind)]
    #[allow(
        clippy::needless_pass_by_value,
        reason = "NAPI owns the decoded operation input"
    )]
    pub fn create_access_key<'env>(
        &self,
        env: &'env Env,
        input: NativeCreateAccessKeyInput,
        cancellation: &NativeCancellation,
    ) -> Result<PromiseRaw<'env, NativeCreateAccessKeyOutput>> {
        let input = (|| {
            let builder =
                mikrio::CreateAccessKeyInput::builder(try_to_string(*env, &input.principal_id)?);

            builder.build().map_err(|error| native_error(*env, &error))
        })()
        .map_err(|error| with_outcome(*env, error, "NotCommitted"))?;

        let store = self.store.clone();

        dispatch(
            env,
            cancellation,
            true,
            async move { store.create_access_key(input).await },
            |_, output| Ok(NativeCreateAccessKeyOutput::from(&output)),
        )
    }

    /// Revoke a key belonging to the specified principal.
    ///
    /// # Errors
    /// Preserves validation, lifecycle and native transaction failures.
    #[napi(catch_unwind)]
    #[allow(
        clippy::needless_pass_by_value,
        reason = "NAPI owns the decoded operation input"
    )]
    pub fn delete_access_key<'env>(
        &self,
        env: &'env Env,
        input: NativeDeleteAccessKeyInput,
        cancellation: &NativeCancellation,
    ) -> Result<PromiseRaw<'env, ()>> {
        let input = (|| {
            let builder = mikrio::DeleteAccessKeyInput::builder(
                try_to_string(*env, &input.principal_id)?,
                try_to_string(*env, &input.access_key_id)?,
            );

            builder.build().map_err(|error| native_error(*env, &error))
        })()
        .map_err(|error| with_outcome(*env, error, "NotCommitted"))?;

        let store = self.store.clone();

        dispatch(
            env,
            cancellation,
            true,
            async move { store.delete_access_key(input).await },
            |_, _| Ok(()),
        )
    }

    /// Read a bounded principal page with a scoped continuation token.
    ///
    /// # Errors
    /// Preserves validation, lifecycle and native transaction failures.
    #[napi(catch_unwind)]
    pub fn list_principals<'env>(
        &self,
        env: &'env Env,
        input: NativeListPrincipalsInput,
        cancellation: &NativeCancellation,
    ) -> Result<PromiseRaw<'env, NativeListPrincipalsOutput>> {
        let mut builder = mikrio::ListPrincipalsInput::builder();

        if let Some(size) = input.max_results {
            let size = size
                .to_string()
                .parse::<u32>()
                .map_err(|_| host_error(*env, "InvalidArgument", "invalid page size"))?;

            builder = builder.max_results(size);
        }

        if let Some(token) = input.continuation_token {
            builder = builder.continuation_token(try_to_string(*env, &token)?);
        }

        let input = builder
            .build()
            .map_err(|error| native_error(*env, &error))?;

        let store = self.store.clone();

        dispatch(
            env,
            cancellation,
            false,
            async move { store.list_principals(input).await },
            |_, output| Ok(NativeListPrincipalsOutput::from(&output)),
        )
    }

    /// Read a bounded page of key facts without returning secrets.
    ///
    /// # Errors
    /// Preserves validation, lifecycle and native transaction failures.
    #[napi(catch_unwind)]
    pub fn list_access_keys<'env>(
        &self,
        env: &'env Env,
        input: NativeListAccessKeysInput,
        cancellation: &NativeCancellation,
    ) -> Result<PromiseRaw<'env, NativeListAccessKeysOutput>> {
        let mut builder =
            mikrio::ListAccessKeysInput::builder(try_to_string(*env, &input.principal_id)?);

        if let Some(size) = input.max_results {
            let size = size
                .to_string()
                .parse::<u32>()
                .map_err(|_| host_error(*env, "InvalidArgument", "invalid page size"))?;

            builder = builder.max_results(size);
        }

        if let Some(token) = input.continuation_token {
            builder = builder.continuation_token(try_to_string(*env, &token)?);
        }

        let input = builder
            .build()
            .map_err(|error| native_error(*env, &error))?;

        let store = self.store.clone();

        dispatch(
            env,
            cancellation,
            false,
            async move { store.list_access_keys(input).await },
            |_, output| Ok(NativeListAccessKeysOutput::from(&output)),
        )
    }
}
