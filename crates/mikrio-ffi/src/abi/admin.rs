//! Sized C administration inputs and detached output views.

use super::{
    Error, MutationOutcome, OptionalU64, Span, StoreHandle, entry, handles, output, required, sized,
};

#[derive(Default)]
#[repr(C)]
pub(super) struct OwnerInfoView {
    account_id: Span,
    canonical_id: Span,
}

/// # Safety
/// A nonnull pointer must identify the matching live, aligned library allocation.
/// Keep it immutable and unreleased until all returned borrowed views expire.
#[unsafe(no_mangle)]
pub(super) unsafe extern "C" fn mikrio_v1_owner_info_view(
    value: *const mikrio::OwnerInfo,
) -> OwnerInfoView {
    // SAFETY: the caller retains this detached allocation throughout view use.
    let Some(value) = (unsafe { value.as_ref() }) else {
        return OwnerInfoView::default();
    };

    OwnerInfoView {
        account_id: Span::borrowed(value.account_id()),
        canonical_id: Span::borrowed(value.canonical_id()),
    }
}

release!(mikrio_v1_owner_info_release, mikrio::OwnerInfo);

#[derive(Default)]
#[repr(C)]
pub(super) struct PrincipalInfoView {
    principal_id: Span,
    name: Span,
    is_enabled: u32,
    reserved: u32,
    created_at_ms: i64,
}

/// # Safety
/// A nonnull pointer must identify the matching live, aligned library allocation.
/// Keep it immutable and unreleased until all returned borrowed views expire.
#[unsafe(no_mangle)]
pub(super) unsafe extern "C" fn mikrio_v1_principal_info_view(
    value: *const mikrio::PrincipalInfo,
) -> PrincipalInfoView {
    // SAFETY: the caller retains this detached allocation throughout view use.
    let Some(value) = (unsafe { value.as_ref() }) else {
        return PrincipalInfoView::default();
    };

    principal_view(value)
}

release!(mikrio_v1_principal_info_release, mikrio::PrincipalInfo);

#[derive(Default)]
#[repr(C)]
pub(super) struct AccessKeyInfoView {
    access_key_id: Span,
    principal_id: Span,
    created_at_ms: i64,
}

fn access_key_view(value: &mikrio::AccessKeyInfo) -> AccessKeyInfoView {
    AccessKeyInfoView {
        access_key_id: Span::borrowed(value.access_key_id()),
        principal_id: Span::borrowed(value.principal_id()),
        created_at_ms: value.created_at().as_unix_millis(),
    }
}

#[derive(Default)]
#[repr(C)]
pub(super) struct CreateAccessKeyOutputView {
    access_key: AccessKeyInfoView,
    secret_access_key: Span,
}

/// # Safety
/// A nonnull pointer must identify the matching live, aligned library allocation.
/// Keep it immutable and unreleased until all returned borrowed views expire.
#[unsafe(no_mangle)]
pub(super) unsafe extern "C" fn mikrio_v1_create_access_key_output_view(
    value: *const mikrio::CreateAccessKeyOutput,
) -> CreateAccessKeyOutputView {
    // SAFETY: the caller retains this detached allocation throughout view use.
    let Some(value) = (unsafe { value.as_ref() }) else {
        return CreateAccessKeyOutputView::default();
    };

    CreateAccessKeyOutputView {
        access_key: access_key_view(value.access_key()),
        secret_access_key: Span::borrowed(value.secret_access_key()),
    }
}

release!(
    mikrio_v1_create_access_key_output_release,
    mikrio::CreateAccessKeyOutput
);

#[derive(Default)]
#[repr(C)]
pub(super) struct ListPrincipalsOutputView {
    count: u64,
    next_continuation_token: Span,
}

/// # Safety
/// A nonnull pointer must identify the matching live, aligned library allocation.
/// Keep it immutable and unreleased until all returned borrowed views expire.
#[unsafe(no_mangle)]
pub(super) unsafe extern "C" fn mikrio_v1_list_principals_output_view(
    value: *const mikrio::ListPrincipalsOutput,
) -> ListPrincipalsOutputView {
    // SAFETY: the caller retains this detached allocation throughout view use.
    let Some(value) = (unsafe { value.as_ref() }) else {
        return ListPrincipalsOutputView::default();
    };

    ListPrincipalsOutputView {
        count: u64::try_from(value.principals().len()).unwrap_or(u64::MAX),
        next_continuation_token: Span::optional(value.next_continuation_token()),
    }
}

