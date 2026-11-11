//! Sized COPY/multipart inputs and independent result allocations at the C ABI.

use mikrio_embed::ProcessBound;

use super::{
    Error, METADATA_LIMITS, MutationOutcome, OptionalI64, OptionalU64, Pair, Span, StoreHandle,
    TAG_LIMITS, Writer, WriterHandle, entry, handles, output, pairs, parse_checksum_algorithm,
    required, sized,
};

#[derive(Clone, Copy)]
#[repr(C)]
pub(super) struct ObjectRef {
    bucket: Span,
    key: Span,
}

#[repr(C)]
pub(super) struct CompletedPart {
    part_number: u32,
    reserved: u32,
    etag: Span,
    checksum_crc32: Span,
    checksum_crc32c: Span,
    checksum_sha1: Span,
    checksum_sha256: Span,
    checksum_crc64_nvme: Span,
}

fn parse_checksum_type(value: &str) -> Result<mikrio::ChecksumType, Error> {
    match value {
        "FULL_OBJECT" => Ok(mikrio::ChecksumType::FullObject),
        "COMPOSITE" => Ok(mikrio::ChecksumType::Composite),
        _ => Err(Error::invalid("unsupported checksum type")),
    }
}

fn parse_copy_directive(value: &str) -> Result<mikrio::CopyDirective, Error> {
    match value {
        "COPY" => Ok(mikrio::CopyDirective::Copy),
        "REPLACE" => Ok(mikrio::CopyDirective::Replace),
        _ => Err(Error::invalid("unsupported copy directive")),
    }
}

unsafe fn decode_completed_parts(
    pointer: *const CompletedPart,
    count: u64,
) -> Result<Vec<mikrio::CompletedPart>, Error> {
    if pointer.is_null() || !(1..=10_000).contains(&count) {
        return Err(Error::invalid("completion requires 1-10000 readable parts"));
    }

    let count = usize::try_from(count).map_err(|_| Error::invalid("invalid part count"))?;

    // SAFETY: the caller supplies this bounded aligned fixed-stride array and all
    // nested spans for the synchronous call. Only owned facade values escape.
    unsafe {
        let mut parts = Vec::with_capacity(count);

        for part in std::slice::from_raw_parts(pointer, count) {
            if part.reserved != 0 {
                return Err(Error::invalid("nonzero completed-part reserved field"));
            }

            let mut builder = mikrio::CompletedPart::builder(part.part_number, part.etag.text()?);

            if let Some(value) = part.checksum_crc32.optional_text()? {
                builder = builder.checksum_crc32(value);
            }

            if let Some(value) = part.checksum_crc32c.optional_text()? {
                builder = builder.checksum_crc32c(value);
            }

            if let Some(value) = part.checksum_sha1.optional_text()? {
                builder = builder.checksum_sha1(value);
            }

            if let Some(value) = part.checksum_sha256.optional_text()? {
                builder = builder.checksum_sha256(value);
            }

            if let Some(value) = part.checksum_crc64_nvme.optional_text()? {
                builder = builder.checksum_crc64_nvme(value);
            }

            parts.push(builder.build()?);
        }

        Ok(parts)
    }
}

#[derive(Clone, Copy)]
#[repr(C)]
pub(super) struct CopyObjectInput {
    struct_size: u64,
    bucket: Span,
    key: Span,
    copy_source: ObjectRef,
    expected_bucket_owner: Span,
    expected_source_bucket_owner: Span,
    copy_source_if_match: Span,
    copy_source_if_none_match: Span,
    copy_source_if_modified_since: OptionalI64,
    copy_source_if_unmodified_since: OptionalI64,
    metadata_directive: Span,
    content_type: Span,
    content_encoding: Span,
    content_disposition: Span,
    content_language: Span,
    cache_control: Span,
    expires: OptionalI64,
    metadata: *const Pair,
    metadata_count: u64,
    tags: *const Pair,
    tags_count: u64,
    tagging_directive: Span,
    checksum_algorithm: Span,
    if_match: Span,
    if_none_match: Span,
}

input_layout!(CopyObjectInput);

