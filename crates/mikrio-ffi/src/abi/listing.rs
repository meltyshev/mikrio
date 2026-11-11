//! Sized listing input and detached page/item views with one allocation owner.

use super::{Error, OptionalU64, Span, StoreHandle, entry, handles, output, required, sized};

#[derive(Clone, Copy)]
#[repr(C)]
pub(super) struct ListObjectsInput {
    struct_size: u64,
    bucket: Span,
    expected_bucket_owner: Span,
    prefix: Span,
    delimiter: Span,
    max_keys: OptionalU64,
    continuation_token: Span,
    start_after: Span,
}

input_layout!(
    ListObjectsInput,
    expected_bucket_owner,
    prefix,
    delimiter,
    max_keys,
    continuation_token,
    start_after
);

#[derive(Default)]
#[repr(C)]
pub(super) struct ListObjectsOutputView {
    contents_count: u64,
    common_prefixes_count: u64,
    is_truncated: u32,
    reserved: u32,
    next_continuation_token: Span,
}

#[derive(Default)]
#[repr(C)]
pub(super) struct ObjectSummaryView {
    key: Span,
    size: u64,
    etag: Span,
    last_modified_at_ms: i64,
}

/// # Safety
/// Follow `include/mikrio.h`: keep handles and declared input bytes live and
/// immutable for this call. Input records and arrays must be aligned; output
/// slots must be aligned, writable and disjoint. Do not race handle release.
#[unsafe(no_mangle)]
pub(super) unsafe extern "C" fn mikrio_v1_store_list_objects_v2(
    store: *const StoreHandle,
    input: *const ListObjectsInput,
    result: *mut *mut mikrio::ListObjectsV2Output,
    error: *mut *mut Error,
) -> u32 {
    // SAFETY: live store, readable sized input/spans and disjoint writable slots.
    // All input strings are copied before submission; no foreign borrow escapes.
    unsafe {
        entry(error, None, || {
            let result = output(result)?;
            let owner = required(store)?.0.get()?;
            let input = sized(input)?;

            let mut builder = mikrio::ListObjectsV2Input::builder(input.bucket.text()?);

            if let Some(value) = input.expected_bucket_owner.optional_text()? {
                builder = builder.expected_bucket_owner(value);
            }

            if let Some(value) = input.prefix.optional_text()? {
                builder = builder.prefix(value);
            }

            if let Some(value) = input.delimiter.optional_text()? {
                builder = builder.delimiter(value);
            }

            if let Some(value) = input.continuation_token.optional_text()? {
                builder = builder.continuation_token(value);
            }

            if let Some(value) = input.start_after.optional_text()? {
                builder = builder.start_after(value);
            }

            if let Some(value) = input.max_keys.get()? {
                builder = builder.max_keys(
                    u32::try_from(value).map_err(|_| Error::invalid("invalid page size"))?,
                );
            }

            let input = builder.build()?;

            let store = owner.store.clone();

            let pending = owner
                .runtime
                .submit(async move { store.list_objects_v2(input).await })?;

            *result = Box::into_raw(Box::new(handles::wait(pending)??));
            Ok(())
        })
    }
}

/// # Safety
/// A nonnull pointer must identify the matching live, aligned library allocation.
/// Keep it immutable and unreleased until all returned borrowed views expire.
#[unsafe(no_mangle)]
pub(super) unsafe extern "C" fn mikrio_v1_list_objects_v2_output_view(
    value: *const mikrio::ListObjectsV2Output,
) -> ListObjectsOutputView {
    // SAFETY: caller retains the immutable page allocation until every view expires.
    let Some(value) = (unsafe { value.as_ref() }) else {
        return ListObjectsOutputView::default();
    };

    ListObjectsOutputView {
        contents_count: u64::try_from(value.contents().len()).unwrap_or(u64::MAX),
        common_prefixes_count: u64::try_from(value.common_prefixes().len()).unwrap_or(u64::MAX),
        is_truncated: u32::from(value.is_truncated()),
        reserved: 0,
        next_continuation_token: Span::optional(value.next_continuation_token()),
    }
}

/// # Safety
/// A nonnull pointer must identify the matching live, aligned library allocation.
/// Keep it immutable and unreleased until all returned borrowed views expire.
#[unsafe(no_mangle)]
pub(super) unsafe extern "C" fn mikrio_v1_list_objects_v2_output_item(
    value: *const mikrio::ListObjectsV2Output,
    index: u64,
) -> ObjectSummaryView {
    // SAFETY: caller keeps the immutable page allocation live during view use.
    let Some(value) = (unsafe { value.as_ref() }) else {
        return ObjectSummaryView::default();
    };

    let Some(object) = usize::try_from(index)
        .ok()
        .and_then(|index| value.contents().get(index))
    else {
        return ObjectSummaryView::default();
    };

    ObjectSummaryView {
        key: Span::borrowed(object.key()),
        size: object.size(),
        etag: Span::borrowed(object.etag()),
        last_modified_at_ms: object.last_modified().as_unix_millis(),
    }
}

