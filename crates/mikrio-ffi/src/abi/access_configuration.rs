//! Sized access-configuration inputs and independently owned immutable results.

use super::{Error, MutationOutcome, Span, StoreHandle, entry, handles, output, required, sized};

#[derive(Clone, Copy, Default)]
#[repr(C)]
pub(super) struct PublicAccessBlockConfiguration {
    block_public_acls: u32,
    ignore_public_acls: u32,
    block_public_policy: u32,
    restrict_public_buckets: u32,
}

impl PublicAccessBlockConfiguration {
    fn try_into_native(self) -> Result<mikrio::PublicAccessBlockConfiguration, Error> {
        if [
            self.block_public_acls,
            self.ignore_public_acls,
            self.block_public_policy,
            self.restrict_public_buckets,
        ]
        .iter()
        .any(|flag| *flag > 1)
        {
            return Err(Error::invalid(
                "public access block flags must be zero or one",
            ));
        }

        Ok(mikrio::PublicAccessBlockConfiguration {
            block_public_acls: self.block_public_acls == 1,
            ignore_public_acls: self.ignore_public_acls == 1,
            block_public_policy: self.block_public_policy == 1,
            restrict_public_buckets: self.restrict_public_buckets == 1,
        })
    }

    fn from_native(value: mikrio::PublicAccessBlockConfiguration) -> Self {
        Self {
            block_public_acls: u32::from(value.block_public_acls),
            ignore_public_acls: u32::from(value.ignore_public_acls),
            block_public_policy: u32::from(value.block_public_policy),
            restrict_public_buckets: u32::from(value.restrict_public_buckets),
        }
    }
}

#[derive(Default)]
#[repr(C)]
pub(super) struct AclOwnerView {
    canonical_id: Span,
}

#[derive(Default)]
#[repr(C)]
pub(super) struct AclGrantView {
    grantee_canonical_id: Span,
    permission: u32,
    reserved: u32,
}

fn grant_view(value: &mikrio::AclGrant) -> AclGrantView {
    AclGrantView {
        grantee_canonical_id: Span::borrowed(value.grantee_canonical_id()),
        permission: match value.permission() {
            mikrio::AclPermission::FullControl => 1,
        },
        reserved: 0,
    }
}

#[derive(Default)]
#[repr(C)]
pub(super) struct GetBucketPolicyOutputView {
    document: Span,
}

/// # Safety
/// A nonnull pointer must identify the matching live, aligned library allocation.
/// Keep it immutable and unreleased until all returned borrowed views expire.
#[unsafe(no_mangle)]
pub(super) unsafe extern "C" fn mikrio_v1_get_bucket_policy_output_view(
    value: *const mikrio::GetBucketPolicyOutput,
) -> GetBucketPolicyOutputView {
    // SAFETY: the caller keeps this immutable allocation live until all view use ends.
    let Some(value) = (unsafe { value.as_ref() }) else {
        return GetBucketPolicyOutputView::default();
    };

    GetBucketPolicyOutputView {
        document: Span::borrowed(value.document()),
    }
}

release!(
    mikrio_v1_get_bucket_policy_output_release,
    mikrio::GetBucketPolicyOutput
);

#[derive(Default)]
#[repr(C)]
pub(super) struct GetBucketPolicyStatusOutputView {
    is_public: u32,
    reserved: u32,
}

/// # Safety
/// A nonnull pointer must identify the matching live, aligned library allocation
/// and remain immutable and unreleased for this call.
#[unsafe(no_mangle)]
pub(super) unsafe extern "C" fn mikrio_v1_get_bucket_policy_status_output_view(
    value: *const mikrio::GetBucketPolicyStatusOutput,
) -> GetBucketPolicyStatusOutputView {
    // SAFETY: the caller keeps this immutable allocation live until all view use ends.
    let Some(value) = (unsafe { value.as_ref() }) else {
        return GetBucketPolicyStatusOutputView::default();
    };

    GetBucketPolicyStatusOutputView {
        is_public: u32::from(value.is_public()),
        reserved: 0,
    }
}

