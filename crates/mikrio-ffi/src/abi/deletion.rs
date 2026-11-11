//! Detached batch arrays and error-owned progress; foreign inputs never escape.

use super::{Error, MutationOutcome, Span, StoreHandle, entry, handles, output, required, sized};

#[repr(C)]
pub(super) struct ObjectIdentifier {
    key: Span,
    etag: Span,
}

#[derive(Clone, Copy)]
#[repr(C)]
pub(super) struct DeleteObjectsInput {
    struct_size: u64,
    bucket: Span,
    expected_bucket_owner: Span,
    objects: *const ObjectIdentifier,
    objects_count: u64,
    quiet: u32,
    reserved: u32,
}

input_layout!(DeleteObjectsInput, quiet);

#[derive(Default)]
#[repr(C)]
pub(super) struct DeleteObjectsOutputView {
    deleted_count: u64,
    errors_count: u64,
}

#[derive(Default)]
#[repr(C)]
pub(super) struct DeleteObjectErrorView {
    key: Span,
    code: Span,
    message: Span,
}

#[derive(Default)]
#[repr(C)]
pub(super) struct DeleteObjectProgressView {
    index: u64,
    key: Span,
    mutation_outcome: u32,
    reserved: u32,
    error_code: Span,
    error_message: Span,
}

/// # Safety
/// Follow `include/mikrio.h`: keep handles and declared input bytes live and
/// immutable for this call. Input records and arrays must be aligned; output
/// slots must be aligned, writable and disjoint. Do not race handle release.
#[unsafe(no_mangle)]
pub(super) unsafe extern "C" fn mikrio_v1_store_delete_objects(
    store: *const StoreHandle,
    input: *const DeleteObjectsInput,
    result: *mut *mut mikrio::DeleteObjectsOutput,
    error: *mut *mut Error,
) -> u32 {
    // SAFETY: caller supplies live handles, readable input/array/spans and
    // disjoint output slots. Every borrowed input is copied before submission.
    unsafe {
        entry(error, Some(MutationOutcome::NotCommitted), || {
            let result = output(result)?;
            let owner = required(store)?.0.get()?;
            let input = sized(input)?;

            if input.quiet > 1 || input.reserved != 0 {
                return Err(Error::invalid("invalid quiet flag or reserved field"));
            }

            if !(1..=1000).contains(&input.objects_count) || input.objects.is_null() {
                return Err(Error::invalid(
                    "delete batch requires 1-1000 readable entries",
                ));
            }

            let count = usize::try_from(input.objects_count)
                .map_err(|_| Error::invalid("invalid entry count"))?;

            let mut objects = Vec::with_capacity(count);

            for object in std::slice::from_raw_parts(input.objects, count) {
                let mut builder = mikrio::ObjectIdentifier::builder(object.key.text()?);

                if let Some(etag) = object.etag.optional_text()? {
                    builder = builder.etag(etag);
                }

                objects.push(builder.build()?);
            }

            let mut builder = mikrio::DeleteObjectsInput::builder(input.bucket.text()?, objects)
                .quiet(input.quiet == 1);

            if let Some(owner) = input.expected_bucket_owner.optional_text()? {
                builder = builder.expected_bucket_owner(owner);
            }

            let input = builder.build()?;

            let store = owner.store.clone();

            let pending = owner
                .runtime
                .submit(async move { store.delete_objects(input).await })?;

            let value = handles::wait_mutation(pending)??;
            *result = Box::into_raw(Box::new(value));
            Ok(())
        })
    }
}

/// # Safety
/// A nonnull pointer must identify the matching live, aligned library allocation
/// and remain immutable and unreleased for this call.
#[unsafe(no_mangle)]
pub(super) unsafe extern "C" fn mikrio_v1_delete_objects_output_view(
    value: *const mikrio::DeleteObjectsOutput,
) -> DeleteObjectsOutputView {
    // SAFETY: caller retains the immutable allocation throughout view use.
    unsafe { value.as_ref() }.map_or_else(DeleteObjectsOutputView::default, |value| {
        DeleteObjectsOutputView {
            deleted_count: u64::try_from(value.deleted().len()).unwrap_or(u64::MAX),
            errors_count: u64::try_from(value.errors().len()).unwrap_or(u64::MAX),
        }
    })
}