/// # Safety
/// Follow `include/mikrio.h`: keep handles and declared input bytes live and
/// immutable for this call. Input records and arrays must be aligned; output
/// slots must be aligned, writable and disjoint. Do not race handle release.
#[unsafe(no_mangle)]
pub(super) unsafe extern "C" fn mikrio_v1_store_copy_object(
    store: *const StoreHandle,
    input: *const CopyObjectInput,
    result: *mut *mut mikrio::CopyObjectOutput,
    error: *mut *mut Error,
) -> u32 {
    // SAFETY: the caller retains readable sized inputs, nested spans/arrays and
    // a live handle for this call, with disjoint writable output slots.
    unsafe {
        entry(error, Some(MutationOutcome::NotCommitted), || {
            let result = output(result)?;
            let owner = required(store)?.0.get()?;
            let input = sized(input)?;

            let mut builder = mikrio::CopyObjectInput::builder(
                input.bucket.text()?,
                input.key.text()?,
                mikrio::ObjectRef::new(
                    input.copy_source.bucket.text()?,
                    input.copy_source.key.text()?,
                )?,
            );

            if let Some(value) = input.expected_bucket_owner.optional_text()? {
                builder = builder.expected_bucket_owner(value);
            }

            if let Some(value) = input.expected_source_bucket_owner.optional_text()? {
                builder = builder.expected_source_bucket_owner(value);
            }

            if let Some(value) = input.copy_source_if_match.optional_text()? {
                builder = builder.copy_source_if_match(value);
            }

            if let Some(value) = input.copy_source_if_none_match.optional_text()? {
                builder = builder.copy_source_if_none_match(value);
            }

            if let Some(value) = input.copy_source_if_modified_since.timestamp()? {
                builder = builder.copy_source_if_modified_since(value);
            }

            if let Some(value) = input.copy_source_if_unmodified_since.timestamp()? {
                builder = builder.copy_source_if_unmodified_since(value);
            }

            if let Some(value) = input.metadata_directive.optional_text()? {
                builder = builder.metadata_directive(parse_copy_directive(value)?);
            }

            if let Some(value) = input.content_type.optional_text()? {
                builder = builder.content_type(value);
            }

            if let Some(value) = input.content_encoding.optional_text()? {
                builder = builder.content_encoding(value);
            }

            if let Some(value) = input.content_disposition.optional_text()? {
                builder = builder.content_disposition(value);
            }

            if let Some(value) = input.content_language.optional_text()? {
                builder = builder.content_language(value);
            }

            if let Some(value) = input.cache_control.optional_text()? {
                builder = builder.cache_control(value);
            }

            if let Some(value) = input.expires.timestamp()? {
                builder = builder.expires(value);
            }

            builder = builder.metadata(pairs(
                input.metadata,
                input.metadata_count,
                METADATA_LIMITS,
            )?);

            if !input.tags.is_null() || input.tags_count != 0 {
                builder = builder.tags(
                    pairs(input.tags, input.tags_count, TAG_LIMITS)?
                        .into_iter()
                        .map(|(key, value)| mikrio::Tag { key, value })
                        .collect::<Vec<_>>(),
                );
            }

            if let Some(value) = input.tagging_directive.optional_text()? {
                builder = builder.tagging_directive(parse_copy_directive(value)?);
            }

            if let Some(value) = input.checksum_algorithm.optional_text()? {
                builder = builder.checksum_algorithm(parse_checksum_algorithm(value)?);
            }

            if let Some(value) = input.if_match.optional_text()? {
                builder = builder.if_match(value);
            }

            if let Some(value) = input.if_none_match.optional_text()? {
                builder = builder.if_none_match(value);
            }

            let input = builder.build()?;

            let store = owner.store.clone();

            let pending = owner
                .runtime
                .submit(async move { store.copy_object(input).await })?;

            *result = Box::into_raw(Box::new(handles::wait_mutation(pending)??));
            Ok(())
        })
    }
}

#[derive(Clone, Copy)]
#[repr(C)]
pub(super) struct CreateMultipartUploadInput {
    struct_size: u64,
    bucket: Span,
    key: Span,
    expected_bucket_owner: Span,
    content_type: Span,
    content_encoding: Span,
    content_disposition: Span,
    content_language: Span,
    cache_control: Span,
    expires: OptionalI64,
    metadata: *const Pair,
    metadata_count: u64,
    tags: *const Pair,
    tags_count: u64,
    checksum_algorithm: Span,
    checksum_type: Span,
}

input_layout!(CreateMultipartUploadInput);

/// # Safety
/// Follow `include/mikrio.h`: keep handles and declared input bytes live and
/// immutable for this call. Input records and arrays must be aligned; output
/// slots must be aligned, writable and disjoint. Do not race handle release.
#[unsafe(no_mangle)]
pub(super) unsafe extern "C" fn mikrio_v1_store_create_multipart_upload(
    store: *const StoreHandle,
    input: *const CreateMultipartUploadInput,
    result: *mut *mut mikrio::CreateMultipartUploadOutput,
    error: *mut *mut Error,
) -> u32 {
    // SAFETY: the caller retains readable sized inputs, nested spans/arrays and
    // a live handle for this call, with disjoint writable output slots.
    unsafe {
        entry(error, Some(MutationOutcome::NotCommitted), || {
            let result = output(result)?;
            let owner = required(store)?.0.get()?;
            let input = sized(input)?;

            let mut builder = mikrio::CreateMultipartUploadInput::builder(
                input.bucket.text()?,
                input.key.text()?,
            );

            if let Some(value) = input.expected_bucket_owner.optional_text()? {
                builder = builder.expected_bucket_owner(value);
            }

            if let Some(value) = input.content_type.optional_text()? {
                builder = builder.content_type(value);
            }

            if let Some(value) = input.content_encoding.optional_text()? {
                builder = builder.content_encoding(value);
            }

            if let Some(value) = input.content_disposition.optional_text()? {
                builder = builder.content_disposition(value);
            }

            if let Some(value) = input.content_language.optional_text()? {
                builder = builder.content_language(value);
            }

            if let Some(value) = input.cache_control.optional_text()? {
                builder = builder.cache_control(value);
            }

            if let Some(value) = input.expires.timestamp()? {
                builder = builder.expires(value);
            }

            builder = builder.metadata(pairs(
                input.metadata,
                input.metadata_count,
                METADATA_LIMITS,
            )?);

            if !input.tags.is_null() || input.tags_count != 0 {
                builder = builder.tags(
                    pairs(input.tags, input.tags_count, TAG_LIMITS)?
                        .into_iter()
                        .map(|(key, value)| mikrio::Tag { key, value })
                        .collect::<Vec<_>>(),
                );
            }

            if let Some(value) = input.checksum_algorithm.optional_text()? {
                builder = builder.checksum_algorithm(parse_checksum_algorithm(value)?);
            }

            if let Some(value) = input.checksum_type.optional_text()? {
                builder = builder.checksum_type(parse_checksum_type(value)?);
            }

            let input = builder.build()?;

            let store = owner.store.clone();

            let pending = owner
                .runtime
                .submit(async move { store.create_multipart_upload(input).await })?;

            *result = Box::into_raw(Box::new(handles::wait_mutation(pending)??));
            Ok(())
        })
    }
}