release!(
    mikrio_v1_get_bucket_policy_status_output_release,
    mikrio::GetBucketPolicyStatusOutput
);

#[derive(Default)]
#[repr(C)]
pub(super) struct GetPublicAccessBlockOutputView {
    configuration: PublicAccessBlockConfiguration,
}

/// # Safety
/// A nonnull pointer must identify the matching live, aligned library allocation
/// and remain immutable and unreleased for this call.
#[unsafe(no_mangle)]
pub(super) unsafe extern "C" fn mikrio_v1_get_public_access_block_output_view(
    value: *const mikrio::GetPublicAccessBlockOutput,
) -> GetPublicAccessBlockOutputView {
    // SAFETY: the caller keeps this immutable allocation live until all view use ends.
    let Some(value) = (unsafe { value.as_ref() }) else {
        return GetPublicAccessBlockOutputView::default();
    };

    GetPublicAccessBlockOutputView {
        configuration: PublicAccessBlockConfiguration::from_native(value.configuration()),
    }
}

release!(
    mikrio_v1_get_public_access_block_output_release,
    mikrio::GetPublicAccessBlockOutput
);

#[derive(Default)]
#[repr(C)]
pub(super) struct GetBucketOwnershipControlsOutputView {
    object_ownership: u32,
    reserved: u32,
}

/// # Safety
/// A nonnull pointer must identify the matching live, aligned library allocation
/// and remain immutable and unreleased for this call.
#[unsafe(no_mangle)]
pub(super) unsafe extern "C" fn mikrio_v1_get_bucket_ownership_controls_output_view(
    value: *const mikrio::GetBucketOwnershipControlsOutput,
) -> GetBucketOwnershipControlsOutputView {
    // SAFETY: the caller keeps this immutable allocation live until all view use ends.
    let Some(value) = (unsafe { value.as_ref() }) else {
        return GetBucketOwnershipControlsOutputView::default();
    };

    GetBucketOwnershipControlsOutputView {
        object_ownership: match value.object_ownership() {
            mikrio::ObjectOwnership::BucketOwnerEnforced => 1,
        },
        reserved: 0,
    }
}

release!(
    mikrio_v1_get_bucket_ownership_controls_output_release,
    mikrio::GetBucketOwnershipControlsOutput
);

#[derive(Default)]
#[repr(C)]
pub(super) struct GetBucketAclOutputView {
    owner: AclOwnerView,
    grant_count: u64,
}

/// # Safety
/// A nonnull pointer must identify the matching live, aligned library allocation.
/// Keep it immutable and unreleased until all returned borrowed views expire.
#[unsafe(no_mangle)]
pub(super) unsafe extern "C" fn mikrio_v1_get_bucket_acl_output_view(
    value: *const mikrio::GetBucketAclOutput,
) -> GetBucketAclOutputView {
    // SAFETY: the caller keeps this immutable allocation live until all view use ends.
    let Some(value) = (unsafe { value.as_ref() }) else {
        return GetBucketAclOutputView::default();
    };

    GetBucketAclOutputView {
        owner: AclOwnerView {
            canonical_id: Span::borrowed(value.owner().canonical_id()),
        },
        grant_count: u64::try_from(value.grants().len()).unwrap_or(u64::MAX),
    }
}

release!(
    mikrio_v1_get_bucket_acl_output_release,
    mikrio::GetBucketAclOutput
);

/// # Safety
/// A nonnull pointer must identify the matching live, aligned library allocation.
/// Keep it immutable and unreleased until all returned borrowed views expire.
#[unsafe(no_mangle)]
pub(super) unsafe extern "C" fn mikrio_v1_get_bucket_acl_output_grant(
    value: *const mikrio::GetBucketAclOutput,
    index: u64,
) -> AclGrantView {
    // SAFETY: the caller retains the immutable result through this call and view use.
    let Some(value) = (unsafe { value.as_ref() }) else {
        return AclGrantView::default();
    };

    let Some(grant) = usize::try_from(index)
        .ok()
        .and_then(|index| value.grants().get(index))
    else {
        return AclGrantView::default();
    };

    grant_view(grant)
}