/// # Safety
/// A nonnull pointer must identify the matching live, aligned library allocation.
/// Keep it immutable and unreleased until all returned borrowed views expire.
#[unsafe(no_mangle)]
pub(super) unsafe extern "C" fn mikrio_v1_delete_objects_output_deleted(
    value: *const mikrio::DeleteObjectsOutput,
    index: u64,
) -> Span {
    // SAFETY: caller retains the immutable allocation throughout view use.
    unsafe { value.as_ref() }
        .and_then(|value| {
            usize::try_from(index)
                .ok()
                .and_then(|index| value.deleted().get(index))
        })
        .map_or_else(Span::default, |item| Span::borrowed(item.key()))
}

/// # Safety
/// A nonnull pointer must identify the matching live, aligned library allocation.
/// Keep it immutable and unreleased until all returned borrowed views expire.
#[unsafe(no_mangle)]
pub(super) unsafe extern "C" fn mikrio_v1_delete_objects_output_error(
    value: *const mikrio::DeleteObjectsOutput,
    index: u64,
) -> DeleteObjectErrorView {
    // SAFETY: caller retains the immutable allocation throughout view use.
    let item = unsafe { value.as_ref() }.and_then(|value| {
        usize::try_from(index)
            .ok()
            .and_then(|index| value.errors().get(index))
    });

    item.map_or_else(DeleteObjectErrorView::default, |item| {
        DeleteObjectErrorView {
            key: Span::borrowed(item.key()),
            code: Span::borrowed(item.code().as_str()),
            message: Span::borrowed(item.message()),
        }
    })
}

/// # Safety
/// A nonnull pointer must identify the matching live, aligned library allocation
/// and remain immutable and unreleased for this call.
#[unsafe(no_mangle)]
pub(super) unsafe extern "C" fn mikrio_v1_error_delete_objects_progress_count(
    error: *const Error,
) -> u64 {
    // SAFETY: caller retains the immutable error throughout view use.
    unsafe { error.as_ref() }
        .and_then(|error| error.partial_result.as_ref())
        .map_or(0, |progress| {
            u64::try_from(progress.items().len()).unwrap_or(u64::MAX)
        })
}

/// # Safety
/// A nonnull pointer must identify the matching live, aligned library allocation.
/// Keep it immutable and unreleased until all returned borrowed views expire.
#[unsafe(no_mangle)]
pub(super) unsafe extern "C" fn mikrio_v1_error_delete_objects_progress_item(
    error: *const Error,
    index: u64,
) -> DeleteObjectProgressView {
    // SAFETY: caller retains the immutable error throughout view use. Item and
    // message spans borrow that error, including after its store is released.
    let item = unsafe { error.as_ref() }
        .and_then(|error| error.partial_result.as_ref())
        .and_then(|progress| {
            usize::try_from(index)
                .ok()
                .and_then(|index| progress.items().get(index))
        });

    item.map_or_else(DeleteObjectProgressView::default, |item| {
        DeleteObjectProgressView {
            index: u64::try_from(item.index()).unwrap_or(u64::MAX),
            key: Span::borrowed(item.key()),
            mutation_outcome: match item.mutation_outcome() {
                MutationOutcome::NotCommitted => 1,
                MutationOutcome::Committed => 2,
                _ => 3,
            },
            reserved: 0,
            error_code: Span::optional(item.error().map(|error| error.code().as_str())),
            error_message: Span::optional(item.error().map(mikrio::DeleteError::message)),
        }
    })
}

release!(
    mikrio_v1_delete_objects_output_release,
    mikrio::DeleteObjectsOutput
);
