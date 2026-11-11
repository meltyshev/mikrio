//! Borrow lifecycle arrays only for the call; detached views borrow owned output.

use super::{
    Error, MutationOutcome, OptionalU64, Span, StoreHandle, entry, handles, output, required, sized,
};

#[repr(C)]
pub(super) struct LifecycleRuleInput {
    struct_size: u64,
    id: Span,
    status: u32,
    reserved: u32,
    prefix: Span,
    expiration_days: OptionalU64,
    abort_days_after_initiation: OptionalU64,
}

fn decode_days(value: &OptionalU64) -> Result<Option<u32>, Error> {
    super::optional(value.present, value.reserved, value.value)?
        .map(|value| u32::try_from(value).map_err(|_| Error::invalid("invalid lifecycle days")))
        .transpose()
}

unsafe fn decode_rules(
    pointer: *const LifecycleRuleInput,
    count: u64,
) -> Result<Vec<mikrio::LifecycleRule>, Error> {
    if count == 0 || count > 1000 || pointer.is_null() {
        return Err(Error::invalid("invalid lifecycle rule array"));
    }

    let count =
        usize::try_from(count).map_err(|_| Error::invalid("lifecycle count exceeds host range"))?;

    // SAFETY: caller supplies count aligned, initialized canonical-stride elements.
    let entries = unsafe { std::slice::from_raw_parts(pointer, count) };
    let mut result = Vec::with_capacity(count);

    for entry in entries {
        if entry.struct_size
            != u64::try_from(std::mem::size_of::<LifecycleRuleInput>()).unwrap_or(u64::MAX)
            || entry.reserved != 0
        {
            return Err(Error::invalid("invalid lifecycle rule element layout"));
        }

        let status = match entry.status {
            1 => mikrio::LifecycleStatus::Enabled,
            2 => mikrio::LifecycleStatus::Disabled,
            _ => return Err(Error::invalid("invalid lifecycle status")),
        };

        // SAFETY: both spans borrow live immutable caller memory for this call.
        let (prefix, id) = unsafe { (entry.prefix.optional_text()?, entry.id.optional_text()?) };
        let filter = mikrio::LifecycleFilter::new(prefix.map(str::to_owned))?;
        let mut builder = mikrio::LifecycleRule::builder(status, filter);

        if let Some(id) = id {
            builder = builder.id(id);
        }

        if let Some(days) = decode_days(&entry.expiration_days)? {
            builder = builder.expiration(mikrio::LifecycleExpiration::new(days)?);
        }

        if let Some(days) = decode_days(&entry.abort_days_after_initiation)? {
            builder = builder.abort_incomplete_multipart_upload(
                mikrio::AbortIncompleteMultipartUpload::new(days)?,
            );
        }

        result.push(builder.build()?);
    }

    Ok(result)
}

#[derive(Default)]
#[repr(C)]
pub(super) struct GetBucketLifecycleConfigurationOutputView {
    lifecycle_rule_count: u64,
}

#[derive(Default)]
#[repr(C)]
pub(super) struct LifecycleRuleView {
    id: Span,
    status: u32,
    reserved: u32,
    prefix: Span,
    expiration_days: OptionalU64,
    abort_days_after_initiation: OptionalU64,
}

fn optional_days(value: Option<u32>) -> OptionalU64 {
    value.map_or_else(OptionalU64::default, |value| OptionalU64 {
        present: 1,
        reserved: 0,
        value: u64::from(value),
    })
}

/// # Safety
/// A nonnull pointer must identify the matching live, aligned library allocation
/// and remain immutable and unreleased for this call.
#[unsafe(no_mangle)]
pub(super) unsafe extern "C" fn mikrio_v1_get_bucket_lifecycle_configuration_output_view(
    value: *const mikrio::GetBucketLifecycleConfigurationOutput,
) -> GetBucketLifecycleConfigurationOutputView {
    // SAFETY: caller retains this immutable output through all view use.
    unsafe { value.as_ref() }.map_or_else(
        GetBucketLifecycleConfigurationOutputView::default,
        |value| GetBucketLifecycleConfigurationOutputView {
            lifecycle_rule_count: u64::try_from(value.lifecycle_rules().len()).unwrap_or(u64::MAX),
        },
    )
}

release!(
    mikrio_v1_get_bucket_lifecycle_configuration_output_release,
    mikrio::GetBucketLifecycleConfigurationOutput
);

/// # Safety
/// A nonnull pointer must identify the matching live, aligned library allocation.
/// Keep it immutable and unreleased until all returned borrowed views expire.
#[unsafe(no_mangle)]
pub(super) unsafe extern "C" fn mikrio_v1_get_bucket_lifecycle_configuration_output_rule(
    value: *const mikrio::GetBucketLifecycleConfigurationOutput,
    index: u64,
) -> LifecycleRuleView {
    // SAFETY: caller retains this immutable output through all view use.
    let Some(value) = (unsafe { value.as_ref() }) else {
        return LifecycleRuleView::default();
    };

    let Some(rule) = usize::try_from(index)
        .ok()
        .and_then(|index| value.lifecycle_rules().get(index))
    else {
        return LifecycleRuleView::default();
    };

    LifecycleRuleView {
        id: Span::optional(rule.id()),
        status: match rule.status() {
            mikrio::LifecycleStatus::Enabled => 1,
            mikrio::LifecycleStatus::Disabled => 2,
        },
        reserved: 0,
        prefix: Span::optional(rule.filter().prefix()),
        expiration_days: optional_days(rule.expiration().map(mikrio::LifecycleExpiration::days)),
        abort_days_after_initiation: optional_days(
            rule.abort_incomplete_multipart_upload()
                .map(mikrio::AbortIncompleteMultipartUpload::days_after_initiation),
        ),
    }
}