#[derive(Default)]
#[repr(C)]
pub(super) struct GetObjectAclOutputView {
    owner: AclOwnerView,
    grant_count: u64,
}

/// # Safety
/// A nonnull pointer must identify the matching live, aligned library allocation.
/// Keep it immutable and unreleased until all returned borrowed views expire.
#[unsafe(no_mangle)]
pub(super) unsafe extern "C" fn mikrio_v1_get_object_acl_output_view(
    value: *const mikrio::GetObjectAclOutput,
) -> GetObjectAclOutputView {
    // SAFETY: the caller keeps this immutable allocation live until all view use ends.
    let Some(value) = (unsafe { value.as_ref() }) else {
        return GetObjectAclOutputView::default();
    };

    GetObjectAclOutputView {
        owner: AclOwnerView {
            canonical_id: Span::borrowed(value.owner().canonical_id()),
        },
        grant_count: u64::try_from(value.grants().len()).unwrap_or(u64::MAX),
    }
}

release!(
    mikrio_v1_get_object_acl_output_release,
    mikrio::GetObjectAclOutput
);

/// # Safety
/// A nonnull pointer must identify the matching live, aligned library allocation.
/// Keep it immutable and unreleased until all returned borrowed views expire.
#[unsafe(no_mangle)]
pub(super) unsafe extern "C" fn mikrio_v1_get_object_acl_output_grant(
    value: *const mikrio::GetObjectAclOutput,
    index: u64,
) -> AclGrantView {
    // SAFETY: the caller retains the immutable result through this call and view use.
    let Some(value) = (unsafe { value.as_ref() }) else {
        return AclGrantView::default();
    };

    let Some(grant) = usize::try_from(index)
        .ok()
        .and_then(|index| value.grants().get(index))
    else {
        return AclGrantView::default();
    };

    grant_view(grant)
}

#[derive(Clone, Copy)]
#[repr(C)]
pub(super) struct PutBucketPolicyInput {
    struct_size: u64,
    bucket: Span,
    expected_bucket_owner: Span,
    document: Span,
}

input_layout!(PutBucketPolicyInput);

/// # Safety
/// Follow `include/mikrio.h`: keep handles and declared input bytes live and
/// immutable for this call. Input records and arrays must be aligned; output
/// slots must be aligned, writable and disjoint. Do not race handle release.
#[unsafe(no_mangle)]
pub(super) unsafe extern "C" fn mikrio_v1_store_put_bucket_policy(
    store: *const StoreHandle,
    input: *const PutBucketPolicyInput,
    error: *mut *mut Error,
) -> u32 {
    // SAFETY: live handle, fully readable sized input, and disjoint writable output slots.
    unsafe {
        entry(error, Some(MutationOutcome::NotCommitted), || {
            let owner = required(store)?.0.get()?;
            let input = sized(input)?;

            let mut builder =
                mikrio::PutBucketPolicyInput::builder(input.bucket.text()?, input.document.text()?);

            if let Some(account) = input.expected_bucket_owner.optional_text()? {
                builder = builder.expected_bucket_owner(account);
            }

            let input = builder.build()?;

            let store = owner.store.clone();

            let pending = owner
                .runtime
                .submit(async move { store.put_bucket_policy(input).await })?;

            handles::wait_mutation(pending)??;
            Ok(())
        })
    }
}

#[derive(Clone, Copy)]
#[repr(C)]
pub(super) struct GetBucketPolicyInput {
    struct_size: u64,
    bucket: Span,
    expected_bucket_owner: Span,
}

input_layout!(GetBucketPolicyInput, expected_bucket_owner);