#[derive(Clone, Copy)]
#[repr(C)]
pub(super) struct UploadPartInput {
    struct_size: u64,
    bucket: Span,
    key: Span,
    upload_id: Span,
    part_number: u32,
    part_number_reserved: u32,
    expected_bucket_owner: Span,
    content_length: OptionalU64,
    checksum_algorithm: Span,
    content_md5: Span,
    checksum_crc32: Span,
    checksum_crc32c: Span,
    checksum_sha1: Span,
    checksum_sha256: Span,
    checksum_crc64_nvme: Span,
}

input_layout!(UploadPartInput);

/// # Safety
/// Follow `include/mikrio.h`: keep handles and declared input bytes live and
/// immutable for this call. Input records and arrays must be aligned; output
/// slots must be aligned, writable and disjoint. Do not race handle release.
#[unsafe(no_mangle)]
pub(super) unsafe extern "C" fn mikrio_v1_store_begin_upload_part(
    store: *const StoreHandle,
    input: *const UploadPartInput,
    result: *mut *mut WriterHandle,
    error: *mut *mut Error,
) -> u32 {
    // SAFETY: the caller retains readable sized inputs, nested spans/arrays and
    // a live handle for this call, with disjoint writable output slots.
    unsafe {
        entry(error, Some(MutationOutcome::NotCommitted), || {
            let result = output(result)?;
            let owner = required(store)?.0.get()?;
            let input = sized(input)?;
            let (upload, source) = mikrio_embed::upload::Upload::channel();

            if input.part_number_reserved != 0 {
                return Err(Error::invalid("nonzero reserved field"));
            }

            let mut builder = mikrio::UploadPartInput::builder(
                input.bucket.text()?,
                input.key.text()?,
                input.upload_id.text()?,
                input.part_number,
                source,
            );

            if let Some(value) = input.expected_bucket_owner.optional_text()? {
                builder = builder.expected_bucket_owner(value);
            }

            if let Some(value) = input.content_length.get()? {
                builder = builder.content_length(value);
            }

            if let Some(value) = input.checksum_algorithm.optional_text()? {
                builder = builder.checksum_algorithm(parse_checksum_algorithm(value)?);
            }

            if let Some(value) = input.content_md5.optional_text()? {
                builder = builder.content_md5(value);
            }

            if let Some(value) = input.checksum_crc32.optional_text()? {
                builder = builder.checksum_crc32(value);
            }

            if let Some(value) = input.checksum_crc32c.optional_text()? {
                builder = builder.checksum_crc32c(value);
            }

            if let Some(value) = input.checksum_sha1.optional_text()? {
                builder = builder.checksum_sha1(value);
            }

            if let Some(value) = input.checksum_sha256.optional_text()? {
                builder = builder.checksum_sha256(value);
            }

            if let Some(value) = input.checksum_crc64_nvme.optional_text()? {
                builder = builder.checksum_crc64_nvme(value);
            }

            let input = builder.build()?;

            let writer = Writer::begin_part(owner, upload, input)?;
            *result = Box::into_raw(Box::new(WriterHandle(ProcessBound::new(writer))));
            Ok(())
        })
    }
}

#[derive(Clone, Copy)]
#[repr(C)]
pub(super) struct UploadPartCopyInput {
    struct_size: u64,
    bucket: Span,
    key: Span,
    upload_id: Span,
    part_number: u32,
    part_number_reserved: u32,
    copy_source: ObjectRef,
    expected_bucket_owner: Span,
    expected_source_bucket_owner: Span,
    copy_source_if_match: Span,
    copy_source_if_none_match: Span,
    copy_source_if_modified_since: OptionalI64,
    copy_source_if_unmodified_since: OptionalI64,
    copy_source_range: Span,
}