#[derive(Clone, Copy)]
#[repr(C)]
pub(super) struct PutBucketLifecycleConfigurationInput {
    struct_size: u64,
    bucket: Span,
    expected_bucket_owner: Span,
    lifecycle_rules: *const LifecycleRuleInput,
    lifecycle_rule_count: u64,
}

input_layout!(PutBucketLifecycleConfigurationInput);

/// # Safety
/// Follow `include/mikrio.h`: keep handles and declared input bytes live and
/// immutable for this call. Input records and arrays must be aligned; output
/// slots must be aligned, writable and disjoint. Do not race handle release.
#[unsafe(no_mangle)]
pub(super) unsafe extern "C" fn mikrio_v1_store_put_bucket_lifecycle_configuration(
    store: *const StoreHandle,
    input: *const PutBucketLifecycleConfigurationInput,
    error: *mut *mut Error,
) -> u32 {
    // SAFETY: live handle, fully readable sized input, and disjoint writable output slots.
    unsafe {
        entry(error, Some(MutationOutcome::NotCommitted), || {
            let owner = required(store)?.0.get()?;
            let input = sized(input)?;

            let mut builder = mikrio::PutBucketLifecycleConfigurationInput::builder(
                input.bucket.text()?,
                decode_rules(input.lifecycle_rules, input.lifecycle_rule_count)?,
            );

            if let Some(account) = input.expected_bucket_owner.optional_text()? {
                builder = builder.expected_bucket_owner(account);
            }

            let input = builder.build()?;

            let store = owner.store.clone();

            let pending = owner
                .runtime
                .submit(async move { store.put_bucket_lifecycle_configuration(input).await })?;

            handles::wait_mutation(pending)??;
            Ok(())
        })
    }
}

#[derive(Clone, Copy)]
#[repr(C)]
pub(super) struct GetBucketLifecycleConfigurationInput {
    struct_size: u64,
    bucket: Span,
    expected_bucket_owner: Span,
}

input_layout!(GetBucketLifecycleConfigurationInput, expected_bucket_owner);

/// # Safety
/// Follow `include/mikrio.h`: keep handles and declared input bytes live and
/// immutable for this call. Input records and arrays must be aligned; output
/// slots must be aligned, writable and disjoint. Do not race handle release.
#[unsafe(no_mangle)]
pub(super) unsafe extern "C" fn mikrio_v1_store_get_bucket_lifecycle_configuration(
    store: *const StoreHandle,
    input: *const GetBucketLifecycleConfigurationInput,
    result: *mut *mut mikrio::GetBucketLifecycleConfigurationOutput,
    error: *mut *mut Error,
) -> u32 {
    // SAFETY: live handle, fully readable sized input, and disjoint writable output slots.
    unsafe {
        entry(error, None, || {
            let result = output(result)?;
            let owner = required(store)?.0.get()?;
            let input = sized(input)?;

            let mut builder =
                mikrio::GetBucketLifecycleConfigurationInput::builder(input.bucket.text()?);

            if let Some(account) = input.expected_bucket_owner.optional_text()? {
                builder = builder.expected_bucket_owner(account);
            }

            let input = builder.build()?;

            let store = owner.store.clone();

            let pending = owner
                .runtime
                .submit(async move { store.get_bucket_lifecycle_configuration(input).await })?;

            let value = handles::wait(pending)??;
            *result = Box::into_raw(Box::new(value));
            Ok(())
        })
    }
}

#[derive(Clone, Copy)]
#[repr(C)]
pub(super) struct DeleteBucketLifecycleInput {
    struct_size: u64,
    bucket: Span,
    expected_bucket_owner: Span,
}

input_layout!(DeleteBucketLifecycleInput, expected_bucket_owner);

/// # Safety
/// Follow `include/mikrio.h`: keep handles and declared input bytes live and
/// immutable for this call. Input records and arrays must be aligned; output
/// slots must be aligned, writable and disjoint. Do not race handle release.
#[unsafe(no_mangle)]
pub(super) unsafe extern "C" fn mikrio_v1_store_delete_bucket_lifecycle(
    store: *const StoreHandle,
    input: *const DeleteBucketLifecycleInput,
    error: *mut *mut Error,
) -> u32 {
    // SAFETY: live handle, fully readable sized input, and disjoint writable output slots.
    unsafe {
        entry(error, Some(MutationOutcome::NotCommitted), || {
            let owner = required(store)?.0.get()?;
            let input = sized(input)?;

            let mut builder = mikrio::DeleteBucketLifecycleInput::builder(input.bucket.text()?);

            if let Some(account) = input.expected_bucket_owner.optional_text()? {
                builder = builder.expected_bucket_owner(account);
            }

            let input = builder.build()?;

            let store = owner.store.clone();

            let pending = owner
                .runtime
                .submit(async move { store.delete_bucket_lifecycle(input).await })?;

            handles::wait_mutation(pending)??;
            Ok(())
        })
    }
}