/// # Safety
/// Follow `include/mikrio.h`: keep handles and declared input bytes live and
/// immutable for this call. Input records and arrays must be aligned; output
/// slots must be aligned, writable and disjoint. Do not race handle release.
#[unsafe(no_mangle)]
pub(super) unsafe extern "C" fn mikrio_v1_store_get_bucket_policy(
    store: *const StoreHandle,
    input: *const GetBucketPolicyInput,
    result: *mut *mut mikrio::GetBucketPolicyOutput,
    error: *mut *mut Error,
) -> u32 {
    // SAFETY: live handle, fully readable sized input, and disjoint writable output slots.
    unsafe {
        entry(error, None, || {
            let result = output(result)?;
            let owner = required(store)?.0.get()?;
            let input = sized(input)?;

            let mut builder = mikrio::GetBucketPolicyInput::builder(input.bucket.text()?);

            if let Some(account) = input.expected_bucket_owner.optional_text()? {
                builder = builder.expected_bucket_owner(account);
            }

            let input = builder.build()?;

            let store = owner.store.clone();

            let pending = owner
                .runtime
                .submit(async move { store.get_bucket_policy(input).await })?;

            let value = handles::wait(pending)??;
            *result = Box::into_raw(Box::new(value));
            Ok(())
        })
    }
}

#[derive(Clone, Copy)]
#[repr(C)]
pub(super) struct DeleteBucketPolicyInput {
    struct_size: u64,
    bucket: Span,
    expected_bucket_owner: Span,
}

input_layout!(DeleteBucketPolicyInput, expected_bucket_owner);

/// # Safety
/// Follow `include/mikrio.h`: keep handles and declared input bytes live and
/// immutable for this call. Input records and arrays must be aligned; output
/// slots must be aligned, writable and disjoint. Do not race handle release.
#[unsafe(no_mangle)]
pub(super) unsafe extern "C" fn mikrio_v1_store_delete_bucket_policy(
    store: *const StoreHandle,
    input: *const DeleteBucketPolicyInput,
    error: *mut *mut Error,
) -> u32 {
    // SAFETY: live handle, fully readable sized input, and disjoint writable output slots.
    unsafe {
        entry(error, Some(MutationOutcome::NotCommitted), || {
            let owner = required(store)?.0.get()?;
            let input = sized(input)?;

            let mut builder = mikrio::DeleteBucketPolicyInput::builder(input.bucket.text()?);

            if let Some(account) = input.expected_bucket_owner.optional_text()? {
                builder = builder.expected_bucket_owner(account);
            }

            let input = builder.build()?;

            let store = owner.store.clone();

            let pending = owner
                .runtime
                .submit(async move { store.delete_bucket_policy(input).await })?;

            handles::wait_mutation(pending)??;
            Ok(())
        })
    }
}

#[derive(Clone, Copy)]
#[repr(C)]
pub(super) struct GetBucketPolicyStatusInput {
    struct_size: u64,
    bucket: Span,
    expected_bucket_owner: Span,
}

input_layout!(GetBucketPolicyStatusInput, expected_bucket_owner);

/// # Safety
/// Follow `include/mikrio.h`: keep handles and declared input bytes live and
/// immutable for this call. Input records and arrays must be aligned; output
/// slots must be aligned, writable and disjoint. Do not race handle release.
#[unsafe(no_mangle)]
pub(super) unsafe extern "C" fn mikrio_v1_store_get_bucket_policy_status(
    store: *const StoreHandle,
    input: *const GetBucketPolicyStatusInput,
    result: *mut *mut mikrio::GetBucketPolicyStatusOutput,
    error: *mut *mut Error,
) -> u32 {
    // SAFETY: live handle, fully readable sized input, and disjoint writable output slots.
    unsafe {
        entry(error, None, || {
            let result = output(result)?;
            let owner = required(store)?.0.get()?;
            let input = sized(input)?;

            let mut builder = mikrio::GetBucketPolicyStatusInput::builder(input.bucket.text()?);

            if let Some(account) = input.expected_bucket_owner.optional_text()? {
                builder = builder.expected_bucket_owner(account);
            }

            let input = builder.build()?;

            let store = owner.store.clone();

            let pending = owner
                .runtime
                .submit(async move { store.get_bucket_policy_status(input).await })?;

            let value = handles::wait(pending)??;
            *result = Box::into_raw(Box::new(value));
            Ok(())
        })
    }
}