release!(
    mikrio_v1_list_principals_output_release,
    mikrio::ListPrincipalsOutput
);

#[derive(Default)]
#[repr(C)]
pub(super) struct ListAccessKeysOutputView {
    count: u64,
    next_continuation_token: Span,
}

/// # Safety
/// A nonnull pointer must identify the matching live, aligned library allocation.
/// Keep it immutable and unreleased until all returned borrowed views expire.
#[unsafe(no_mangle)]
pub(super) unsafe extern "C" fn mikrio_v1_list_access_keys_output_view(
    value: *const mikrio::ListAccessKeysOutput,
) -> ListAccessKeysOutputView {
    // SAFETY: the caller retains this detached allocation throughout view use.
    let Some(value) = (unsafe { value.as_ref() }) else {
        return ListAccessKeysOutputView::default();
    };

    ListAccessKeysOutputView {
        count: u64::try_from(value.access_keys().len()).unwrap_or(u64::MAX),
        next_continuation_token: Span::optional(value.next_continuation_token()),
    }
}

release!(
    mikrio_v1_list_access_keys_output_release,
    mikrio::ListAccessKeysOutput
);

#[derive(Default)]
#[repr(C)]
pub(super) struct GetPrincipalPolicyOutputView {
    document: Span,
}

/// # Safety
/// A nonnull pointer must identify the matching live, aligned library allocation.
/// Keep it immutable and unreleased until all returned borrowed views expire.
#[unsafe(no_mangle)]
pub(super) unsafe extern "C" fn mikrio_v1_get_principal_policy_output_view(
    value: *const mikrio::GetPrincipalPolicyOutput,
) -> GetPrincipalPolicyOutputView {
    // SAFETY: the caller retains this detached allocation throughout view use.
    let Some(value) = (unsafe { value.as_ref() }) else {
        return GetPrincipalPolicyOutputView::default();
    };

    GetPrincipalPolicyOutputView {
        document: Span::borrowed(value.document()),
    }
}

release!(
    mikrio_v1_get_principal_policy_output_release,
    mikrio::GetPrincipalPolicyOutput
);

/// # Safety
/// A nonnull pointer must identify the matching live, aligned library allocation.
/// Keep it immutable and unreleased until all returned borrowed views expire.
#[unsafe(no_mangle)]
pub(super) unsafe extern "C" fn mikrio_v1_list_principals_output_item(
    value: *const mikrio::ListPrincipalsOutput,
    index: u64,
) -> PrincipalInfoView {
    // SAFETY: allocation is live and immutable until its matching release.
    let Some(value) = (unsafe { value.as_ref() }) else {
        return PrincipalInfoView::default();
    };

    let Some(item) = usize::try_from(index)
        .ok()
        .and_then(|index| value.principals().get(index))
    else {
        return PrincipalInfoView::default();
    };

    principal_view(item)
}

/// # Safety
/// A nonnull pointer must identify the matching live, aligned library allocation.
/// Keep it immutable and unreleased until all returned borrowed views expire.
#[unsafe(no_mangle)]
pub(super) unsafe extern "C" fn mikrio_v1_list_access_keys_output_item(
    value: *const mikrio::ListAccessKeysOutput,
    index: u64,
) -> AccessKeyInfoView {
    // SAFETY: allocation is live and immutable until its matching release.
    let Some(value) = (unsafe { value.as_ref() }) else {
        return AccessKeyInfoView::default();
    };

    let Some(item) = usize::try_from(index)
        .ok()
        .and_then(|index| value.access_keys().get(index))
    else {
        return AccessKeyInfoView::default();
    };

    access_key_view(item)
}

fn principal_view(value: &mikrio::PrincipalInfo) -> PrincipalInfoView {
    PrincipalInfoView {
        principal_id: Span::borrowed(value.principal_id()),
        name: Span::borrowed(value.name()),
        is_enabled: u32::from(value.is_enabled()),
        reserved: 0,
        created_at_ms: value.created_at().as_unix_millis(),
    }
}