input_layout!(UploadPartCopyInput);

/// # Safety
/// Follow `include/mikrio.h`: keep handles and declared input bytes live and
/// immutable for this call. Input records and arrays must be aligned; output
/// slots must be aligned, writable and disjoint. Do not race handle release.
#[unsafe(no_mangle)]
pub(super) unsafe extern "C" fn mikrio_v1_store_upload_part_copy(
    store: *const StoreHandle,
    input: *const UploadPartCopyInput,
    result: *mut *mut mikrio::UploadPartCopyOutput,
    error: *mut *mut Error,
) -> u32 {
    // SAFETY: the caller retains readable sized inputs, nested spans/arrays and
    // a live handle for this call, with disjoint writable output slots.
    unsafe {
        entry(error, Some(MutationOutcome::NotCommitted), || {
            let result = output(result)?;
            let owner = required(store)?.0.get()?;
            let input = sized(input)?;

            if input.part_number_reserved != 0 {
                return Err(Error::invalid("nonzero reserved field"));
            }

            let mut builder = mikrio::UploadPartCopyInput::builder(
                input.bucket.text()?,
                input.key.text()?,
                input.upload_id.text()?,
                input.part_number,
                mikrio::ObjectRef::new(
                    input.copy_source.bucket.text()?,
                    input.copy_source.key.text()?,
                )?,
            );

            if let Some(value) = input.expected_bucket_owner.optional_text()? {
                builder = builder.expected_bucket_owner(value);
            }

            if let Some(value) = input.expected_source_bucket_owner.optional_text()? {
                builder = builder.expected_source_bucket_owner(value);
            }

            if let Some(value) = input.copy_source_if_match.optional_text()? {
                builder = builder.copy_source_if_match(value);
            }

            if let Some(value) = input.copy_source_if_none_match.optional_text()? {
                builder = builder.copy_source_if_none_match(value);
            }

            if let Some(value) = input.copy_source_if_modified_since.timestamp()? {
                builder = builder.copy_source_if_modified_since(value);
            }

            if let Some(value) = input.copy_source_if_unmodified_since.timestamp()? {
                builder = builder.copy_source_if_unmodified_since(value);
            }

            if let Some(value) = input.copy_source_range.optional_text()? {
                builder = builder.copy_source_range(value);
            }

            let input = builder.build()?;

            let store = owner.store.clone();

            let pending = owner
                .runtime
                .submit(async move { store.upload_part_copy(input).await })?;

            *result = Box::into_raw(Box::new(handles::wait_mutation(pending)??));
            Ok(())
        })
    }
}

#[derive(Clone, Copy)]
#[repr(C)]
pub(super) struct CompleteMultipartUploadInput {
    struct_size: u64,
    bucket: Span,
    key: Span,
    upload_id: Span,
    parts: *const CompletedPart,
    parts_count: u64,
    expected_bucket_owner: Span,
    if_match: Span,
    if_none_match: Span,
    content_length: OptionalU64,
    checksum_type: Span,
    checksum_crc32: Span,
    checksum_crc32c: Span,
    checksum_sha1: Span,
    checksum_sha256: Span,
    checksum_crc64_nvme: Span,
}

input_layout!(CompleteMultipartUploadInput);

/// # Safety
/// Follow `include/mikrio.h`: keep handles and declared input bytes live and
/// immutable for this call. Input records and arrays must be aligned; output
/// slots must be aligned, writable and disjoint. Do not race handle release.
#[unsafe(no_mangle)]
pub(super) unsafe extern "C" fn mikrio_v1_store_complete_multipart_upload(
    store: *const StoreHandle,
    input: *const CompleteMultipartUploadInput,
    result: *mut *mut mikrio::CompleteMultipartUploadOutput,
    error: *mut *mut Error,
) -> u32 {
    // SAFETY: the caller retains readable sized inputs, nested spans/arrays and
    // a live handle for this call, with disjoint writable output slots.
    unsafe {
        entry(error, Some(MutationOutcome::NotCommitted), || {
            let result = output(result)?;
            let owner = required(store)?.0.get()?;
            let input = sized(input)?;

            let mut builder = mikrio::CompleteMultipartUploadInput::builder(
                input.bucket.text()?,
                input.key.text()?,
                input.upload_id.text()?,
                decode_completed_parts(input.parts, input.parts_count)?,
            );

            if let Some(value) = input.expected_bucket_owner.optional_text()? {
                builder = builder.expected_bucket_owner(value);
            }

            if let Some(value) = input.if_match.optional_text()? {
                builder = builder.if_match(value);
            }

            if let Some(value) = input.if_none_match.optional_text()? {
                builder = builder.if_none_match(value);
            }

            if let Some(value) = input.content_length.get()? {
                builder = builder.content_length(value);
            }

            if let Some(value) = input.checksum_type.optional_text()? {
                builder = builder.checksum_type(parse_checksum_type(value)?);
            }

            if let Some(value) = input.checksum_crc32.optional_text()? {
                builder = builder.checksum_crc32(value);
            }

            if let Some(value) = input.checksum_crc32c.optional_text()? {
                builder = builder.checksum_crc32c(value);
            }

            if let Some(value) = input.checksum_sha1.optional_text()? {
                builder = builder.checksum_sha1(value);
            }

            if let Some(value) = input.checksum_sha256.optional_text()? {
                builder = builder.checksum_sha256(value);
            }

            if let Some(value) = input.checksum_crc64_nvme.optional_text()? {
                builder = builder.checksum_crc64_nvme(value);
            }

            let input = builder.build()?;

            let store = owner.store.clone();

            let pending = owner
                .runtime
                .submit(async move { store.complete_multipart_upload(input).await })?;

            *result = Box::into_raw(Box::new(handles::wait_mutation(pending)??));
            Ok(())
        })
    }
}