#[derive(Clone, Copy)]
#[repr(C)]
pub(super) struct PutPublicAccessBlockInput {
    struct_size: u64,
    bucket: Span,
    expected_bucket_owner: Span,
    configuration: PublicAccessBlockConfiguration,
}

input_layout!(PutPublicAccessBlockInput);

/// # Safety
/// Follow `include/mikrio.h`: keep handles and declared input bytes live and
/// immutable for this call. Input records and arrays must be aligned; output
/// slots must be aligned, writable and disjoint. Do not race handle release.
#[unsafe(no_mangle)]
pub(super) unsafe extern "C" fn mikrio_v1_store_put_public_access_block(
    store: *const StoreHandle,
    input: *const PutPublicAccessBlockInput,
    error: *mut *mut Error,
) -> u32 {
    // SAFETY: live handle, fully readable sized input, and disjoint writable output slots.
    unsafe {
        entry(error, Some(MutationOutcome::NotCommitted), || {
            let owner = required(store)?.0.get()?;
            let input = sized(input)?;

            let mut builder = mikrio::PutPublicAccessBlockInput::builder(
                input.bucket.text()?,
                input.configuration.try_into_native()?,
            );

            if let Some(account) = input.expected_bucket_owner.optional_text()? {
                builder = builder.expected_bucket_owner(account);
            }

            let input = builder.build()?;

            let store = owner.store.clone();

            let pending = owner
                .runtime
                .submit(async move { store.put_public_access_block(input).await })?;

            handles::wait_mutation(pending)??;
            Ok(())
        })
    }
}

#[derive(Clone, Copy)]
#[repr(C)]
pub(super) struct GetPublicAccessBlockInput {
    struct_size: u64,
    bucket: Span,
    expected_bucket_owner: Span,
}

input_layout!(GetPublicAccessBlockInput, expected_bucket_owner);

/// # Safety
/// Follow `include/mikrio.h`: keep handles and declared input bytes live and
/// immutable for this call. Input records and arrays must be aligned; output
/// slots must be aligned, writable and disjoint. Do not race handle release.
#[unsafe(no_mangle)]
pub(super) unsafe extern "C" fn mikrio_v1_store_get_public_access_block(
    store: *const StoreHandle,
    input: *const GetPublicAccessBlockInput,
    result: *mut *mut mikrio::GetPublicAccessBlockOutput,
    error: *mut *mut Error,
) -> u32 {
    // SAFETY: live handle, fully readable sized input, and disjoint writable output slots.
    unsafe {
        entry(error, None, || {
            let result = output(result)?;
            let owner = required(store)?.0.get()?;
            let input = sized(input)?;

            let mut builder = mikrio::GetPublicAccessBlockInput::builder(input.bucket.text()?);

            if let Some(account) = input.expected_bucket_owner.optional_text()? {
                builder = builder.expected_bucket_owner(account);
            }

            let input = builder.build()?;

            let store = owner.store.clone();

            let pending = owner
                .runtime
                .submit(async move { store.get_public_access_block(input).await })?;

            let value = handles::wait(pending)??;
            *result = Box::into_raw(Box::new(value));
            Ok(())
        })
    }
}

#[derive(Clone, Copy)]
#[repr(C)]
pub(super) struct DeletePublicAccessBlockInput {
    struct_size: u64,
    bucket: Span,
    expected_bucket_owner: Span,
}