/// # Safety
/// Keep the aligned handle live and immutable throughout this call, without
/// racing release. Output slots must be aligned, writable and disjoint, as
/// specified in `include/mikrio.h`.
#[unsafe(no_mangle)]
pub(super) unsafe extern "C" fn mikrio_v1_store_get_owner_info(
    store: *const StoreHandle,
    result: *mut *mut mikrio::OwnerInfo,
    error: *mut *mut Error,
) -> u32 {
    // SAFETY: live handle and disjoint writable output slots.
    unsafe {
        entry(error, None, || {
            let result = output(result)?;
            let owner = required(store)?.0.get()?;
            let store = owner.store.clone();

            let pending = owner
                .runtime
                .submit(async move { store.get_owner_info().await })?;

            let value = handles::wait(pending)??;
            *result = Box::into_raw(Box::new(value));
            Ok(())
        })
    }
}

#[derive(Clone, Copy)]
#[repr(C)]
pub(super) struct CreatePrincipalInput {
    struct_size: u64,
    name: Span,
}

input_layout!(CreatePrincipalInput);

/// # Safety
/// Follow `include/mikrio.h`: keep handles and declared input bytes live and
/// immutable for this call. Input records and arrays must be aligned; output
/// slots must be aligned, writable and disjoint. Do not race handle release.
#[unsafe(no_mangle)]
pub(super) unsafe extern "C" fn mikrio_v1_store_create_principal(
    store: *const StoreHandle,
    input: *const CreatePrincipalInput,
    result: *mut *mut mikrio::PrincipalInfo,
    error: *mut *mut Error,
) -> u32 {
    // SAFETY: live handle, sized borrowed input and disjoint writable output slots.
    unsafe {
        entry(error, Some(MutationOutcome::NotCommitted), || {
            let result = output(result)?;
            let owner = required(store)?.0.get()?;
            let input = sized(input)?;

            let builder = mikrio::CreatePrincipalInput::builder(input.name.text()?);
            let input = builder.build()?;

            let store = owner.store.clone();

            let pending = owner
                .runtime
                .submit(async move { store.create_principal(input).await })?;

            let value = handles::wait_mutation(pending)??;
            *result = Box::into_raw(Box::new(value));
            Ok(())
        })
    }
}

#[derive(Clone, Copy)]
#[repr(C)]
pub(super) struct GetPrincipalInput {
    struct_size: u64,
    principal_id: Span,
}

input_layout!(GetPrincipalInput);

/// # Safety
/// Follow `include/mikrio.h`: keep handles and declared input bytes live and
/// immutable for this call. Input records and arrays must be aligned; output
/// slots must be aligned, writable and disjoint. Do not race handle release.
#[unsafe(no_mangle)]
pub(super) unsafe extern "C" fn mikrio_v1_store_get_principal(
    store: *const StoreHandle,
    input: *const GetPrincipalInput,
    result: *mut *mut mikrio::PrincipalInfo,
    error: *mut *mut Error,
) -> u32 {
    // SAFETY: live handle, sized borrowed input and disjoint writable output slots.
    unsafe {
        entry(error, None, || {
            let result = output(result)?;
            let owner = required(store)?.0.get()?;
            let input = sized(input)?;

            let builder = mikrio::GetPrincipalInput::builder(input.principal_id.text()?);
            let input = builder.build()?;

            let store = owner.store.clone();

            let pending = owner
                .runtime
                .submit(async move { store.get_principal(input).await })?;

            let value = handles::wait(pending)??;
            *result = Box::into_raw(Box::new(value));
            Ok(())
        })
    }
}

#[derive(Clone, Copy)]
#[repr(C)]
pub(super) struct SetPrincipalEnabledInput {
    struct_size: u64,
    principal_id: Span,
    is_enabled: u32,
    reserved: u32,
}

input_layout!(SetPrincipalEnabledInput);