#[derive(Clone, Copy)]
#[repr(C)]
pub(super) struct AbortMultipartUploadInput {
    struct_size: u64,
    bucket: Span,
    key: Span,
    upload_id: Span,
    expected_bucket_owner: Span,
}

input_layout!(AbortMultipartUploadInput);

/// # Safety
/// Follow `include/mikrio.h`: keep handles and declared input bytes live and
/// immutable for this call. Input records and arrays must be aligned; output
/// slots must be aligned, writable and disjoint. Do not race handle release.
#[unsafe(no_mangle)]
pub(super) unsafe extern "C" fn mikrio_v1_store_abort_multipart_upload(
    store: *const StoreHandle,
    input: *const AbortMultipartUploadInput,
    error: *mut *mut Error,
) -> u32 {
    // SAFETY: the caller retains readable sized inputs, nested spans/arrays and
    // a live handle for this call, with disjoint writable output slots.
    unsafe {
        entry(error, Some(MutationOutcome::NotCommitted), || {
            let owner = required(store)?.0.get()?;
            let input = sized(input)?;

            let mut builder = mikrio::AbortMultipartUploadInput::builder(
                input.bucket.text()?,
                input.key.text()?,
                input.upload_id.text()?,
            );

            if let Some(value) = input.expected_bucket_owner.optional_text()? {
                builder = builder.expected_bucket_owner(value);
            }

            let input = builder.build()?;

            let store = owner.store.clone();

            let pending = owner
                .runtime
                .submit(async move { store.abort_multipart_upload(input).await })?;

            handles::wait_mutation(pending)??;
            Ok(())
        })
    }
}

#[derive(Clone, Copy)]
#[repr(C)]
pub(super) struct ListPartsInput {
    struct_size: u64,
    bucket: Span,
    key: Span,
    upload_id: Span,
    expected_bucket_owner: Span,
    part_number_marker: OptionalU64,
    max_parts: OptionalU64,
}

input_layout!(ListPartsInput);

/// # Safety
/// Follow `include/mikrio.h`: keep handles and declared input bytes live and
/// immutable for this call. Input records and arrays must be aligned; output
/// slots must be aligned, writable and disjoint. Do not race handle release.
#[unsafe(no_mangle)]
pub(super) unsafe extern "C" fn mikrio_v1_store_list_parts(
    store: *const StoreHandle,
    input: *const ListPartsInput,
    result: *mut *mut mikrio::ListPartsOutput,
    error: *mut *mut Error,
) -> u32 {
    // SAFETY: the caller retains readable sized inputs, nested spans/arrays and
    // a live handle for this call, with disjoint writable output slots.
    unsafe {
        entry(error, None, || {
            let result = output(result)?;
            let owner = required(store)?.0.get()?;
            let input = sized(input)?;

            let mut builder = mikrio::ListPartsInput::builder(
                input.bucket.text()?,
                input.key.text()?,
                input.upload_id.text()?,
            );

            if let Some(value) = input.expected_bucket_owner.optional_text()? {
                builder = builder.expected_bucket_owner(value);
            }

            if let Some(value) = input.part_number_marker.get()? {
                builder = builder.part_number_marker(
                    u32::try_from(value)
                        .map_err(|_| Error::invalid("count exceeds the supported range"))?,
                );
            }

            if let Some(value) = input.max_parts.get()? {
                builder = builder.max_parts(
                    u32::try_from(value)
                        .map_err(|_| Error::invalid("count exceeds the supported range"))?,
                );
            }

            let input = builder.build()?;

            let store = owner.store.clone();

            let pending = owner
                .runtime
                .submit(async move { store.list_parts(input).await })?;

            *result = Box::into_raw(Box::new(handles::wait(pending)??));
            Ok(())
        })
    }
}

#[derive(Clone, Copy)]
#[repr(C)]
pub(super) struct ListMultipartUploadsInput {
    struct_size: u64,
    bucket: Span,
    expected_bucket_owner: Span,
    prefix: Span,
    delimiter: Span,
    key_marker: Span,
    upload_id_marker: Span,
    max_uploads: OptionalU64,
}

input_layout!(ListMultipartUploadsInput);

