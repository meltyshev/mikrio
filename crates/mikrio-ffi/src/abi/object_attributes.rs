//! Sized Attributes requests and borrowed views of detached owned results.

use super::{Error, OptionalU64, Span, StoreHandle, entry, handles, output, required, sized};

#[derive(Clone, Copy)]
#[repr(C)]
pub(super) struct GetObjectAttributesInput {
    struct_size: u64,
    bucket: Span,
    key: Span,
    expected_bucket_owner: Span,
    object_attributes: *const Span,
    object_attributes_count: u64,
    max_parts: OptionalU64,
    part_number_marker: OptionalU64,
}

input_layout!(GetObjectAttributesInput, max_parts, part_number_marker);

unsafe fn decode_selectors(
    pointer: *const Span,
    count: u64,
) -> Result<Vec<mikrio::ObjectAttribute>, Error> {
    if pointer.is_null() || !(1..=5).contains(&count) {
        return Err(Error::invalid(
            "Attributes requires one to five readable selectors",
        ));
    }

    let count = usize::try_from(count).map_err(|_| Error::invalid("invalid selector count"))?;

    // SAFETY: caller supplies this bounded, aligned, fixed-stride array and its
    // nested spans for this call. Only owned validated enum values escape.
    unsafe {
        std::slice::from_raw_parts(pointer, count)
            .iter()
            .map(|value| Ok(mikrio::ObjectAttribute::parse(value.text()?)?))
            .collect()
    }
}

/// # Safety
/// Follow `include/mikrio.h`: keep handles and declared input bytes live and
/// immutable for this call. Input records and arrays must be aligned; output
/// slots must be aligned, writable and disjoint. Do not race handle release.
#[unsafe(no_mangle)]
pub(super) unsafe extern "C" fn mikrio_v1_store_get_object_attributes(
    store: *const StoreHandle,
    input: *const GetObjectAttributesInput,
    result: *mut *mut mikrio::GetObjectAttributesOutput,
    error: *mut *mut Error,
) -> u32 {
    // SAFETY: live handle, readable sized inputs, and disjoint writable output slots.
    unsafe {
        entry(error, None, || {
            let result = output(result)?;
            let owner = required(store)?.0.get()?;
            let input = sized(input)?;

            let mut builder = mikrio::GetObjectAttributesInput::builder(
                input.bucket.text()?,
                input.key.text()?,
                decode_selectors(input.object_attributes, input.object_attributes_count)?,
            );

            if let Some(value) = input.expected_bucket_owner.optional_text()? {
                builder = builder.expected_bucket_owner(value);
            }

            if let Some(value) = input.max_parts.get()? {
                builder = builder.max_parts(decode_count(value)?);
            }

            if let Some(value) = input.part_number_marker.get()? {
                builder = builder.part_number_marker(decode_count(value)?);
            }

            let input = builder.build()?;

            let store = owner.store.clone();

            let pending = owner
                .runtime
                .submit(async move { store.get_object_attributes(input).await })?;

            *result = Box::into_raw(Box::new(handles::wait(pending)??));
            Ok(())
        })
    }
}

fn decode_count(value: u64) -> Result<u32, Error> {
    u32::try_from(value).map_err(|_| Error::invalid("count exceeds supported range"))
}

fn optional(value: Option<u64>) -> OptionalU64 {
    value.map_or_else(OptionalU64::default, |value| OptionalU64 {
        present: 1,
        reserved: 0,
        value,
    })
}

#[derive(Default)]
#[repr(C)]
#[allow(
    clippy::struct_field_names,
    reason = "fields match the public C schema"
)]
pub(super) struct ChecksumView {
    checksum_crc32: Span,
    checksum_crc32c: Span,
    checksum_sha1: Span,
    checksum_sha256: Span,
    checksum_crc64_nvme: Span,
    checksum_type: Span,
}

impl From<&mikrio::Checksum> for ChecksumView {
    fn from(value: &mikrio::Checksum) -> Self {
        Self {
            checksum_crc32: Span::optional(value.checksum_crc32()),
            checksum_crc32c: Span::optional(value.checksum_crc32c()),
            checksum_sha1: Span::optional(value.checksum_sha1()),
            checksum_sha256: Span::optional(value.checksum_sha256()),
            checksum_crc64_nvme: Span::optional(value.checksum_crc64_nvme()),
            checksum_type: Span::optional(value.checksum_type().map(mikrio::ChecksumType::as_str)),
        }
    }
}