/// # Safety
/// Follow `include/mikrio.h`: keep handles and declared input bytes live and
/// immutable for this call. Input records and arrays must be aligned; output
/// slots must be aligned, writable and disjoint. Do not race handle release.
#[unsafe(no_mangle)]
pub(super) unsafe extern "C" fn mikrio_v1_store_set_principal_enabled(
    store: *const StoreHandle,
    input: *const SetPrincipalEnabledInput,
    result: *mut *mut mikrio::PrincipalInfo,
    error: *mut *mut Error,
) -> u32 {
    // SAFETY: live handle, sized borrowed input and disjoint writable output slots.
    unsafe {
        entry(error, Some(MutationOutcome::NotCommitted), || {
            let result = output(result)?;
            let owner = required(store)?.0.get()?;
            let input = sized(input)?;

            if input.is_enabled > 1 || input.reserved != 0 {
                return Err(Error::invalid("invalid enabled flag or reserved field"));
            }

            let builder = mikrio::SetPrincipalEnabledInput::builder(
                input.principal_id.text()?,
                input.is_enabled == 1,
            );

            let input = builder.build()?;

            let store = owner.store.clone();

            let pending = owner
                .runtime
                .submit(async move { store.set_principal_enabled(input).await })?;

            let value = handles::wait_mutation(pending)??;
            *result = Box::into_raw(Box::new(value));
            Ok(())
        })
    }
}

#[derive(Clone, Copy)]
#[repr(C)]
pub(super) struct DeletePrincipalInput {
    struct_size: u64,
    principal_id: Span,
}

input_layout!(DeletePrincipalInput);

/// # Safety
/// Follow `include/mikrio.h`: keep handles and declared input bytes live and
/// immutable for this call. Input records and arrays must be aligned; output
/// slots must be aligned, writable and disjoint. Do not race handle release.
#[unsafe(no_mangle)]
pub(super) unsafe extern "C" fn mikrio_v1_store_delete_principal(
    store: *const StoreHandle,
    input: *const DeletePrincipalInput,
    error: *mut *mut Error,
) -> u32 {
    // SAFETY: live handle, sized borrowed input and disjoint writable output slots.
    unsafe {
        entry(error, Some(MutationOutcome::NotCommitted), || {
            let owner = required(store)?.0.get()?;
            let input = sized(input)?;

            let builder = mikrio::DeletePrincipalInput::builder(input.principal_id.text()?);
            let input = builder.build()?;

            let store = owner.store.clone();

            let pending = owner
                .runtime
                .submit(async move { store.delete_principal(input).await })?;

            handles::wait_mutation(pending)??;
            Ok(())
        })
    }
}

#[derive(Clone, Copy)]
#[repr(C)]
pub(super) struct PutPrincipalPolicyInput {
    struct_size: u64,
    principal_id: Span,
    document: Span,
}

input_layout!(PutPrincipalPolicyInput);

/// # Safety
/// Follow `include/mikrio.h`: keep handles and declared input bytes live and
/// immutable for this call. Input records and arrays must be aligned; output
/// slots must be aligned, writable and disjoint. Do not race handle release.
#[unsafe(no_mangle)]
pub(super) unsafe extern "C" fn mikrio_v1_store_put_principal_policy(
    store: *const StoreHandle,
    input: *const PutPrincipalPolicyInput,
    error: *mut *mut Error,
) -> u32 {
    // SAFETY: live handle, sized borrowed input and disjoint writable output slots.
    unsafe {
        entry(error, Some(MutationOutcome::NotCommitted), || {
            let owner = required(store)?.0.get()?;
            let input = sized(input)?;

            let builder = mikrio::PutPrincipalPolicyInput::builder(
                input.principal_id.text()?,
                input.document.text()?,
            );

            let input = builder.build()?;

            let store = owner.store.clone();

            let pending = owner
                .runtime
                .submit(async move { store.put_principal_policy(input).await })?;

            handles::wait_mutation(pending)??;
            Ok(())
        })
    }
}

#[derive(Clone, Copy)]
#[repr(C)]
pub(super) struct GetPrincipalPolicyInput {
    struct_size: u64,
    principal_id: Span,
}

input_layout!(GetPrincipalPolicyInput);

