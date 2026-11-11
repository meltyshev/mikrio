//! Host access-configuration values; shared validation and authorization use the facade.

use napi::bindgen_prelude::{PromiseRaw, Utf16String};
use napi::{Env, Result};
use napi_derive::napi;

use crate::values::try_to_string;
use crate::{NativeCancellation, NativeStore, dispatch, host_error, native_error, with_outcome};

#[napi(object)]
pub struct NativeBucketAccessInput {
    pub bucket: Utf16String,
    pub expected_bucket_owner: Option<Utf16String>,
}

#[napi(object)]
pub struct NativePutBucketPolicyInput {
    pub bucket: Utf16String,
    pub expected_bucket_owner: Option<Utf16String>,
    pub document: Utf16String,
}

#[napi(object)]
#[allow(
    clippy::struct_excessive_bools,
    reason = "four independent S3 configuration flags"
)]
pub struct NativePublicAccessBlockConfiguration {
    pub block_public_acls: bool,
    pub ignore_public_acls: bool,
    pub block_public_policy: bool,
    pub restrict_public_buckets: bool,
}

impl From<NativePublicAccessBlockConfiguration> for mikrio::PublicAccessBlockConfiguration {
    fn from(configuration: NativePublicAccessBlockConfiguration) -> Self {
        Self {
            block_public_acls: configuration.block_public_acls,
            ignore_public_acls: configuration.ignore_public_acls,
            block_public_policy: configuration.block_public_policy,
            restrict_public_buckets: configuration.restrict_public_buckets,
        }
    }
}

impl From<mikrio::PublicAccessBlockConfiguration> for NativePublicAccessBlockConfiguration {
    fn from(configuration: mikrio::PublicAccessBlockConfiguration) -> Self {
        Self {
            block_public_acls: configuration.block_public_acls,
            ignore_public_acls: configuration.ignore_public_acls,
            block_public_policy: configuration.block_public_policy,
            restrict_public_buckets: configuration.restrict_public_buckets,
        }
    }
}

#[napi(object)]
pub struct NativePutPublicAccessBlockInput {
    pub bucket: Utf16String,
    pub expected_bucket_owner: Option<Utf16String>,
    pub configuration: NativePublicAccessBlockConfiguration,
}

#[napi(object)]
pub struct NativePutBucketOwnershipControlsInput {
    pub bucket: Utf16String,
    pub expected_bucket_owner: Option<Utf16String>,
    pub object_ownership: Utf16String,
}

#[napi(object)]
pub struct NativeGetObjectAclInput {
    pub bucket: Utf16String,
    pub key: Utf16String,
    pub expected_bucket_owner: Option<Utf16String>,
}

#[napi(object, object_from_js = false)]
pub struct NativeGetBucketPolicyOutput {
    pub document: String,
}

#[napi(object, object_from_js = false)]
pub struct NativeGetBucketPolicyStatusOutput {
    pub is_public: bool,
}

#[napi(object, object_from_js = false)]
pub struct NativeGetPublicAccessBlockOutput {
    pub configuration: NativePublicAccessBlockConfiguration,
}

#[napi(object, object_from_js = false)]
pub struct NativeGetBucketOwnershipControlsOutput {
    pub object_ownership: String,
}

#[napi(object, object_from_js = false)]
pub struct NativeAclOwner {
    pub canonical_id: String,
}

#[napi(object, object_from_js = false)]
pub struct NativeAclGrant {
    pub grantee_canonical_id: String,
    pub permission: String,
}

#[napi(object, object_from_js = false)]
pub struct NativeAclOutput {
    pub owner: NativeAclOwner,
    pub grants: Vec<NativeAclGrant>,
}

impl NativeAclOutput {
    fn from_native(owner: &mikrio::AclOwner, grants: &[mikrio::AclGrant]) -> Self {
        Self {
            owner: NativeAclOwner {
                canonical_id: owner.canonical_id().into(),
            },
            grants: grants
                .iter()
                .map(|grant| NativeAclGrant {
                    grantee_canonical_id: grant.grantee_canonical_id().into(),
                    permission: match grant.permission() {
                        mikrio::AclPermission::FullControl => "FULL_CONTROL".into(),
                    },
                })
                .collect(),
        }
    }
}