#[derive(Default)]
#[repr(C)]
pub(super) struct ObjectPartsView {
    total_parts_count: u64,
    max_parts: u64,
    part_number_marker: u64,
    parts_count: u64,
    is_truncated: u32,
    reserved: u32,
    next_part_number_marker: OptionalU64,
}

impl From<&mikrio::ObjectParts> for ObjectPartsView {
    fn from(value: &mikrio::ObjectParts) -> Self {
        Self {
            total_parts_count: u64::from(value.total_parts_count()),
            max_parts: u64::from(value.max_parts()),
            part_number_marker: u64::from(value.part_number_marker()),
            parts_count: u64::try_from(value.parts().len()).unwrap_or(u64::MAX),
            is_truncated: u32::from(value.is_truncated()),
            reserved: 0,
            next_part_number_marker: optional(value.next_part_number_marker().map(u64::from)),
        }
    }
}

#[derive(Default)]
#[repr(C)]
pub(super) struct GetObjectAttributesOutputView {
    object_size: OptionalU64,
    etag: Span,
    checksum_present: u32,
    reserved: u32,
    checksum: ChecksumView,
    storage_class: Span,
    object_parts_present: u32,
    reserved2: u32,
    object_parts: ObjectPartsView,
    last_modified_unix_millis: i64,
}

/// # Safety
/// A nonnull pointer must identify the matching live, aligned library allocation.
/// Keep it immutable and unreleased until all returned borrowed views expire.
#[unsafe(no_mangle)]
pub(super) unsafe extern "C" fn mikrio_v1_get_object_attributes_output_view(
    value: *const mikrio::GetObjectAttributesOutput,
) -> GetObjectAttributesOutputView {
    // SAFETY: caller retains this immutable output through all borrowed views.
    unsafe { value.as_ref() }.map_or_else(GetObjectAttributesOutputView::default, |value| {
        GetObjectAttributesOutputView {
            object_size: optional(value.object_size()),
            etag: Span::optional(value.etag()),
            checksum_present: u32::from(value.checksum().is_some()),
            reserved: 0,
            checksum: value
                .checksum()
                .map_or_else(ChecksumView::default, ChecksumView::from),
            storage_class: Span::optional(value.storage_class()),
            object_parts_present: u32::from(value.object_parts().is_some()),
            reserved2: 0,
            object_parts: value
                .object_parts()
                .map_or_else(ObjectPartsView::default, ObjectPartsView::from),
            last_modified_unix_millis: value.last_modified().as_unix_millis(),
        }
    })
}

#[derive(Default)]
#[repr(C)]
pub(super) struct ObjectPartView {
    present: u32,
    reserved: u32,
    part_number: u64,
    size: u64,
    checksum_crc32: Span,
    checksum_crc32c: Span,
    checksum_sha1: Span,
    checksum_sha256: Span,
    checksum_crc64_nvme: Span,
}

/// # Safety
/// A nonnull pointer must identify the matching live, aligned library allocation.
/// Keep it immutable and unreleased until all returned borrowed views expire.
#[unsafe(no_mangle)]
pub(super) unsafe extern "C" fn mikrio_v1_get_object_attributes_output_part(
    value: *const mikrio::GetObjectAttributesOutput,
    index: u64,
) -> ObjectPartView {
    // SAFETY: caller retains this immutable output through all borrowed views.
    let part = unsafe { value.as_ref() }
        .and_then(|value| value.object_parts())
        .and_then(|value| {
            usize::try_from(index)
                .ok()
                .and_then(|index| value.parts().get(index))
        });

    part.map_or_else(ObjectPartView::default, |value| ObjectPartView {
        present: 1,
        reserved: 0,
        part_number: u64::from(value.part_number()),
        size: value.size(),
        checksum_crc32: Span::optional(value.checksum_crc32()),
        checksum_crc32c: Span::optional(value.checksum_crc32c()),
        checksum_sha1: Span::optional(value.checksum_sha1()),
        checksum_sha256: Span::optional(value.checksum_sha256()),
        checksum_crc64_nvme: Span::optional(value.checksum_crc64_nvme()),
    })
}

release!(
    mikrio_v1_get_object_attributes_output_release,
    mikrio::GetObjectAttributesOutput
);