/// # Safety
/// Follow `include/mikrio.h`: keep handles and declared input bytes live and
/// immutable for this call. Input records and arrays must be aligned; output
/// slots must be aligned, writable and disjoint. Do not race handle release.
#[unsafe(no_mangle)]
pub(super) unsafe extern "C" fn mikrio_v1_store_list_multipart_uploads(
    store: *const StoreHandle,
    input: *const ListMultipartUploadsInput,
    result: *mut *mut mikrio::ListMultipartUploadsOutput,
    error: *mut *mut Error,
) -> u32 {
    // SAFETY: the caller retains readable sized inputs, nested spans/arrays and
    // a live handle for this call, with disjoint writable output slots.
    unsafe {
        entry(error, None, || {
            let result = output(result)?;
            let owner = required(store)?.0.get()?;
            let input = sized(input)?;

            let mut builder = mikrio::ListMultipartUploadsInput::builder(input.bucket.text()?);

            if let Some(value) = input.expected_bucket_owner.optional_text()? {
                builder = builder.expected_bucket_owner(value);
            }

            if let Some(value) = input.prefix.optional_text()? {
                builder = builder.prefix(value);
            }

            if let Some(value) = input.delimiter.optional_text()? {
                builder = builder.delimiter(value);
            }

            if let Some(value) = input.key_marker.optional_text()? {
                builder = builder.key_marker(value);
            }

            if let Some(value) = input.upload_id_marker.optional_text()? {
                builder = builder.upload_id_marker(value);
            }

            if let Some(value) = input.max_uploads.get()? {
                builder = builder.max_uploads(
                    u32::try_from(value)
                        .map_err(|_| Error::invalid("count exceeds the supported range"))?,
                );
            }

            let input = builder.build()?;

            let store = owner.store.clone();

            let pending = owner
                .runtime
                .submit(async move { store.list_multipart_uploads(input).await })?;

            *result = Box::into_raw(Box::new(handles::wait(pending)??));
            Ok(())
        })
    }
}

