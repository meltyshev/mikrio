//! Borrowed tag inputs and detached output views at the C boundary.

use super::{
    Error, MutationOutcome, Pair, Span, StoreHandle, TAG_LIMITS, entry, handles, output, pairs,
    required, sized,
};

#[derive(Default)]
#[repr(C)]
pub(super) struct GetObjectTaggingOutputView {
    tag_count: u64,
}

/// # Safety
/// A nonnull pointer must identify the matching live, aligned library allocation
/// and remain immutable and unreleased for this call.
#[unsafe(no_mangle)]
pub(super) unsafe extern "C" fn mikrio_v1_get_object_tagging_output_view(
    value: *const mikrio::GetObjectTaggingOutput,
) -> GetObjectTaggingOutputView {
    // SAFETY: caller retains the immutable output through every borrowed view.
    unsafe { value.as_ref() }.map_or_else(GetObjectTaggingOutputView::default, |value| {
        GetObjectTaggingOutputView {
            tag_count: u64::try_from(value.tags().len()).unwrap_or(u64::MAX),
        }
    })
}

/// # Safety
/// A nonnull pointer must identify the matching live, aligned library allocation.
/// Keep it immutable and unreleased until all returned borrowed views expire.
#[unsafe(no_mangle)]
pub(super) unsafe extern "C" fn mikrio_v1_get_object_tagging_output_tag(
    value: *const mikrio::GetObjectTaggingOutput,
    index: u64,
) -> Pair {
    // SAFETY: caller retains the immutable output through every borrowed view.
    let tag = unsafe { value.as_ref() }.and_then(|value| {
        usize::try_from(index)
            .ok()
            .and_then(|index| value.tags().get(index))
    });

    tag.map_or(
        Pair {
            key: Span::default(),
            value: Span::default(),
        },
        |tag| Pair {
            key: Span::borrowed(&tag.key),
            value: Span::borrowed(&tag.value),
        },
    )
}

release!(
    mikrio_v1_get_object_tagging_output_release,
    mikrio::GetObjectTaggingOutput
);

#[derive(Clone, Copy)]
#[repr(C)]
pub(super) struct GetObjectTaggingInput {
    struct_size: u64,
    bucket: Span,
    key: Span,
    expected_bucket_owner: Span,
}

input_layout!(GetObjectTaggingInput, expected_bucket_owner);

/// # Safety
/// Follow `include/mikrio.h`: keep handles and declared input bytes live and
/// immutable for this call. Input records and arrays must be aligned; output
/// slots must be aligned, writable and disjoint. Do not race handle release.
#[unsafe(no_mangle)]
pub(super) unsafe extern "C" fn mikrio_v1_store_get_object_tagging(
    store: *const StoreHandle,
    input: *const GetObjectTaggingInput,
    result: *mut *mut mikrio::GetObjectTaggingOutput,
    error: *mut *mut Error,
) -> u32 {
    // SAFETY: live handle, readable sized input and disjoint writable output slots.
    unsafe {
        entry(error, None, || {
            let result = output(result)?;
            let owner = required(store)?.0.get()?;
            let input = sized(input)?;

            let mut builder =
                mikrio::GetObjectTaggingInput::builder(input.bucket.text()?, input.key.text()?);

            if let Some(account) = input.expected_bucket_owner.optional_text()? {
                builder = builder.expected_bucket_owner(account);
            }

            let input = builder.build()?;

            let store = owner.store.clone();

            let pending = owner
                .runtime
                .submit(async move { store.get_object_tagging(input).await })?;

            let value = handles::wait(pending)??;
            *result = Box::into_raw(Box::new(value));
            Ok(())
        })
    }
}

#[derive(Clone, Copy)]
#[repr(C)]
pub(super) struct PutObjectTaggingInput {
    struct_size: u64,
    bucket: Span,
    key: Span,
    expected_bucket_owner: Span,
    tags: *const Pair,
    tag_count: u64,
}

input_layout!(PutObjectTaggingInput);

/// # Safety
/// Follow `include/mikrio.h`: keep handles and declared input bytes live and
/// immutable for this call. Input records and arrays must be aligned; output
/// slots must be aligned, writable and disjoint. Do not race handle release.
#[unsafe(no_mangle)]
pub(super) unsafe extern "C" fn mikrio_v1_store_put_object_tagging(
    store: *const StoreHandle,
    input: *const PutObjectTaggingInput,
    error: *mut *mut Error,
) -> u32 {
    // SAFETY: live handle, readable sized input and disjoint writable output slots.
    unsafe {
        entry(error, Some(MutationOutcome::NotCommitted), || {
            let owner = required(store)?.0.get()?;
            let input = sized(input)?;

            let mut builder = mikrio::PutObjectTaggingInput::builder(
                input.bucket.text()?,
                input.key.text()?,
                pairs(input.tags, input.tag_count, TAG_LIMITS)?
                    .into_iter()
                    .map(|(key, value)| mikrio::Tag { key, value })
                    .collect(),
            );

            if let Some(account) = input.expected_bucket_owner.optional_text()? {
                builder = builder.expected_bucket_owner(account);
            }

            let input = builder.build()?;

            let store = owner.store.clone();

            let pending = owner
                .runtime
                .submit(async move { store.put_object_tagging(input).await })?;

            handles::wait_mutation(pending)??;
            Ok(())
        })
    }
}

#[derive(Clone, Copy)]
#[repr(C)]
pub(super) struct DeleteObjectTaggingInput {
    struct_size: u64,
    bucket: Span,
    key: Span,
    expected_bucket_owner: Span,
}

input_layout!(DeleteObjectTaggingInput, expected_bucket_owner);

/// # Safety
/// Follow `include/mikrio.h`: keep handles and declared input bytes live and
/// immutable for this call. Input records and arrays must be aligned; output
/// slots must be aligned, writable and disjoint. Do not race handle release.
#[unsafe(no_mangle)]
pub(super) unsafe extern "C" fn mikrio_v1_store_delete_object_tagging(
    store: *const StoreHandle,
    input: *const DeleteObjectTaggingInput,
    error: *mut *mut Error,
) -> u32 {
    // SAFETY: live handle, readable sized input and disjoint writable output slots.
    unsafe {
        entry(error, Some(MutationOutcome::NotCommitted), || {
            let owner = required(store)?.0.get()?;
            let input = sized(input)?;

            let mut builder =
                mikrio::DeleteObjectTaggingInput::builder(input.bucket.text()?, input.key.text()?);

            if let Some(account) = input.expected_bucket_owner.optional_text()? {
                builder = builder.expected_bucket_owner(account);
            }

            let input = builder.build()?;

            let store = owner.store.clone();

            let pending = owner
                .runtime
                .submit(async move { store.delete_object_tagging(input).await })?;

            handles::wait_mutation(pending)??;
            Ok(())
        })
    }
}