/// # Safety
/// Follow `include/mikrio.h`: keep handles and declared input bytes live and
/// immutable for this call. Input records and arrays must be aligned; output
/// slots must be aligned, writable and disjoint. Do not race handle release.
#[unsafe(no_mangle)]
pub(super) unsafe extern "C" fn mikrio_v1_store_get_principal_policy(
    store: *const StoreHandle,
    input: *const GetPrincipalPolicyInput,
    result: *mut *mut mikrio::GetPrincipalPolicyOutput,
    error: *mut *mut Error,
) -> u32 {
    // SAFETY: live handle, sized borrowed input and disjoint writable output slots.
    unsafe {
        entry(error, None, || {
            let result = output(result)?;
            let owner = required(store)?.0.get()?;
            let input = sized(input)?;

            let builder = mikrio::GetPrincipalPolicyInput::builder(input.principal_id.text()?);
            let input = builder.build()?;

            let store = owner.store.clone();

            let pending = owner
                .runtime
                .submit(async move { store.get_principal_policy(input).await })?;

            let value = handles::wait(pending)??;
            *result = Box::into_raw(Box::new(value));
            Ok(())
        })
    }
}

#[derive(Clone, Copy)]
#[repr(C)]
pub(super) struct DeletePrincipalPolicyInput {
    struct_size: u64,
    principal_id: Span,
}

input_layout!(DeletePrincipalPolicyInput);

/// # Safety
/// Follow `include/mikrio.h`: keep handles and declared input bytes live and
/// immutable for this call. Input records and arrays must be aligned; output
/// slots must be aligned, writable and disjoint. Do not race handle release.
#[unsafe(no_mangle)]
pub(super) unsafe extern "C" fn mikrio_v1_store_delete_principal_policy(
    store: *const StoreHandle,
    input: *const DeletePrincipalPolicyInput,
    error: *mut *mut Error,
) -> u32 {
    // SAFETY: live handle, sized borrowed input and disjoint writable output slots.
    unsafe {
        entry(error, Some(MutationOutcome::NotCommitted), || {
            let owner = required(store)?.0.get()?;
            let input = sized(input)?;

            let builder = mikrio::DeletePrincipalPolicyInput::builder(input.principal_id.text()?);
            let input = builder.build()?;

            let store = owner.store.clone();

            let pending = owner
                .runtime
                .submit(async move { store.delete_principal_policy(input).await })?;

            handles::wait_mutation(pending)??;
            Ok(())
        })
    }
}

#[derive(Clone, Copy)]
#[repr(C)]
pub(super) struct CreateAccessKeyInput {
    struct_size: u64,
    principal_id: Span,
}

input_layout!(CreateAccessKeyInput);

/// # Safety
/// Follow `include/mikrio.h`: keep handles and declared input bytes live and
/// immutable for this call. Input records and arrays must be aligned; output
/// slots must be aligned, writable and disjoint. Do not race handle release.
#[unsafe(no_mangle)]
pub(super) unsafe extern "C" fn mikrio_v1_store_create_access_key(
    store: *const StoreHandle,
    input: *const CreateAccessKeyInput,
    result: *mut *mut mikrio::CreateAccessKeyOutput,
    error: *mut *mut Error,
) -> u32 {
    // SAFETY: live handle, sized borrowed input and disjoint writable output slots.
    unsafe {
        entry(error, Some(MutationOutcome::NotCommitted), || {
            let result = output(result)?;
            let owner = required(store)?.0.get()?;
            let input = sized(input)?;

            let builder = mikrio::CreateAccessKeyInput::builder(input.principal_id.text()?);
            let input = builder.build()?;

            let store = owner.store.clone();

            let pending = owner
                .runtime
                .submit(async move { store.create_access_key(input).await })?;

            let value = handles::wait_mutation(pending)??;
            *result = Box::into_raw(Box::new(value));
            Ok(())
        })
    }
}

#[derive(Clone, Copy)]
#[repr(C)]
pub(super) struct DeleteAccessKeyInput {
    struct_size: u64,
    principal_id: Span,
    access_key_id: Span,
}

input_layout!(DeleteAccessKeyInput);