input_layout!(DeletePublicAccessBlockInput, expected_bucket_owner);

/// # Safety
/// Follow `include/mikrio.h`: keep handles and declared input bytes live and
/// immutable for this call. Input records and arrays must be aligned; output
/// slots must be aligned, writable and disjoint. Do not race handle release.
#[unsafe(no_mangle)]
pub(super) unsafe extern "C" fn mikrio_v1_store_delete_public_access_block(
    store: *const StoreHandle,
    input: *const DeletePublicAccessBlockInput,
    error: *mut *mut Error,
) -> u32 {
    // SAFETY: live handle, fully readable sized input, and disjoint writable output slots.
    unsafe {
        entry(error, Some(MutationOutcome::NotCommitted), || {
            let owner = required(store)?.0.get()?;
            let input = sized(input)?;

            let mut builder = mikrio::DeletePublicAccessBlockInput::builder(input.bucket.text()?);

            if let Some(account) = input.expected_bucket_owner.optional_text()? {
                builder = builder.expected_bucket_owner(account);
            }

            let input = builder.build()?;

            let store = owner.store.clone();

            let pending = owner
                .runtime
                .submit(async move { store.delete_public_access_block(input).await })?;

            handles::wait_mutation(pending)??;
            Ok(())
        })
    }
}

#[derive(Clone, Copy)]
#[repr(C)]
pub(super) struct PutBucketOwnershipControlsInput {
    struct_size: u64,
    bucket: Span,
    expected_bucket_owner: Span,
    object_ownership: u32,
    reserved: u32,
}

input_layout!(PutBucketOwnershipControlsInput);

/// # Safety
/// Follow `include/mikrio.h`: keep handles and declared input bytes live and
/// immutable for this call. Input records and arrays must be aligned; output
/// slots must be aligned, writable and disjoint. Do not race handle release.
#[unsafe(no_mangle)]
pub(super) unsafe extern "C" fn mikrio_v1_store_put_bucket_ownership_controls(
    store: *const StoreHandle,
    input: *const PutBucketOwnershipControlsInput,
    error: *mut *mut Error,
) -> u32 {
    // SAFETY: live handle, fully readable sized input, and disjoint writable output slots.
    unsafe {
        entry(error, Some(MutationOutcome::NotCommitted), || {
            let owner = required(store)?.0.get()?;
            let input = sized(input)?;

            if input.object_ownership != 1 || input.reserved != 0 {
                return Err(Error::invalid(
                    "unsupported ownership mode or reserved field",
                ));
            }

            let mut builder = mikrio::PutBucketOwnershipControlsInput::builder(
                input.bucket.text()?,
                mikrio::ObjectOwnership::BucketOwnerEnforced,
            );

            if let Some(account) = input.expected_bucket_owner.optional_text()? {
                builder = builder.expected_bucket_owner(account);
            }

            let input = builder.build()?;

            let store = owner.store.clone();

            let pending = owner
                .runtime
                .submit(async move { store.put_bucket_ownership_controls(input).await })?;

            handles::wait_mutation(pending)??;
            Ok(())
        })
    }
}

#[derive(Clone, Copy)]
#[repr(C)]
pub(super) struct GetBucketOwnershipControlsInput {
    struct_size: u64,
    bucket: Span,
    expected_bucket_owner: Span,
}

input_layout!(GetBucketOwnershipControlsInput, expected_bucket_owner);