#[derive(Default)]
#[repr(C)]
pub(super) struct CopyObjectOutputView {
    etag: Span,
    last_modified_ms: i64,
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
pub(super) unsafe extern "C" fn mikrio_v1_copy_object_output_view(
    value: *const mikrio::CopyObjectOutput,
) -> CopyObjectOutputView {
    // SAFETY: the immutable result allocation remains live for every borrowed view.
    let Some(value) = (unsafe { value.as_ref() }) else {
        return CopyObjectOutputView::default();
    };

    CopyObjectOutputView {
        etag: Span::borrowed(value.etag()),
        last_modified_ms: value.last_modified().as_unix_millis(),
        checksum_crc32: Span::optional(value.checksum_crc32()),
        checksum_crc32c: Span::optional(value.checksum_crc32c()),
        checksum_sha1: Span::optional(value.checksum_sha1()),
        checksum_sha256: Span::optional(value.checksum_sha256()),
        checksum_crc64_nvme: Span::optional(value.checksum_crc64_nvme()),
    }
}

release!(
    mikrio_v1_copy_object_output_release,
    mikrio::CopyObjectOutput
);

#[derive(Default)]
#[repr(C)]
pub(super) struct CreateMultipartUploadOutputView {
    upload_id: Span,
    checksum_algorithm: Span,
    checksum_type: Span,
}

/// # Safety
/// A nonnull pointer must identify the matching live, aligned library allocation.
/// Keep it immutable and unreleased until all returned borrowed views expire.
#[unsafe(no_mangle)]
pub(super) unsafe extern "C" fn mikrio_v1_create_multipart_upload_output_view(
    value: *const mikrio::CreateMultipartUploadOutput,
) -> CreateMultipartUploadOutputView {
    // SAFETY: the immutable result allocation remains live for every borrowed view.
    let Some(value) = (unsafe { value.as_ref() }) else {
        return CreateMultipartUploadOutputView::default();
    };

    CreateMultipartUploadOutputView {
        upload_id: Span::borrowed(value.upload_id()),
        checksum_algorithm: Span::borrowed(value.checksum_algorithm().as_str()),
        checksum_type: Span::borrowed(value.checksum_type().as_str()),
    }
}

release!(
    mikrio_v1_create_multipart_upload_output_release,
    mikrio::CreateMultipartUploadOutput
);

#[derive(Default)]
#[repr(C)]
pub(super) struct UploadPartOutputView {
    etag: Span,
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
pub(super) unsafe extern "C" fn mikrio_v1_upload_part_output_view(
    value: *const mikrio::UploadPartOutput,
) -> UploadPartOutputView {
    // SAFETY: the immutable result allocation remains live for every borrowed view.
    let Some(value) = (unsafe { value.as_ref() }) else {
        return UploadPartOutputView::default();
    };

    UploadPartOutputView {
        etag: Span::borrowed(value.etag()),
        checksum_crc32: Span::optional(value.checksum_crc32()),
        checksum_crc32c: Span::optional(value.checksum_crc32c()),
        checksum_sha1: Span::optional(value.checksum_sha1()),
        checksum_sha256: Span::optional(value.checksum_sha256()),
        checksum_crc64_nvme: Span::optional(value.checksum_crc64_nvme()),
    }
}

release!(
    mikrio_v1_upload_part_output_release,
    mikrio::UploadPartOutput
);

#[derive(Default)]
#[repr(C)]
pub(super) struct UploadPartCopyOutputView {
    etag: Span,
    last_modified_ms: i64,
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
pub(super) unsafe extern "C" fn mikrio_v1_upload_part_copy_output_view(
    value: *const mikrio::UploadPartCopyOutput,
) -> UploadPartCopyOutputView {
    // SAFETY: the immutable result allocation remains live for every borrowed view.
    let Some(value) = (unsafe { value.as_ref() }) else {
        return UploadPartCopyOutputView::default();
    };

    UploadPartCopyOutputView {
        etag: Span::borrowed(value.etag()),
        last_modified_ms: value.last_modified().as_unix_millis(),
        checksum_crc32: Span::optional(value.checksum_crc32()),
        checksum_crc32c: Span::optional(value.checksum_crc32c()),
        checksum_sha1: Span::optional(value.checksum_sha1()),
        checksum_sha256: Span::optional(value.checksum_sha256()),
        checksum_crc64_nvme: Span::optional(value.checksum_crc64_nvme()),
    }
}

release!(
    mikrio_v1_upload_part_copy_output_release,
    mikrio::UploadPartCopyOutput
);

#[derive(Default)]
#[repr(C)]
pub(super) struct CompleteMultipartUploadOutputView {
    etag: Span,
    checksum_type: Span,
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
pub(super) unsafe extern "C" fn mikrio_v1_complete_multipart_upload_output_view(
    value: *const mikrio::CompleteMultipartUploadOutput,
) -> CompleteMultipartUploadOutputView {
    // SAFETY: the immutable result allocation remains live for every borrowed view.
    let Some(value) = (unsafe { value.as_ref() }) else {
        return CompleteMultipartUploadOutputView::default();
    };

    CompleteMultipartUploadOutputView {
        etag: Span::borrowed(value.etag()),
        checksum_type: Span::borrowed(value.checksum_type().as_str()),
        checksum_crc32: Span::optional(value.checksum_crc32()),
        checksum_crc32c: Span::optional(value.checksum_crc32c()),
        checksum_sha1: Span::optional(value.checksum_sha1()),
        checksum_sha256: Span::optional(value.checksum_sha256()),
        checksum_crc64_nvme: Span::optional(value.checksum_crc64_nvme()),
    }
}

release!(
    mikrio_v1_complete_multipart_upload_output_release,
    mikrio::CompleteMultipartUploadOutput
);

#[derive(Default)]
#[repr(C)]
pub(super) struct PartView {
    part_number: u32,
    part_number_reserved: u32,
    size: u64,
    etag: Span,
    last_modified_ms: i64,
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
pub(super) unsafe extern "C" fn mikrio_v1_list_parts_output_item(
    value: *const mikrio::ListPartsOutput,
    index: u64,
) -> PartView {
    // SAFETY: the immutable result allocation remains live for every borrowed view.
    let Some(value) = (unsafe { value.as_ref() }) else {
        return PartView::default();
    };

    let Some(part) = usize::try_from(index)
        .ok()
        .and_then(|index| value.parts().get(index))
    else {
        return PartView::default();
    };

    PartView {
        part_number: part.part_number(),
        part_number_reserved: 0,
        size: part.size(),
        etag: Span::borrowed(part.etag()),
        last_modified_ms: part.last_modified().as_unix_millis(),
        checksum_crc32: Span::optional(part.checksum_crc32()),
        checksum_crc32c: Span::optional(part.checksum_crc32c()),
        checksum_sha1: Span::optional(part.checksum_sha1()),
        checksum_sha256: Span::optional(part.checksum_sha256()),
        checksum_crc64_nvme: Span::optional(part.checksum_crc64_nvme()),
    }
}

#[derive(Default)]
#[repr(C)]
pub(super) struct MultipartUploadView {
    key: Span,
    upload_id: Span,
    initiated_ms: i64,
    checksum_algorithm: Span,
    checksum_type: Span,
}

/// # Safety
/// A nonnull pointer must identify the matching live, aligned library allocation.
/// Keep it immutable and unreleased until all returned borrowed views expire.
#[unsafe(no_mangle)]
pub(super) unsafe extern "C" fn mikrio_v1_list_multipart_uploads_output_item(
    value: *const mikrio::ListMultipartUploadsOutput,
    index: u64,
) -> MultipartUploadView {
    // SAFETY: the immutable result allocation remains live for every borrowed view.
    let Some(value) = (unsafe { value.as_ref() }) else {
        return MultipartUploadView::default();
    };

    let Some(upload) = usize::try_from(index)
        .ok()
        .and_then(|index| value.uploads().get(index))
    else {
        return MultipartUploadView::default();
    };

    MultipartUploadView {
        key: Span::borrowed(upload.key()),
        upload_id: Span::borrowed(upload.upload_id()),
        initiated_ms: upload.initiated().as_unix_millis(),
        checksum_algorithm: Span::borrowed(upload.checksum_algorithm().as_str()),
        checksum_type: Span::borrowed(upload.checksum_type().as_str()),
    }
}

#[derive(Default)]
#[repr(C)]
pub(super) struct ListPartsOutputView {
    parts_count: u64,
    is_truncated: u32,
    is_truncated_reserved: u32,
    next_part_number_marker: OptionalU64,
    checksum_algorithm: Span,
    checksum_type: Span,
}

/// # Safety
/// A nonnull pointer must identify the matching live, aligned library allocation.
/// Keep it immutable and unreleased until all returned borrowed views expire.
#[unsafe(no_mangle)]
pub(super) unsafe extern "C" fn mikrio_v1_list_parts_output_view(
    value: *const mikrio::ListPartsOutput,
) -> ListPartsOutputView {
    // SAFETY: the immutable result allocation remains live for every borrowed view.
    let Some(value) = (unsafe { value.as_ref() }) else {
        return ListPartsOutputView::default();
    };

    ListPartsOutputView {
        parts_count: u64::try_from(value.parts().len()).unwrap_or(u64::MAX),
        is_truncated: u32::from(value.is_truncated()),
        is_truncated_reserved: 0,
        next_part_number_marker: value.next_part_number_marker().map_or_else(
            OptionalU64::default,
            |value| OptionalU64 {
                present: 1,
                reserved: 0,
                value: u64::from(value),
            },
        ),
        checksum_algorithm: Span::borrowed(value.checksum_algorithm().as_str()),
        checksum_type: Span::borrowed(value.checksum_type().as_str()),
    }
}

release!(mikrio_v1_list_parts_output_release, mikrio::ListPartsOutput);

#[derive(Default)]
#[repr(C)]
pub(super) struct ListMultipartUploadsOutputView {
    uploads_count: u64,
    common_prefixes_count: u64,
    is_truncated: u32,
    is_truncated_reserved: u32,
    next_key_marker: Span,
    next_upload_id_marker: Span,
}

/// # Safety
/// A nonnull pointer must identify the matching live, aligned library allocation.
/// Keep it immutable and unreleased until all returned borrowed views expire.
#[unsafe(no_mangle)]
pub(super) unsafe extern "C" fn mikrio_v1_list_multipart_uploads_output_view(
    value: *const mikrio::ListMultipartUploadsOutput,
) -> ListMultipartUploadsOutputView {
    // SAFETY: the immutable result allocation remains live for every borrowed view.
    let Some(value) = (unsafe { value.as_ref() }) else {
        return ListMultipartUploadsOutputView::default();
    };

    ListMultipartUploadsOutputView {
        uploads_count: u64::try_from(value.uploads().len()).unwrap_or(u64::MAX),
        common_prefixes_count: u64::try_from(value.common_prefixes().len()).unwrap_or(u64::MAX),
        is_truncated: u32::from(value.is_truncated()),
        is_truncated_reserved: 0,
        next_key_marker: Span::optional(value.next_key_marker()),
        next_upload_id_marker: Span::optional(value.next_upload_id_marker()),
    }
}

release!(
    mikrio_v1_list_multipart_uploads_output_release,
    mikrio::ListMultipartUploadsOutput
);

/// # Safety
/// A nonnull pointer must identify the matching live, aligned library allocation.
/// Keep it immutable and unreleased until all returned borrowed views expire.
#[unsafe(no_mangle)]
pub(super) unsafe extern "C" fn mikrio_v1_list_multipart_uploads_output_prefix(
    value: *const mikrio::ListMultipartUploadsOutput,
    index: u64,
) -> Span {
    // SAFETY: caller retains the immutable page for all borrowed prefix views.
    unsafe { value.as_ref() }
        .and_then(|value| {
            usize::try_from(index)
                .ok()
                .and_then(|index| value.common_prefixes().get(index))
        })
        .map_or_else(Span::default, |prefix| Span::borrowed(prefix.prefix()))
}

/// # Safety
/// Serialize access to the live, aligned writer, including against release.
/// Output slots must be aligned, writable and disjoint, as specified in
/// `include/mikrio.h`.
#[unsafe(no_mangle)]
pub(super) unsafe extern "C" fn mikrio_v1_writer_finish_part(
    writer: *mut WriterHandle,
    result: *mut *mut mikrio::UploadPartOutput,
    error: *mut *mut Error,
) -> u32 {
    // SAFETY: caller serializes writer use and supplies disjoint output slots.
    unsafe {
        entry(error, Some(MutationOutcome::Unknown), || {
            let result = output(result)?;

            let writer = writer
                .as_mut()
                .ok_or_else(|| Error::invalid("null writer"))?;

            *result = Box::into_raw(Box::new(writer.0.get_mut()?.finish_part()?));
            Ok(())
        })
    }
}