/// # Safety
/// Follow `include/mikrio.h`: keep handles and declared input bytes live and
/// immutable for this call. Input records and arrays must be aligned; output
/// slots must be aligned, writable and disjoint. Do not race handle release.
#[unsafe(no_mangle)]
pub(super) unsafe extern "C" fn mikrio_v1_store_delete_access_key(
    store: *const StoreHandle,
    input: *const DeleteAccessKeyInput,
    error: *mut *mut Error,
) -> u32 {
    // SAFETY: live handle, sized borrowed input and disjoint writable output slots.
    unsafe {
        entry(error, Some(MutationOutcome::NotCommitted), || {
            let owner = required(store)?.0.get()?;
            let input = sized(input)?;

            let builder = mikrio::DeleteAccessKeyInput::builder(
                input.principal_id.text()?,
                input.access_key_id.text()?,
            );

            let input = builder.build()?;

            let store = owner.store.clone();

            let pending = owner
                .runtime
                .submit(async move { store.delete_access_key(input).await })?;

            handles::wait_mutation(pending)??;
            Ok(())
        })
    }
}

#[derive(Clone, Copy)]
#[repr(C)]
pub(super) struct ListPrincipalsInput {
    struct_size: u64,
    max_results: OptionalU64,
    continuation_token: Span,
}

input_layout!(ListPrincipalsInput, max_results, continuation_token);

/// # Safety
/// Follow `include/mikrio.h`: keep handles and declared input bytes live and
/// immutable for this call. Input records and arrays must be aligned; output
/// slots must be aligned, writable and disjoint. Do not race handle release.
#[unsafe(no_mangle)]
pub(super) unsafe extern "C" fn mikrio_v1_store_list_principals(
    store: *const StoreHandle,
    input: *const ListPrincipalsInput,
    result: *mut *mut mikrio::ListPrincipalsOutput,
    error: *mut *mut Error,
) -> u32 {
    // SAFETY: live handle, sized borrowed input and disjoint writable output slots.
    unsafe {
        entry(error, None, || {
            let result = output(result)?;
            let owner = required(store)?.0.get()?;
            let input = sized(input)?;

            let mut builder = mikrio::ListPrincipalsInput::builder();

            if let Some(size) = input.max_results.get()? {
                builder = builder.max_results(
                    u32::try_from(size).map_err(|_| Error::invalid("invalid page size"))?,
                );
            }

            if let Some(token) = input.continuation_token.optional_text()? {
                builder = builder.continuation_token(token);
            }

            let input = builder.build()?;

            let store = owner.store.clone();

            let pending = owner
                .runtime
                .submit(async move { store.list_principals(input).await })?;

            let value = handles::wait(pending)??;
            *result = Box::into_raw(Box::new(value));
            Ok(())
        })
    }
}

#[derive(Clone, Copy)]
#[repr(C)]
pub(super) struct ListAccessKeysInput {
    struct_size: u64,
    principal_id: Span,
    max_results: OptionalU64,
    continuation_token: Span,
}

input_layout!(ListAccessKeysInput, max_results, continuation_token);

/// # Safety
/// Follow `include/mikrio.h`: keep handles and declared input bytes live and
/// immutable for this call. Input records and arrays must be aligned; output
/// slots must be aligned, writable and disjoint. Do not race handle release.
#[unsafe(no_mangle)]
pub(super) unsafe extern "C" fn mikrio_v1_store_list_access_keys(
    store: *const StoreHandle,
    input: *const ListAccessKeysInput,
    result: *mut *mut mikrio::ListAccessKeysOutput,
    error: *mut *mut Error,
) -> u32 {
    // SAFETY: live handle, sized borrowed input and disjoint writable output slots.
    unsafe {
        entry(error, None, || {
            let result = output(result)?;
            let owner = required(store)?.0.get()?;
            let input = sized(input)?;

            let mut builder = mikrio::ListAccessKeysInput::builder(input.principal_id.text()?);

            if let Some(size) = input.max_results.get()? {
                builder = builder.max_results(
                    u32::try_from(size).map_err(|_| Error::invalid("invalid page size"))?,
                );
            }

            if let Some(token) = input.continuation_token.optional_text()? {
                builder = builder.continuation_token(token);
            }

            let input = builder.build()?;

            let store = owner.store.clone();

            let pending = owner
                .runtime
                .submit(async move { store.list_access_keys(input).await })?;

            let value = handles::wait(pending)??;
            *result = Box::into_raw(Box::new(value));
            Ok(())
        })
    }
}