/// # Safety
/// Follow `include/mikrio.h`: keep handles and declared input bytes live and
/// immutable for this call. Input records and arrays must be aligned; output
/// slots must be aligned, writable and disjoint. Do not race handle release.
#[unsafe(no_mangle)]
pub(super) unsafe extern "C" fn mikrio_v1_store_get_bucket_ownership_controls(
    store: *const StoreHandle,
    input: *const GetBucketOwnershipControlsInput,
    result: *mut *mut mikrio::GetBucketOwnershipControlsOutput,
    error: *mut *mut Error,
) -> u32 {
    // SAFETY: live handle, fully readable sized input, and disjoint writable output slots.
    unsafe {
        entry(error, None, || {
            let result = output(result)?;
            let owner = required(store)?.0.get()?;
            let input = sized(input)?;

            let mut builder =
                mikrio::GetBucketOwnershipControlsInput::builder(input.bucket.text()?);

            if let Some(account) = input.expected_bucket_owner.optional_text()? {
                builder = builder.expected_bucket_owner(account);
            }

            let input = builder.build()?;

            let store = owner.store.clone();

            let pending = owner
                .runtime
                .submit(async move { store.get_bucket_ownership_controls(input).await })?;

            let value = handles::wait(pending)??;
            *result = Box::into_raw(Box::new(value));
            Ok(())
        })
    }
}

#[derive(Clone, Copy)]
#[repr(C)]
pub(super) struct GetBucketAclInput {
    struct_size: u64,
    bucket: Span,
    expected_bucket_owner: Span,
}

input_layout!(GetBucketAclInput, expected_bucket_owner);

/// # Safety
/// Follow `include/mikrio.h`: keep handles and declared input bytes live and
/// immutable for this call. Input records and arrays must be aligned; output
/// slots must be aligned, writable and disjoint. Do not race handle release.
#[unsafe(no_mangle)]
pub(super) unsafe extern "C" fn mikrio_v1_store_get_bucket_acl(
    store: *const StoreHandle,
    input: *const GetBucketAclInput,
    result: *mut *mut mikrio::GetBucketAclOutput,
    error: *mut *mut Error,
) -> u32 {
    // SAFETY: live handle, fully readable sized input, and disjoint writable output slots.
    unsafe {
        entry(error, None, || {
            let result = output(result)?;
            let owner = required(store)?.0.get()?;
            let input = sized(input)?;

            let mut builder = mikrio::GetBucketAclInput::builder(input.bucket.text()?);

            if let Some(account) = input.expected_bucket_owner.optional_text()? {
                builder = builder.expected_bucket_owner(account);
            }

            let input = builder.build()?;

            let store = owner.store.clone();

            let pending = owner
                .runtime
                .submit(async move { store.get_bucket_acl(input).await })?;

            let value = handles::wait(pending)??;
            *result = Box::into_raw(Box::new(value));
            Ok(())
        })
    }
}

#[derive(Clone, Copy)]
#[repr(C)]
pub(super) struct GetObjectAclInput {
    struct_size: u64,
    bucket: Span,
    key: Span,
    expected_bucket_owner: Span,
}

input_layout!(GetObjectAclInput, expected_bucket_owner);

/// # Safety
/// Follow `include/mikrio.h`: keep handles and declared input bytes live and
/// immutable for this call. Input records and arrays must be aligned; output
/// slots must be aligned, writable and disjoint. Do not race handle release.
#[unsafe(no_mangle)]
pub(super) unsafe extern "C" fn mikrio_v1_store_get_object_acl(
    store: *const StoreHandle,
    input: *const GetObjectAclInput,
    result: *mut *mut mikrio::GetObjectAclOutput,
    error: *mut *mut Error,
) -> u32 {
    // SAFETY: live handle, fully readable sized input, and disjoint writable output slots.
    unsafe {
        entry(error, None, || {
            let result = output(result)?;
            let owner = required(store)?.0.get()?;
            let input = sized(input)?;

            let mut builder =
                mikrio::GetObjectAclInput::builder(input.bucket.text()?, input.key.text()?);

            if let Some(account) = input.expected_bucket_owner.optional_text()? {
                builder = builder.expected_bucket_owner(account);
            }

            let input = builder.build()?;

            let store = owner.store.clone();

            let pending = owner
                .runtime
                .submit(async move { store.get_object_acl(input).await })?;

            let value = handles::wait(pending)??;
            *result = Box::into_raw(Box::new(value));
            Ok(())
        })
    }
}