/// # Safety
/// A nonnull pointer must identify the matching live, aligned library allocation.
/// Keep it immutable and unreleased until all returned borrowed views expire.
#[unsafe(no_mangle)]
pub(super) unsafe extern "C" fn mikrio_v1_list_objects_v2_output_prefix(
    value: *const mikrio::ListObjectsV2Output,
    index: u64,
) -> Span {
    // SAFETY: caller keeps the immutable page allocation live during view use.
    let Some(value) = (unsafe { value.as_ref() }) else {
        return Span::default();
    };

    usize::try_from(index)
        .ok()
        .and_then(|index| value.common_prefixes().get(index))
        .map_or_else(Span::default, |prefix| Span::borrowed(prefix.prefix()))
}

release!(
    mikrio_v1_list_objects_v2_output_release,
    mikrio::ListObjectsV2Output
);

#[derive(Clone, Copy)]
#[repr(C)]
pub(super) struct ListBucketsInput {
    struct_size: u64,
    prefix: Span,
    max_buckets: OptionalU64,
    continuation_token: Span,
}

input_layout!(ListBucketsInput, prefix, max_buckets, continuation_token);

#[derive(Default)]
#[repr(C)]
pub(super) struct ListBucketsOutputView {
    buckets_count: u64,
    continuation_token: Span,
}

#[derive(Default)]
#[repr(C)]
pub(super) struct BucketSummaryView {
    name: Span,
    creation_date_ms: i64,
    bucket_region: Span,
}

/// # Safety
/// Follow `include/mikrio.h`: keep handles and declared input bytes live and
/// immutable for this call. Input records and arrays must be aligned; output
/// slots must be aligned, writable and disjoint. Do not race handle release.
#[unsafe(no_mangle)]
pub(super) unsafe extern "C" fn mikrio_v1_store_list_buckets(
    store: *const StoreHandle,
    input: *const ListBucketsInput,
    result: *mut *mut mikrio::ListBucketsOutput,
    error: *mut *mut Error,
) -> u32 {
    // SAFETY: live store, readable sized input/spans and disjoint writable slots.
    // Strings are copied before submission and no foreign borrow escapes.
    unsafe {
        entry(error, None, || {
            let result = output(result)?;
            let owner = required(store)?.0.get()?;
            let input = sized(input)?;

            let mut builder = mikrio::ListBucketsInput::builder();

            if let Some(value) = input.prefix.optional_text()? {
                builder = builder.prefix(value);
            }

            if let Some(value) = input.continuation_token.optional_text()? {
                builder = builder.continuation_token(value);
            }

            if let Some(value) = input.max_buckets.get()? {
                builder = builder.max_buckets(
                    u32::try_from(value).map_err(|_| Error::invalid("invalid page size"))?,
                );
            }

            let input = builder.build()?;

            let store = owner.store.clone();

            let pending = owner
                .runtime
                .submit(async move { store.list_buckets(input).await })?;

            *result = Box::into_raw(Box::new(handles::wait(pending)??));
            Ok(())
        })
    }
}

/// # Safety
/// A nonnull pointer must identify the matching live, aligned library allocation.
/// Keep it immutable and unreleased until all returned borrowed views expire.
#[unsafe(no_mangle)]
pub(super) unsafe extern "C" fn mikrio_v1_list_buckets_output_view(
    value: *const mikrio::ListBucketsOutput,
) -> ListBucketsOutputView {
    // SAFETY: caller retains the immutable page allocation until every view expires.
    let Some(value) = (unsafe { value.as_ref() }) else {
        return ListBucketsOutputView::default();
    };

    ListBucketsOutputView {
        buckets_count: u64::try_from(value.buckets().len()).unwrap_or(u64::MAX),
        continuation_token: Span::optional(value.continuation_token()),
    }
}

/// # Safety
/// A nonnull pointer must identify the matching live, aligned library allocation.
/// Keep it immutable and unreleased until all returned borrowed views expire.
#[unsafe(no_mangle)]
pub(super) unsafe extern "C" fn mikrio_v1_list_buckets_output_item(
    value: *const mikrio::ListBucketsOutput,
    index: u64,
) -> BucketSummaryView {
    // SAFETY: caller keeps the immutable page allocation live during view use.
    let Some(value) = (unsafe { value.as_ref() }) else {
        return BucketSummaryView::default();
    };

    let Some(bucket) = usize::try_from(index)
        .ok()
        .and_then(|index| value.buckets().get(index))
    else {
        return BucketSummaryView::default();
    };

    BucketSummaryView {
        name: Span::borrowed(bucket.name()),
        creation_date_ms: bucket.creation_date().as_unix_millis(),
        bucket_region: Span::borrowed(bucket.bucket_region()),
    }
}

release!(
    mikrio_v1_list_buckets_output_release,
    mikrio::ListBucketsOutput
);