#[napi]
impl NativeStore {
    /// Validate and atomically replace the bucket policy.
    ///
    /// # Errors
    /// Preserves validation, ownership, lifecycle and transaction failures.
    #[napi(catch_unwind)]
    #[allow(
        clippy::needless_pass_by_value,
        reason = "NAPI owns the decoded operation input"
    )]
    pub fn put_bucket_policy<'env>(
        &self,
        env: &'env Env,
        input: NativePutBucketPolicyInput,
        cancellation: &NativeCancellation,
    ) -> Result<PromiseRaw<'env, ()>> {
        let input = (|| {
            let mut builder = mikrio::PutBucketPolicyInput::builder(
                try_to_string(*env, &input.bucket)?,
                try_to_string(*env, &input.document)?,
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
            async move { store.put_bucket_policy(input).await },
            |_, _| Ok(()),
        )
    }

    /// Read the stored bucket policy document without rewriting it.
    ///
    /// # Errors
    /// Preserves validation, ownership, lifecycle and transaction failures.
    #[napi(catch_unwind)]
    #[allow(
        clippy::needless_pass_by_value,
        reason = "NAPI owns the decoded operation input"
    )]
    pub fn get_bucket_policy<'env>(
        &self,
        env: &'env Env,
        input: NativeBucketAccessInput,
        cancellation: &NativeCancellation,
    ) -> Result<PromiseRaw<'env, NativeGetBucketPolicyOutput>> {
        let mut builder =
            mikrio::GetBucketPolicyInput::builder(try_to_string(*env, &input.bucket)?);

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
            async move { store.get_bucket_policy(input).await },
            |_, output| {
                Ok(NativeGetBucketPolicyOutput {
                    document: output.document().into(),
                })
            },
        )
    }

    /// Remove the bucket policy through the shared transaction.
    ///
    /// # Errors
    /// Preserves validation, ownership, lifecycle and transaction failures.
    #[napi(catch_unwind)]
    #[allow(
        clippy::needless_pass_by_value,
        reason = "NAPI owns the decoded operation input"
    )]
    pub fn delete_bucket_policy<'env>(
        &self,
        env: &'env Env,
        input: NativeBucketAccessInput,
        cancellation: &NativeCancellation,
    ) -> Result<PromiseRaw<'env, ()>> {
        let input = (|| {
            let mut builder =
                mikrio::DeleteBucketPolicyInput::builder(try_to_string(*env, &input.bucket)?);

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
            async move { store.delete_bucket_policy(input).await },
            |_, _| Ok(()),
        )
    }

    /// Read the shared policy classification for public access.
    ///
    /// # Errors
    /// Preserves validation, ownership, lifecycle and transaction failures.
    #[napi(catch_unwind)]
    #[allow(
        clippy::needless_pass_by_value,
        reason = "NAPI owns the decoded operation input"
    )]
    pub fn get_bucket_policy_status<'env>(
        &self,
        env: &'env Env,
        input: NativeBucketAccessInput,
        cancellation: &NativeCancellation,
    ) -> Result<PromiseRaw<'env, NativeGetBucketPolicyStatusOutput>> {
        let mut builder =
            mikrio::GetBucketPolicyStatusInput::builder(try_to_string(*env, &input.bucket)?);

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
            async move { store.get_bucket_policy_status(input).await },
            |_, output| {
                Ok(NativeGetBucketPolicyStatusOutput {
                    is_public: output.is_public(),
                })
            },
        )
    }

    /// Replace the four independent public-access restrictions.
    ///
    /// # Errors
    /// Preserves validation, ownership, lifecycle and transaction failures.
    #[napi(catch_unwind)]
    pub fn put_public_access_block<'env>(
        &self,
        env: &'env Env,
        input: NativePutPublicAccessBlockInput,
        cancellation: &NativeCancellation,
    ) -> Result<PromiseRaw<'env, ()>> {
        let input = (|| {
            let mut builder = mikrio::PutPublicAccessBlockInput::builder(
                try_to_string(*env, &input.bucket)?,
                input.configuration.into(),
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
            async move { store.put_public_access_block(input).await },
            |_, _| Ok(()),
        )
    }

    /// Read detached public-access restrictions for the bucket.
    ///
    /// # Errors
    /// Preserves validation, ownership, lifecycle and transaction failures.
    #[napi(catch_unwind)]
    #[allow(
        clippy::needless_pass_by_value,
        reason = "NAPI owns the decoded operation input"
    )]
    pub fn get_public_access_block<'env>(
        &self,
        env: &'env Env,
        input: NativeBucketAccessInput,
        cancellation: &NativeCancellation,
    ) -> Result<PromiseRaw<'env, NativeGetPublicAccessBlockOutput>> {
        let mut builder =
            mikrio::GetPublicAccessBlockInput::builder(try_to_string(*env, &input.bucket)?);

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
            async move { store.get_public_access_block(input).await },
            |_, output| {
                Ok(NativeGetPublicAccessBlockOutput {
                    configuration: NativePublicAccessBlockConfiguration::from(
                        output.configuration(),
                    ),
                })
            },
        )
    }

    /// Remove the bucket public-access-block configuration.
    ///
    /// # Errors
    /// Preserves validation, ownership, lifecycle and transaction failures.
    #[napi(catch_unwind)]
    #[allow(
        clippy::needless_pass_by_value,
        reason = "NAPI owns the decoded operation input"
    )]
    pub fn delete_public_access_block<'env>(
        &self,
        env: &'env Env,
        input: NativeBucketAccessInput,
        cancellation: &NativeCancellation,
    ) -> Result<PromiseRaw<'env, ()>> {
        let input = (|| {
            let mut builder =
                mikrio::DeletePublicAccessBlockInput::builder(try_to_string(*env, &input.bucket)?);

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
            async move { store.delete_public_access_block(input).await },
            |_, _| Ok(()),
        )
    }

    /// Set the supported bucket-owner-enforced ownership mode.
    ///
    /// # Errors
    /// Preserves validation, ownership, lifecycle and transaction failures.
    #[napi(catch_unwind)]
    #[allow(
        clippy::needless_pass_by_value,
        reason = "NAPI owns the decoded operation input"
    )]
    pub fn put_bucket_ownership_controls<'env>(
        &self,
        env: &'env Env,
        input: NativePutBucketOwnershipControlsInput,
        cancellation: &NativeCancellation,
    ) -> Result<PromiseRaw<'env, ()>> {
        let input = (|| {
            let ownership = match try_to_string(*env, &input.object_ownership)?.as_str() {
                "BucketOwnerEnforced" => mikrio::ObjectOwnership::BucketOwnerEnforced,
                _ => {
                    return Err(host_error(
                        *env,
                        "InvalidArgument",
                        "unsupported ownership mode",
                    ));
                }
            };

            let mut builder = mikrio::PutBucketOwnershipControlsInput::builder(
                try_to_string(*env, &input.bucket)?,
                ownership,
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
            async move { store.put_bucket_ownership_controls(input).await },
            |_, _| Ok(()),
        )
    }

    /// Read the bucket ownership mode as a detached value.
    ///
    /// # Errors
    /// Preserves validation, ownership, lifecycle and transaction failures.
    #[napi(catch_unwind)]
    #[allow(
        clippy::needless_pass_by_value,
        reason = "NAPI owns the decoded operation input"
    )]
    pub fn get_bucket_ownership_controls<'env>(
        &self,
        env: &'env Env,
        input: NativeBucketAccessInput,
        cancellation: &NativeCancellation,
    ) -> Result<PromiseRaw<'env, NativeGetBucketOwnershipControlsOutput>> {
        let mut builder =
            mikrio::GetBucketOwnershipControlsInput::builder(try_to_string(*env, &input.bucket)?);

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
            async move { store.get_bucket_ownership_controls(input).await },
            |_, output| {
                Ok(NativeGetBucketOwnershipControlsOutput {
                    object_ownership: match output.object_ownership() {
                        mikrio::ObjectOwnership::BucketOwnerEnforced => {
                            "BucketOwnerEnforced".into()
                        }
                    },
                })
            },
        )
    }

    /// Read the bucket owner and grants as detached ACL values.
    ///
    /// # Errors
    /// Preserves validation, ownership, lifecycle and transaction failures.
    #[napi(catch_unwind)]
    #[allow(
        clippy::needless_pass_by_value,
        reason = "NAPI owns the decoded operation input"
    )]
    pub fn get_bucket_acl<'env>(
        &self,
        env: &'env Env,
        input: NativeBucketAccessInput,
        cancellation: &NativeCancellation,
    ) -> Result<PromiseRaw<'env, NativeAclOutput>> {
        let mut builder = mikrio::GetBucketAclInput::builder(try_to_string(*env, &input.bucket)?);

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
            async move { store.get_bucket_acl(input).await },
            |_, output| {
                Ok(NativeAclOutput::from_native(
                    output.owner(),
                    output.grants(),
                ))
            },
        )
    }

    /// Read the object owner and grants as detached ACL values.
    ///
    /// # Errors
    /// Preserves validation, ownership, lifecycle and transaction failures.
    #[napi(catch_unwind)]
    #[allow(
        clippy::needless_pass_by_value,
        reason = "NAPI owns the decoded operation input"
    )]
    pub fn get_object_acl<'env>(
        &self,
        env: &'env Env,
        input: NativeGetObjectAclInput,
        cancellation: &NativeCancellation,
    ) -> Result<PromiseRaw<'env, NativeAclOutput>> {
        let mut builder = mikrio::GetObjectAclInput::builder(
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
            async move { store.get_object_acl(input).await },
            |_, output| {
                Ok(NativeAclOutput::from_native(
                    output.owner(),
                    output.grants(),
                ))
            },
        )
    }
}
