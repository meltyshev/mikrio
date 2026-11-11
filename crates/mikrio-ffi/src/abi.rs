//! Foreign input records and buffers are borrowed only within entry calls.
//! The header defines allocation, aliasing, serialization, release and
//! diagnostic callback-context lifetimes.

use std::mem::{MaybeUninit, size_of};
use std::ptr;
use std::sync::Arc;

use mikrio::{MutationOutcome, Timestamp};
use mikrio_embed::{CloseGate, HostRuntime, ProcessBound};

use crate::error::Error;
use crate::handles::{self, Body, BodyHandle, Connection, StoreHandle, Writer, WriterHandle};

/// Plain C input layouts with explicitly accepted whole-field prefixes.
///
/// # Safety
/// Implementations must be `repr(C)`, begin with a `u64`, and contain only
/// integers and raw pointers (possibly nested). Every initialized bit pattern
/// must be valid, and omitted fields must accept all-zero initialization.
/// Prefixes must include every required field and must not split a field,
/// pointer/count pair, optional integer, or flag/reserved pair. No padding may
/// overlap a field; copied padding is never inspected.
unsafe trait SizedInput: Copy {
    const PREFIX_SIZES: &'static [usize];
}

// Each listed field starts an optional suffix. The full layout always remains
// accepted. Keep the corresponding prefix table in the canonical header current.
macro_rules! input_layout {
    ($input:ty $(, $field:ident)* $(,)?) => {
        // SAFETY: these C records contain only integers/raw pointers and nested
        // records of those types. Listed offsets end complete preceding fields;
        // every omitted suffix has the documented zero/absent default.
        unsafe impl $crate::abi::SizedInput for $input {
            const PREFIX_SIZES: &'static [usize] = &[
                $(std::mem::offset_of!($input, $field),)*
                std::mem::size_of::<$input>(),
            ];
        }
    };
}

#[derive(Clone, Copy, Default)]
#[repr(C)]
pub(crate) struct Span {
    data: *const u8,
    len: u64,
}

impl Span {
    fn borrowed(value: &str) -> Self {
        Self {
            data: value.as_ptr(),
            len: u64::try_from(value.len()).unwrap_or(u64::MAX),
        }
    }

    fn optional(value: Option<&str>) -> Self {
        value.map_or_else(Self::default, Self::borrowed)
    }

    unsafe fn bytes<'a>(self) -> Result<&'a [u8], Error> {
        let len = usize::try_from(self.len)
            .map_err(|_| Error::invalid("span length exceeds the host range"))?;

        if len > isize::MAX.unsigned_abs() || (self.data.is_null() && len != 0) {
            return Err(Error::invalid("invalid span length or null data"));
        }

        if len == 0 {
            return Ok(&[]);
        }

        // SAFETY: caller supplies one live readable allocation of len bytes,
        // immutable for this call; null/size were checked. No reference escapes.
        Ok(unsafe { std::slice::from_raw_parts(self.data, len) })
    }

    unsafe fn text<'a>(self) -> Result<&'a str, Error> {
        // SAFETY: inherited span borrow contract, bounded to this entry call.
        std::str::from_utf8(unsafe { self.bytes()? })
            .map_err(|_| Error::invalid("string is not UTF-8"))
    }

    unsafe fn optional_text<'a>(self) -> Result<Option<&'a str>, Error> {
        if self.data.is_null() {
            if self.len != 0 {
                return Err(Error::invalid("absent string has a nonzero length"));
            }

            return Ok(None);
        }

        // SAFETY: inherited span borrow contract.
        unsafe { self.text().map(Some) }
    }
}

#[derive(Clone, Copy, Default)]
#[repr(C)]
pub(crate) struct OptionalU64 {
    present: u32,
    reserved: u32,
    value: u64,
}

impl OptionalU64 {
    fn get(self) -> Result<Option<u64>, Error> {
        optional(self.present, self.reserved, self.value)
    }
}

#[derive(Clone, Copy, Default)]
#[repr(C)]
pub(crate) struct OptionalI64 {
    present: u32,
    reserved: u32,
    value: i64,
}

impl OptionalI64 {
    fn timestamp(self) -> Result<Option<Timestamp>, Error> {
        optional(self.present, self.reserved, self.value)?
            .map(Timestamp::from_unix_millis)
            .transpose()
            .map_err(Error::from)
    }

    fn from_timestamp(value: Option<Timestamp>) -> Self {
        value.map_or_else(Self::default, |value| Self {
            present: 1,
            reserved: 0,
            value: value.as_unix_millis(),
        })
    }
}

fn optional<T: Default + PartialEq>(
    present: u32,
    reserved: u32,
    value: T,
) -> Result<Option<T>, Error> {
    if reserved != 0 || present > 1 || (present == 0 && value != T::default()) {
        return Err(Error::invalid(
            "invalid optional value discriminant or reserved field",
        ));
    }

    Ok((present == 1).then_some(value))
}

#[derive(Clone, Copy)]
#[repr(C)]
pub(crate) struct Pair {
    key: Span,
    value: Span,
}

#[derive(Clone, Copy)]
#[repr(C)]
pub(crate) struct OpenOptions {
    struct_size: u64,
    directory: Span,
    region: Span,
    create_if_missing: OptionalU64,
}

input_layout!(OpenOptions, region, create_if_missing);

#[derive(Clone, Copy)]
#[repr(C)]
pub(crate) struct BucketInput {
    struct_size: u64,
    bucket: Span,
    expected_bucket_owner: Span,
}

input_layout!(BucketInput, expected_bucket_owner);

#[derive(Clone, Copy)]
#[repr(C)]
pub(crate) struct PutInput {
    struct_size: u64,
    bucket: Span,
    key: Span,
    expected_bucket_owner: Span,
    content_length: OptionalU64,
    content_type: Span,
    content_encoding: Span,
    content_disposition: Span,
    content_language: Span,
    cache_control: Span,
    expires: OptionalI64,
    metadata: *const Pair,
    metadata_count: u64,
    tags: *const Pair,
    tag_count: u64,
    checksum_algorithm: Span,
    content_md5: Span,
    checksum_crc32: Span,
    checksum_crc32c: Span,
    checksum_sha1: Span,
    checksum_sha256: Span,
    checksum_crc64_nvme: Span,
    if_match: Span,
    if_none_match: Span,
}

input_layout!(
    PutInput,
    expected_bucket_owner,
    content_length,
    content_type,
    content_encoding,
    content_disposition,
    content_language,
    cache_control,
    expires,
    metadata,
    tags,
    checksum_algorithm,
    content_md5,
    checksum_crc32,
    checksum_crc32c,
    checksum_sha1,
    checksum_sha256,
    checksum_crc64_nvme,
    if_match,
    if_none_match
);

#[derive(Clone, Copy)]
#[repr(C)]
pub(crate) struct ReadInput {
    struct_size: u64,
    bucket: Span,
    key: Span,
    expected_bucket_owner: Span,
    checksum_mode: Span,
    range: Span,
    if_match: Span,
    if_none_match: Span,
    if_modified_since: OptionalI64,
    if_unmodified_since: OptionalI64,
    response_content_type: Span,
    response_content_encoding: Span,
    response_content_disposition: Span,
    response_content_language: Span,
    response_cache_control: Span,
    response_expires: OptionalI64,
    part_number: OptionalU64,
}

input_layout!(
    ReadInput,
    expected_bucket_owner,
    checksum_mode,
    range,
    if_match,
    if_none_match,
    if_modified_since,
    if_unmodified_since,
    response_content_type,
    response_content_encoding,
    response_content_disposition,
    response_content_language,
    response_cache_control,
    response_expires,
    part_number
);

#[derive(Clone, Copy)]
#[repr(C)]
pub(crate) struct DeleteInput {
    struct_size: u64,
    bucket: Span,
    key: Span,
    expected_bucket_owner: Span,
    if_match: Span,
}

input_layout!(DeleteInput, expected_bucket_owner, if_match);

#[derive(Default)]
#[repr(C)]
pub(crate) struct InfoView {
    store_id: Span,
    format_version: u32,
    reserved: u32,
    directory: Span,
    region: Span,
}

pub(crate) struct Info {
    info: mikrio::StoreInfo,
    directory: String,
}

#[derive(Default)]
#[repr(C)]
pub(crate) struct ErrorView {
    code: Span,
    message: Span,
    cause: Span,
    mutation_outcome: u32,
    reserved: u32,
}

#[derive(Default)]
#[repr(C)]
pub(crate) struct PutView {
    etag: Span,
    checksum_crc32: Span,
    checksum_crc32c: Span,
    checksum_sha1: Span,
    checksum_sha256: Span,
    checksum_crc64_nvme: Span,
}

#[derive(Default)]
#[repr(C)]
pub(crate) struct ReadView {
    content_length: u64,
    etag: Span,
    last_modified_ms: i64,
    metadata: *const Pair,
    metadata_count: u64,
    tag_count: u32,
    reserved: u32,
    content_type: Span,
    content_encoding: Span,
    content_disposition: Span,
    content_language: Span,
    cache_control: Span,
    expires: OptionalI64,
    content_range: Span,
    parts_count: OptionalU64,
    checksum_crc32: Span,
    checksum_crc32c: Span,
    checksum_sha1: Span,
    checksum_sha256: Span,
    checksum_crc64_nvme: Span,
    checksum_type: Span,
}

pub(crate) struct ReadOutput {
    facts: mikrio::HeadObjectOutput,
    metadata: Vec<Pair>,
}

impl ReadOutput {
    fn new(facts: mikrio::HeadObjectOutput) -> Self {
        let metadata = facts
            .metadata()
            .iter()
            .map(|(key, value)| Pair {
                key: Span::borrowed(key),
                value: Span::borrowed(value),
            })
            .collect();

        Self { facts, metadata }
    }
}

// Checked output initialization happens before any operation starts. Output
// allocations/views are immutable and released only by their matching symbol.
unsafe fn output<'a, T>(pointer: *mut *mut T) -> Result<&'a mut *mut T, Error> {
    // SAFETY: caller provides exclusive writable, aligned output storage.
    let output =
        unsafe { pointer.as_mut() }.ok_or_else(|| Error::invalid("null output pointer"))?;

    *output = ptr::null_mut();

    Ok(output)
}

unsafe fn required<'a, T>(pointer: *const T) -> Result<&'a T, Error> {
    // SAFETY: caller supplies a live aligned immutable T for this call.
    unsafe { pointer.as_ref() }.ok_or_else(|| Error::invalid("null handle or input"))
}

unsafe fn sized<T: SizedInput>(pointer: *const T) -> Result<T, Error> {
    if pointer.is_null() {
        return Err(Error::invalid("null options"));
    }

    // SAFETY: every input begins with an aligned readable u64. The caller
    // supplies that word even for a rejected size, and the declared bytes for
    // accepted sizes. No reference to a possibly truncated T is constructed.
    let length = unsafe { pointer.cast::<u64>().read() };

    let length =
        usize::try_from(length).map_err(|_| Error::invalid("options size exceeds host range"))?;

    if length > 65_536 || (length < size_of::<T>() && !T::PREFIX_SIZES.contains(&length)) {
        return Err(Error::invalid("unsupported options size"));
    }

    if length > size_of::<T>() {
        // SAFETY: caller-owned storage includes the initialized extension.
        // Inspect only unknown bytes, never padding inside the known layout.
        let extension = unsafe {
            std::slice::from_raw_parts(
                pointer.cast::<u8>().add(size_of::<T>()),
                length - size_of::<T>(),
            )
        };

        if extension.iter().any(|byte| *byte != 0) {
            return Err(Error::invalid("unsupported nonzero options extension"));
        }
    }

    let mut value = MaybeUninit::<T>::zeroed();

    // SAFETY: the accepted prefix contains complete initialized fields, possibly
    // uninitialized padding, and fits the destination. Copying padding is valid;
    // it is never read as a value. SizedInput guarantees zero-valid remaining
    // fields. These disjoint allocations stay live for the copy; nested pointers
    // are still borrowed only for this entry call, just as in a full-size input.
    unsafe {
        ptr::copy_nonoverlapping(
            pointer.cast::<u8>(),
            value.as_mut_ptr().cast::<u8>(),
            length.min(size_of::<T>()),
        );

        Ok(value.assume_init())
    }
}

unsafe fn entry(
    error: *mut *mut Error,
    mutation: Option<MutationOutcome>,
    action: impl FnOnce() -> Result<(), Error>,
) -> u32 {
    if !error.is_null() {
        // SAFETY: caller error slot is writable and disjoint from all arguments.
        unsafe { error.write(ptr::null_mut()) };
    }

    let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(action));

    let failure = match result {
        Ok(Ok(())) => return 0,
        Ok(Err(mut error)) => {
            if let Some(outcome) = mutation {
                error.outcome.get_or_insert(outcome);
            }

            error
        }

        Err(_) => {
            let error = Error::new("IoError", "native entry panicked");

            if mutation.is_some() {
                error.mutation(MutationOutcome::Unknown)
            } else {
                error
            }
        }
    };

    if !error.is_null() {
        // SAFETY: initialized exclusive error slot receives one library-owned box.
        unsafe { error.write(Box::into_raw(Box::new(failure))) };
    }

    1
}

#[derive(Clone, Copy)]
struct PairLimits {
    entries: u64,
    bytes: u64,
}

const METADATA_LIMITS: PairLimits = PairLimits {
    entries: 64,
    bytes: 2048,
};

const TAG_LIMITS: PairLimits = PairLimits {
    entries: 10,
    // A UTF-16 unit cannot require more than four UTF-8 bytes. This deliberately
    // loose allocation bound leaves exact unit/character validation in core.
    bytes: 10 * (128 + 256) * 4,
};

unsafe fn pairs(
    pointer: *const Pair,
    count: u64,
    limits: PairLimits,
) -> Result<Vec<(String, String)>, Error> {
    if count > limits.entries || (pointer.is_null() && count != 0) {
        return Err(Error::invalid("invalid entry array"));
    }

    if count == 0 {
        return Ok(Vec::new());
    }

    let count =
        usize::try_from(count).map_err(|_| Error::invalid("entry count exceeds host range"))?;

    // SAFETY: caller supplies count aligned readable pairs; finite bound checked.
    let entries = unsafe { std::slice::from_raw_parts(pointer, count) };

    // Check the whole byte budget before decoding or copying any caller string.
    // A rejected late entry must not first allocate all preceding values.
    let mut remaining = limits.bytes;

    for pair in entries {
        remaining = remaining
            .checked_sub(pair.key.len)
            .and_then(|remaining| remaining.checked_sub(pair.value.len))
            .ok_or_else(|| Error::invalid("entry array exceeds its byte budget"))?;
    }

    entries
        .iter()
        .map(|pair| {
            // SAFETY: each pair contains caller-owned UTF-8 spans for this call.
            unsafe { Ok((pair.key.text()?.into(), pair.value.text()?.into())) }
        })
        .collect()
}

#[unsafe(no_mangle)]
pub(crate) extern "C" fn mikrio_v1_abi_version() -> u32 {
    1
}

/// # Safety
/// Options must provide an aligned readable size word and all declared input
/// bytes and spans for this call. Output slots must be aligned, writable and
/// disjoint, following the allocation rules in `include/mikrio.h`.
#[unsafe(no_mangle)]
pub(crate) unsafe extern "C" fn mikrio_v1_store_open(
    options: *const OpenOptions,
    result: *mut *mut StoreHandle,
    error: *mut *mut Error,
) -> u32 {
    // SAFETY: header requires valid/disjoint pointers for the duration of this call.
    unsafe {
        entry(error, None, || {
            let result = output(result)?;
            let options = sized(options)?;

            let create_if_missing = match options.create_if_missing.get()? {
                None | Some(1) => true,
                Some(0) => false,
                Some(_) => return Err(Error::invalid("create_if_missing must be zero or one")),
            };

            let mut input = mikrio::OpenOptions::new(options.directory.text()?)
                .create_if_missing(create_if_missing);

            if let Some(region) = options.region.optional_text()? {
                input = input.region(region);
            }

            let runtime = HostRuntime::shared()?;

            let (store, diagnostics) =
                handles::wait(runtime.submit(mikrio::embedding::open(input))?)??;

            *result = Box::into_raw(Box::new(StoreHandle(ProcessBound::new(Connection {
                runtime,
                store,
                close: CloseGate::default(),
                diagnostics,
                registered: Arc::default(),
                diagnostic_closed: Arc::default(),
            }))));

            Ok(())
        })
    }
}

/// # Safety
/// The handle must remain live and aligned throughout this call, without racing
/// release. Any error slot must be aligned, writable and disjoint from the handle.
#[unsafe(no_mangle)]
pub(crate) unsafe extern "C" fn mikrio_v1_store_close(
    store: *const StoreHandle,
    error: *mut *mut Error,
) -> u32 {
    // SAFETY: live immutable handle and writable disjoint error slot.
    unsafe { entry(error, None, || required(store)?.0.get()?.close()) }
}

/// # Safety
/// Keep the aligned handle live and immutable throughout this call, without
/// racing release. Output slots must be aligned, writable and disjoint, as
/// specified in `include/mikrio.h`.
#[unsafe(no_mangle)]
pub(crate) unsafe extern "C" fn mikrio_v1_store_info(
    store: *const StoreHandle,
    result: *mut *mut Info,
    error: *mut *mut Error,
) -> u32 {
    // SAFETY: live handle and exclusive output slots; all values are copied.
    unsafe {
        entry(error, None, || {
            let result = output(result)?;
            let info = required(store)?.0.get()?.store.info().clone();

            let directory = info
                .directory()
                .to_str()
                .ok_or_else(|| Error::invalid("store directory is not UTF-8"))?
                .to_owned();

            *result = Box::into_raw(Box::new(Info { info, directory }));
            Ok(())
        })
    }
}

/// # Safety
/// A nonnull pointer must identify the matching live, aligned library allocation.
/// Keep it immutable and unreleased until all returned borrowed views expire.
#[unsafe(no_mangle)]
pub(crate) unsafe extern "C" fn mikrio_v1_info_view(info: *const Info) -> InfoView {
    // SAFETY: caller keeps this immutable allocation alive while using its views.
    let Some(info) = (unsafe { info.as_ref() }) else {
        return InfoView::default();
    };

    InfoView {
        store_id: Span::borrowed(info.info.store_id()),
        format_version: info.info.format_version(),
        reserved: 0,
        directory: Span::borrowed(&info.directory),
        region: Span::borrowed(info.info.region()),
    }
}

/// # Safety
/// A nonnull pointer must identify the matching live, aligned library allocation.
/// Keep it immutable and unreleased until all returned borrowed views expire.
#[unsafe(no_mangle)]
pub(crate) unsafe extern "C" fn mikrio_v1_error_view(error: *const Error) -> ErrorView {
    // SAFETY: caller keeps this detached allocation alive while using its views.
    let Some(error) = (unsafe { error.as_ref() }) else {
        return ErrorView::default();
    };

    let mutation_outcome = match error.outcome {
        None => 0,
        Some(MutationOutcome::NotCommitted) => 1,
        Some(MutationOutcome::Committed) => 2,
        Some(MutationOutcome::Partial) => 4,
        Some(_) => 3,
    };

    ErrorView {
        code: Span::borrowed(&error.code),
        message: Span::borrowed(&error.message),
        cause: Span::optional(error.cause.as_deref()),
        mutation_outcome,
        reserved: 0,
    }
}

macro_rules! release {
    ($name:ident, $ty:ty) => {
        /// # Safety
        /// A nonnull value must be the matching library allocation, released
        /// exactly once with no concurrent use or remaining borrowed views.
        #[unsafe(no_mangle)]
        pub(crate) unsafe extern "C" fn $name(value: *mut $ty) {
            if value.is_null() {
                return;
            }

            // SAFETY: exactly one matching allocation, no concurrent use/release.
            // Containment includes Drop; a panic cannot cross the C boundary.
            let _contained = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| unsafe {
                drop(Box::from_raw(value));
            }));
        }
    };
}

// These modules use the input-layout and release macros defined above.
mod access_configuration;
mod admin;
mod cors;
mod deletion;
mod lifecycle;
mod listener;
mod listing;
mod multipart;
mod object_attributes;
mod object_tagging;

#[cfg(test)]
mod input_tests;

release!(mikrio_v1_store_release, StoreHandle);
release!(mikrio_v1_info_release, Info);
release!(mikrio_v1_error_release, Error);
release!(mikrio_v1_writer_release, WriterHandle);
release!(mikrio_v1_body_release, BodyHandle);
release!(mikrio_v1_put_output_release, mikrio::PutObjectOutput);
release!(mikrio_v1_read_output_release, ReadOutput);
release!(mikrio_v1_bucket_output_release, String);

type CallbackFn = unsafe extern "C" fn(*mut std::ffi::c_void, Span);

struct Callback {
    function: CallbackFn,
    context: *mut std::ffi::c_void,
}

// SAFETY: registration's C contract requires context to be valid for the
// serialized observer thread through successful unregister/release. No other
// Rust access to the pointed-to data occurs; the foreign host synchronizes its
// own concurrent access. This does not grant Sync or allow retained event spans.
unsafe impl Send for Callback {}

impl Callback {
    fn invoke(&self, message: &str) {
        // SAFETY: caller-admitted function/context remain live through the drain
        // boundary; message is immutable and borrowed only for this invocation.
        unsafe {
            (self.function)(self.context, Span::borrowed(message));
        }
    }
}

pub(crate) struct DiagnosticHandle(ProcessBound<crate::diagnostics::Observer>);

/// # Safety
/// Keep the aligned handle live and immutable throughout this call, without
/// racing release. Output slots must be aligned, writable and disjoint, as
/// specified in `include/mikrio.h`.
/// Keep callback code and context live until successful diagnostic drain;
/// synchronize foreign context access and never unwind or retain event spans.
#[unsafe(no_mangle)]
pub(crate) unsafe extern "C" fn mikrio_v1_store_register_diagnostics(
    store: *const StoreHandle,
    callback: Option<CallbackFn>,
    context: *mut std::ffi::c_void,
    result: *mut *mut DiagnosticHandle,
    error: *mut *mut Error,
) -> u32 {
    // SAFETY: pointer contract plus callback lifetime/thread rules in the header.
    unsafe {
        entry(error, None, || {
            let result = output(result)?;
            let owner = required(store)?.0.get()?;

            let function = callback.ok_or_else(|| Error::invalid("null diagnostic callback"))?;
            let callback = Callback { function, context };

            let observer = crate::diagnostics::Observer::start(
                owner.diagnostics.clone(),
                Arc::clone(&owner.registered),
                Arc::clone(&owner.diagnostic_closed),
                move |message| callback.invoke(message),
            )?;

            *result = Box::into_raw(Box::new(DiagnosticHandle(ProcessBound::new(observer))));
            Ok(())
        })
    }
}

/// # Safety
/// The handle must remain live and aligned throughout this call, without racing
/// release. Any error slot must be aligned, writable and disjoint from the handle.
#[unsafe(no_mangle)]
pub(crate) unsafe extern "C" fn mikrio_v1_diagnostics_unregister(
    observer: *const DiagnosticHandle,
    error: *mut *mut Error,
) -> u32 {
    // SAFETY: allocation remains live through all concurrent unregister calls.
    unsafe { entry(error, None, || required(observer)?.0.get()?.unregister()) }
}

/// # Safety
/// A nonnull observer must be its matching live library allocation, exclusively
/// owned for this call. The disjoint error slot must be aligned and writable.
/// Keep callback code and context live until drain succeeds; failure retains
/// the observer allocation. See `include/mikrio.h` for callback reentrancy rules.
#[unsafe(no_mangle)]
pub(crate) unsafe extern "C" fn mikrio_v1_diagnostics_release(
    observer: *mut DiagnosticHandle,
    error: *mut *mut Error,
) -> u32 {
    // SAFETY: exclusive allocation ownership; on failed drain it is not freed.
    unsafe {
        entry(error, None, || {
            if observer.is_null() {
                return Ok(());
            }

            required(observer)?.0.get()?.unregister()?;

            drop(Box::from_raw(observer));
            Ok(())
        })
    }
}

/// # Safety
/// A nonnull pointer must identify the matching live, aligned library allocation.
/// Keep it immutable and unreleased until all returned borrowed views expire.
#[unsafe(no_mangle)]
pub(crate) unsafe extern "C" fn mikrio_v1_put_output_view(
    value: *const mikrio::PutObjectOutput,
) -> PutView {
    // SAFETY: immutable output remains allocated while borrowed views are used.
    let Some(value) = (unsafe { value.as_ref() }) else {
        return PutView::default();
    };

    PutView {
        etag: Span::borrowed(value.etag()),
        checksum_crc32: Span::optional(value.checksum_crc32()),
        checksum_crc32c: Span::optional(value.checksum_crc32c()),
        checksum_sha1: Span::optional(value.checksum_sha1()),
        checksum_sha256: Span::optional(value.checksum_sha256()),
        checksum_crc64_nvme: Span::optional(value.checksum_crc64_nvme()),
    }
}

/// # Safety
/// A nonnull pointer must identify the matching live, aligned library allocation.
/// Keep it immutable and unreleased until all returned borrowed views expire.
#[unsafe(no_mangle)]
pub(crate) unsafe extern "C" fn mikrio_v1_read_output_view(value: *const ReadOutput) -> ReadView {
    // SAFETY: immutable output and its pair array remain live until release.
    let Some(value) = (unsafe { value.as_ref() }) else {
        return ReadView::default();
    };

    let facts = &value.facts;

    ReadView {
        content_length: facts.content_length(),
        etag: Span::borrowed(facts.etag()),
        last_modified_ms: facts.last_modified().as_unix_millis(),
        metadata: value.metadata.as_ptr(),
        metadata_count: u64::try_from(value.metadata.len()).unwrap_or(u64::MAX),
        tag_count: facts.tag_count(),
        reserved: 0,
        content_type: Span::optional(facts.content_type()),
        content_encoding: Span::optional(facts.content_encoding()),
        content_disposition: Span::optional(facts.content_disposition()),
        content_language: Span::optional(facts.content_language()),
        cache_control: Span::optional(facts.cache_control()),
        expires: OptionalI64::from_timestamp(facts.expires()),
        content_range: Span::optional(facts.content_range()),
        parts_count: facts
            .parts_count()
            .map_or_else(OptionalU64::default, |value| OptionalU64 {
                present: 1,
                reserved: 0,
                value: u64::from(value),
            }),
        checksum_crc32: Span::optional(facts.checksum_crc32()),
        checksum_crc32c: Span::optional(facts.checksum_crc32c()),
        checksum_sha1: Span::optional(facts.checksum_sha1()),
        checksum_sha256: Span::optional(facts.checksum_sha256()),
        checksum_crc64_nvme: Span::optional(facts.checksum_crc64_nvme()),
        checksum_type: Span::optional(facts.checksum_type().map(mikrio::ChecksumType::as_str)),
    }
}

fn parse_checksum_algorithm(value: &str) -> Result<mikrio::ChecksumAlgorithm, Error> {
    match value {
        "CRC32" => Ok(mikrio::ChecksumAlgorithm::Crc32),
        "CRC32C" => Ok(mikrio::ChecksumAlgorithm::Crc32c),
        "CRC64NVME" => Ok(mikrio::ChecksumAlgorithm::Crc64Nvme),
        "SHA1" => Ok(mikrio::ChecksumAlgorithm::Sha1),
        "SHA256" => Ok(mikrio::ChecksumAlgorithm::Sha256),
        _ => Err(Error::invalid("unsupported checksum algorithm")),
    }
}

unsafe fn put_input(
    input: &PutInput,
    source: mikrio::BodySource,
) -> Result<mikrio::PutObjectInput, Error> {
    // SAFETY: entry validated the sized input; every nested span/array remains
    // readable until return and is copied into the owned facade builder.
    unsafe {
        let mut builder =
            mikrio::PutObjectInput::builder(input.bucket.text()?, input.key.text()?, source);

        if let Some(value) = input.expected_bucket_owner.optional_text()? {
            builder = builder.expected_bucket_owner(value);
        }

        if let Some(value) = input.content_length.get()? {
            builder = builder.content_length(value);
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

        if !input.tags.is_null() || input.tag_count != 0 {
            builder = builder.tags(
                pairs(input.tags, input.tag_count, TAG_LIMITS)?
                    .into_iter()
                    .map(|(key, value)| mikrio::Tag { key, value })
                    .collect(),
            );
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

        if let Some(value) = input.if_match.optional_text()? {
            builder = builder.if_match(value);
        }

        if let Some(value) = input.if_none_match.optional_text()? {
            builder = builder.if_none_match(value);
        }

        builder.build().map_err(Error::from)
    }
}

/// # Safety
/// Follow `include/mikrio.h`: keep handles and declared input bytes live and
/// immutable for this call. Input records and arrays must be aligned; output
/// slots must be aligned, writable and disjoint. Do not race handle release.
#[unsafe(no_mangle)]
pub(crate) unsafe extern "C" fn mikrio_v1_store_begin_put_object(
    store: *const StoreHandle,
    input: *const PutInput,
    result: *mut *mut WriterHandle,
    error: *mut *mut Error,
) -> u32 {
    // SAFETY: live handle, sized borrowed input and disjoint writable outputs.
    unsafe {
        entry(error, Some(MutationOutcome::NotCommitted), || {
            let result = output(result)?;
            let owner = required(store)?.0.get()?;
            let input = sized(input)?;

            let (upload, source) = mikrio_embed::upload::Upload::channel();

            let input = put_input(&input, source)
                .map_err(|error| error.mutation(MutationOutcome::NotCommitted))?;

            let writer = Writer::begin(owner, upload, input)?;
            *result = Box::into_raw(Box::new(WriterHandle(ProcessBound::new(writer))));
            Ok(())
        })
    }
}

/// # Safety
/// Serialize access to the live, aligned writer, including against release.
/// Keep any input buffer readable and immutable for this call, and provide
/// aligned, disjoint writable output slots as specified in `include/mikrio.h`.
#[unsafe(no_mangle)]
pub(crate) unsafe extern "C" fn mikrio_v1_writer_write(
    writer: *mut WriterHandle,
    bytes: Span,
    error: *mut *mut Error,
) -> u32 {
    // SAFETY: exclusive writer use; caller bytes are borrowed only until return.
    unsafe {
        entry(error, Some(MutationOutcome::Unknown), || {
            let bytes = bytes.bytes()?;

            let writer = writer
                .as_mut()
                .ok_or_else(|| Error::invalid("null writer"))?;

            writer.0.get_mut()?.write(bytes)
        })
    }
}

/// # Safety
/// Serialize access to the live, aligned writer, including against release.
/// Output slots must be aligned, writable and disjoint, as specified in
/// `include/mikrio.h`.
#[unsafe(no_mangle)]
pub(crate) unsafe extern "C" fn mikrio_v1_writer_finish(
    writer: *mut WriterHandle,
    result: *mut *mut mikrio::PutObjectOutput,
    error: *mut *mut Error,
) -> u32 {
    // SAFETY: exclusive writer use and disjoint initialized output slots.
    unsafe {
        entry(error, Some(MutationOutcome::Unknown), || {
            let result = output(result)?;

            let writer = writer
                .as_mut()
                .ok_or_else(|| Error::invalid("null writer"))?;

            *result = Box::into_raw(Box::new(writer.0.get_mut()?.finish()?));
            Ok(())
        })
    }
}

/// # Safety
/// Serialize access to the live, aligned writer, including against release.
/// Output slots must be aligned, writable and disjoint, as specified in
/// `include/mikrio.h`.
#[unsafe(no_mangle)]
pub(crate) unsafe extern "C" fn mikrio_v1_writer_abort(
    writer: *mut WriterHandle,
    error: *mut *mut Error,
) -> u32 {
    // SAFETY: caller serializes all uses of this live writer allocation.
    unsafe {
        entry(error, Some(MutationOutcome::Unknown), || {
            let writer = writer
                .as_mut()
                .ok_or_else(|| Error::invalid("null writer"))?;

            writer.0.get_mut()?.abort()
        })
    }
}

// The two facade builders deliberately stay distinct operation types. This
// macro only translates their identical implemented input field projections.
macro_rules! read_input {
    ($input:ident, $kind:ident) => {{
        let mut builder = mikrio::$kind::builder($input.bucket.text()?, $input.key.text()?);

        if let Some(value) = $input.expected_bucket_owner.optional_text()? {
            builder = builder.expected_bucket_owner(value);
        }

        if let Some(value) = $input.range.optional_text()? {
            builder = builder.range(value);
        }

        if let Some(value) = $input.part_number.get()? {
            builder = builder.part_number(
                u32::try_from(value)
                    .map_err(|_| Error::invalid("part number exceeds the supported range"))?,
            );
        }

        if let Some(value) = $input.response_content_type.optional_text()? {
            builder = builder.response_content_type(value);
        }

        if let Some(value) = $input.response_content_encoding.optional_text()? {
            builder = builder.response_content_encoding(value);
        }

        if let Some(value) = $input.response_content_disposition.optional_text()? {
            builder = builder.response_content_disposition(value);
        }

        if let Some(value) = $input.response_content_language.optional_text()? {
            builder = builder.response_content_language(value);
        }

        if let Some(value) = $input.response_cache_control.optional_text()? {
            builder = builder.response_cache_control(value);
        }

        if let Some(value) = $input.response_expires.timestamp()? {
            builder = builder.response_expires(value);
        }

        if let Some(value) = $input.if_match.optional_text()? {
            builder = builder.if_match(value);
        }

        if let Some(value) = $input.if_none_match.optional_text()? {
            builder = builder.if_none_match(value);
        }

        if let Some(value) = $input.if_modified_since.timestamp()? {
            builder = builder.if_modified_since(value);
        }

        if let Some(value) = $input.if_unmodified_since.timestamp()? {
            builder = builder.if_unmodified_since(value);
        }

        if let Some(value) = $input.checksum_mode.optional_text()? {
            if value != "ENABLED" {
                return Err(Error::invalid("unsupported checksum mode"));
            }

            builder = builder.checksum_mode(mikrio::ChecksumMode::Enabled);
        }

        builder.build()?
    }};
}

/// # Safety
/// Follow `include/mikrio.h`: keep handles and declared input bytes live and
/// immutable for this call. Input records and arrays must be aligned; output
/// slots must be aligned, writable and disjoint. Do not race handle release.
#[unsafe(no_mangle)]
pub(crate) unsafe extern "C" fn mikrio_v1_store_head_object(
    store: *const StoreHandle,
    input: *const ReadInput,
    result: *mut *mut ReadOutput,
    error: *mut *mut Error,
) -> u32 {
    // SAFETY: live/sized inputs and disjoint writable output slots.
    unsafe {
        entry(error, None, || {
            let result = output(result)?;
            let owner = required(store)?.0.get()?;
            let input = sized(input)?;

            let input = read_input!(input, HeadObjectInput);

            let store = owner.store.clone();

            let facts = handles::wait(
                owner
                    .runtime
                    .submit(async move { store.head_object(input).await })?,
            )??;

            *result = Box::into_raw(Box::new(ReadOutput::new(facts)));
            Ok(())
        })
    }
}

/// # Safety
/// Follow `include/mikrio.h`: keep handles and declared input bytes live and
/// immutable for this call. Input records and arrays must be aligned; output
/// slots must be aligned, writable and disjoint. Do not race handle release.
#[unsafe(no_mangle)]
pub(crate) unsafe extern "C" fn mikrio_v1_store_get_object(
    store: *const StoreHandle,
    input: *const ReadInput,
    result: *mut *mut ReadOutput,
    body: *mut *mut BodyHandle,
    error: *mut *mut Error,
) -> u32 {
    // SAFETY: all output slots are disjoint; caller inputs outlive conversion.
    unsafe {
        entry(error, None, || {
            let result = output(result);
            let body = output(body);
            let result = result?;
            let body = body?;
            let owner = required(store)?.0.get()?;
            let input = sized(input)?;

            let input = read_input!(input, GetObjectInput);

            let store = owner.store.clone();

            let (facts, cursor) = handles::wait(owner.runtime.submit(async move {
                let output = store.get_object(input).await?;
                let (facts, body) = mikrio::embedding::split_get(output);
                let body = mikrio_embed::body::Body::new(body, facts.content_length());
                Ok::<_, mikrio::StorageError>((facts, body))
            })?)??;

            let facts = Box::new(ReadOutput::new(facts));

            let cursor = Box::new(BodyHandle(ProcessBound::new(Body {
                runtime: Arc::clone(&owner.runtime),
                body: cursor,
            })));

            *result = Box::into_raw(facts);
            *body = Box::into_raw(cursor);
            Ok(())
        })
    }
}

/// # Safety
/// Follow `include/mikrio.h`: keep handles and declared input bytes live and
/// immutable for this call. Input records and arrays must be aligned; output
/// slots must be aligned, writable and disjoint. Do not race handle release.
#[unsafe(no_mangle)]
pub(crate) unsafe extern "C" fn mikrio_v1_store_delete_object(
    store: *const StoreHandle,
    input: *const DeleteInput,
    error: *mut *mut Error,
) -> u32 {
    // SAFETY: caller retains live input storage; native task receives owned values.
    unsafe {
        entry(error, Some(MutationOutcome::NotCommitted), || {
            let owner = required(store)?.0.get()?;
            let input = sized(input)?;

            let mut builder =
                mikrio::DeleteObjectInput::builder(input.bucket.text()?, input.key.text()?);

            if let Some(value) = input.expected_bucket_owner.optional_text()? {
                builder = builder.expected_bucket_owner(value);
            }

            if let Some(value) = input.if_match.optional_text()? {
                builder = builder.if_match(value);
            }

            let input = builder
                .build()
                .map_err(|error| Error::from(error).mutation(MutationOutcome::NotCommitted))?;

            let store = owner.store.clone();

            let pending = owner
                .runtime
                .submit(async move { store.delete_object(input).await })
                .map_err(|error| Error::from(error).mutation(MutationOutcome::NotCommitted))?;

            handles::wait_mutation(pending)??;
            Ok(())
        })
    }
}

/// # Safety
/// Keep the aligned body handle live without racing release. The buffer must be
/// writable for `capacity` bytes; count and error slots must be aligned, writable
/// and disjoint from the buffer and handle. Follow `include/mikrio.h` cursor rules.
#[unsafe(no_mangle)]
pub(crate) unsafe extern "C" fn mikrio_v1_body_read_chunk(
    body: *const BodyHandle,
    buffer: *mut u8,
    capacity: u64,
    count: *mut u64,
    error: *mut *mut Error,
) -> u32 {
    // SAFETY: caller supplies exclusive count/buffer storage; body may also be
    // closed concurrently, but never released. No pointer enters native work.
    unsafe {
        entry(error, None, || {
            let count = count
                .as_mut()
                .ok_or_else(|| Error::invalid("null byte count"))?;

            *count = 0;

            if buffer.is_null()
                || capacity == 0
                || capacity > u64::try_from(isize::MAX).unwrap_or(u64::MAX)
            {
                return Err(Error::invalid(
                    "read requires a nonnull positive-capacity buffer",
                ));
            }

            let body = required(body)?.0.get()?;

            let capacity = usize::try_from(capacity)
                .map_err(|_| Error::invalid("capacity exceeds host range"))?;

            let mut reading = body.body.start_read()?;

            let bytes = handles::transfer(&body.runtime, reading.next(capacity))??;

            if let Some(bytes) = bytes {
                // Destination is exclusively writable for capacity bytes, source is
                // a distinct native allocation and never exceeds that capacity.
                ptr::copy_nonoverlapping(bytes.as_ptr(), buffer, bytes.len());

                *count = u64::try_from(bytes.len())
                    .map_err(|_| Error::invalid("native byte count exceeds u64"))?;
            }

            reading.finish();
            Ok(())
        })
    }
}

/// # Safety
/// The handle must remain live and aligned throughout this call, without racing
/// release. Any error slot must be aligned, writable and disjoint from the handle.
#[unsafe(no_mangle)]
pub(crate) unsafe extern "C" fn mikrio_v1_body_close(
    body: *const BodyHandle,
    error: *mut *mut Error,
) -> u32 {
    // SAFETY: body handle stays live; underlying close is synchronized and joinable.
    unsafe {
        entry(error, None, || {
            let body = required(body)?.0.get()?;

            handles::transfer(&body.runtime, body.body.close())??;
            Ok(())
        })
    }
}

/// # Safety
/// A nonnull pointer must identify the matching live, aligned library allocation.
/// Keep it immutable and unreleased until all returned borrowed views expire.
#[unsafe(no_mangle)]
pub(crate) unsafe extern "C" fn mikrio_v1_bucket_output_view(value: *const String) -> Span {
    // SAFETY: caller retains the detached output while using its borrowed view.
    Span::optional(unsafe { value.as_ref() }.map(String::as_str))
}

/// # Safety
/// Follow `include/mikrio.h`: keep handles and declared input bytes live and
/// immutable for this call. Input records and arrays must be aligned; output
/// slots must be aligned, writable and disjoint. Do not race handle release.
#[unsafe(no_mangle)]
pub(crate) unsafe extern "C" fn mikrio_v1_store_create_bucket(
    store: *const StoreHandle,
    input: *const BucketInput,
    error: *mut *mut Error,
) -> u32 {
    // SAFETY: header pointer/size/borrow contract; no input reference is retained.
    unsafe {
        entry(error, Some(MutationOutcome::NotCommitted), || {
            let owner = required(store)?.0.get()?;
            let input = sized(input)?;

            if input.expected_bucket_owner.optional_text()?.is_some() {
                return Err(
                    Error::invalid("CreateBucket does not accept expected_bucket_owner")
                        .mutation(MutationOutcome::NotCommitted),
                );
            }

            let input = mikrio::CreateBucketInput::builder(input.bucket.text()?)
                .build()
                .map_err(|error| Error::from(error).mutation(MutationOutcome::NotCommitted))?;

            let store = owner.store.clone();

            let pending = owner
                .runtime
                .submit(async move { store.create_bucket(input).await })
                .map_err(|error| Error::from(error).mutation(MutationOutcome::NotCommitted))?;

            handles::wait_mutation(pending)??;
            Ok(())
        })
    }
}

/// # Safety
/// Follow `include/mikrio.h`: keep handles and declared input bytes live and
/// immutable for this call. Input records and arrays must be aligned; output
/// slots must be aligned, writable and disjoint. Do not race handle release.
#[unsafe(no_mangle)]
pub(crate) unsafe extern "C" fn mikrio_v1_store_head_bucket(
    store: *const StoreHandle,
    input: *const BucketInput,
    result: *mut *mut String,
    error: *mut *mut Error,
) -> u32 {
    // SAFETY: live inputs and exclusive output slots; values are copied before wait.
    unsafe {
        entry(error, None, || {
            let result = output(result)?;
            let owner = required(store)?.0.get()?;
            let input = sized(input)?;

            let mut builder = mikrio::HeadBucketInput::builder(input.bucket.text()?);

            if let Some(value) = input.expected_bucket_owner.optional_text()? {
                builder = builder.expected_bucket_owner(value);
            }

            let input = builder.build()?;

            let store = owner.store.clone();

            let value = handles::wait(
                owner
                    .runtime
                    .submit(async move { store.head_bucket(input).await })?,
            )??;

            *result = Box::into_raw(Box::new(value.bucket_region().to_owned()));
            Ok(())
        })
    }
}

/// # Safety
/// Follow `include/mikrio.h`: keep handles and declared input bytes live and
/// immutable for this call. Input records and arrays must be aligned; output
/// slots must be aligned, writable and disjoint. Do not race handle release.
#[unsafe(no_mangle)]
pub(crate) unsafe extern "C" fn mikrio_v1_store_get_bucket_location(
    store: *const StoreHandle,
    input: *const BucketInput,
    result: *mut *mut String,
    error: *mut *mut Error,
) -> u32 {
    // SAFETY: live inputs and exclusive output slots; values are copied before wait.
    unsafe {
        entry(error, None, || {
            let result = output(result)?;
            let owner = required(store)?.0.get()?;
            let input = sized(input)?;

            let mut builder = mikrio::GetBucketLocationInput::builder(input.bucket.text()?);

            if let Some(value) = input.expected_bucket_owner.optional_text()? {
                builder = builder.expected_bucket_owner(value);
            }

            let input = builder.build()?;

            let store = owner.store.clone();

            let value = handles::wait(
                owner
                    .runtime
                    .submit(async move { store.get_bucket_location(input).await })?,
            )??;

            if let Some(region) = value.location_constraint() {
                *result = Box::into_raw(Box::new(region.to_owned()));
            }

            Ok(())
        })
    }
}

/// # Safety
/// Follow `include/mikrio.h`: keep handles and declared input bytes live and
/// immutable for this call. Input records and arrays must be aligned; output
/// slots must be aligned, writable and disjoint. Do not race handle release.
#[unsafe(no_mangle)]
pub(crate) unsafe extern "C" fn mikrio_v1_store_delete_bucket(
    store: *const StoreHandle,
    input: *const BucketInput,
    error: *mut *mut Error,
) -> u32 {
    // SAFETY: caller owns input storage throughout this synchronous call.
    unsafe {
        entry(error, Some(MutationOutcome::NotCommitted), || {
            let owner = required(store)?.0.get()?;
            let input = sized(input)?;

            let mut builder = mikrio::DeleteBucketInput::builder(input.bucket.text()?);

            if let Some(value) = input.expected_bucket_owner.optional_text()? {
                builder = builder.expected_bucket_owner(value);
            }

            let input = builder
                .build()
                .map_err(|error| Error::from(error).mutation(MutationOutcome::NotCommitted))?;

            let store = owner.store.clone();

            let pending = owner
                .runtime
                .submit(async move { store.delete_bucket(input).await })
                .map_err(|error| Error::from(error).mutation(MutationOutcome::NotCommitted))?;

            handles::wait_mutation(pending)??;
            Ok(())
        })
    }
}

#[cfg(test)]
#[allow(
    clippy::panic,
    reason = "controlled entry/future unwinds test the foreign containment boundary"
)]
mod tests {
    use super::*;

    #[test]
    fn unwinding_entry_initializes_outputs_and_preserves_an_unknown_mutation() {
        let mut failure = ptr::null_mut();
        let mut result = ptr::dangling_mut::<u8>();

        // SAFETY: these local disjoint slots are valid through entry and error reclamation.
        unsafe {
            let status = entry(
                &raw mut failure,
                Some(MutationOutcome::NotCommitted),
                || {
                    let _result = output(&raw mut result)?;

                    panic!("controlled entry unwind");
                },
            );

            assert!(!failure.is_null(), "entry must return an owned error");

            // Reclaim the matching allocation even if a later assertion fails.
            let failure = Box::from_raw(failure);

            assert_eq!(status, 1);
            assert!(result.is_null());
            assert_eq!(failure.outcome, Some(MutationOutcome::Unknown));
            assert_eq!(failure.message, "native entry panicked");
        }
    }

    #[test]
    fn native_future_panic_is_contained_before_foreign_delivery() {
        let runtime = HostRuntime::shared().expect("native runtime");
        let mut failure = ptr::null_mut();

        // SAFETY: exclusive local error slot and exactly one matching allocation reclamation.
        unsafe {
            let status = entry(&raw mut failure, Some(MutationOutcome::Unknown), || {
                let pending = runtime.submit(async { panic!("controlled poll unwind") })?;

                handles::wait::<()>(pending)
            });

            assert!(!failure.is_null(), "entry must return an owned error");

            // Reclaim the matching allocation even if a later assertion fails.
            let failure = Box::from_raw(failure);

            assert_eq!(status, 1);
            assert_eq!(failure.code, "IoError");
            assert_eq!(failure.outcome, Some(MutationOutcome::Unknown));
        }

        assert_eq!(
            handles::wait(runtime.submit(async { 7 }).unwrap()).unwrap(),
            7
        );
    }

    #[test]
    fn lost_mutation_delivery_preserves_unknown_after_durable_creation() -> Result<(), String> {
        crate::test_store::run("outcome", |owner| {
            let runtime = &owner.runtime;
            let store = &owner.store;

            let worker = store.clone();

            handles::wait(
                runtime
                    .submit(async move {
                        worker
                            .create_bucket(
                                mikrio::CreateBucketInput::builder("attachments").build()?,
                            )
                            .await
                    })
                    .unwrap(),
            )
            .unwrap()
            .unwrap();

            let mut failure = ptr::null_mut();
            let mut result = ptr::dangling_mut::<mikrio::CreateMultipartUploadOutput>();
            let worker = store.clone();

            // SAFETY: disjoint local slots stay live through entry and error reclamation.
            unsafe {
                let status = entry(
                    &raw mut failure,
                    Some(MutationOutcome::NotCommitted),
                    || {
                        let result = output(&raw mut result)?;

                        let pending: mikrio_embed::Pending<
                            Result<mikrio::CreateMultipartUploadOutput, mikrio::StorageError>,
                        > = runtime.submit(async move {
                            worker
                                .create_multipart_upload(
                                    mikrio::CreateMultipartUploadInput::builder(
                                        "attachments",
                                        "pending",
                                    )
                                    .build()
                                    .unwrap(),
                                )
                                .await
                                .unwrap();

                            panic!("controlled lost acknowledgment after durable creation");
                        })?;

                        *result = Box::into_raw(Box::new(handles::wait_mutation(pending)??));
                        Ok(())
                    },
                );

                assert!(!failure.is_null(), "entry must return an owned error");

                // Reclaim the matching allocation even if a later assertion fails.
                let failure = Box::from_raw(failure);

                assert_eq!(status, 1);
                assert!(result.is_null());
                assert_eq!(failure.code, "IoError");
                assert_eq!(failure.outcome, Some(MutationOutcome::Unknown));
            }

            let worker = store.clone();

            let uploads = handles::wait(
                runtime
                    .submit(async move {
                        worker
                            .list_multipart_uploads(
                                mikrio::ListMultipartUploadsInput::builder("attachments")
                                    .build()?,
                            )
                            .await
                    })
                    .unwrap(),
            )
            .unwrap()
            .unwrap();

            assert_eq!(uploads.uploads().len(), 1);
            assert_eq!(uploads.uploads()[0].key(), "pending");

            let worker = store.clone();

            let rejected = handles::wait_mutation(
                runtime
                    .submit(async move {
                        worker
                            .create_multipart_upload(
                                mikrio::CreateMultipartUploadInput::builder("absent", "pending")
                                    .build()?,
                            )
                            .await
                    })
                    .unwrap(),
            )
            .unwrap()
            .unwrap_err();

            assert_eq!(rejected.code(), mikrio::ErrorCode::NoSuchBucket);

            assert_eq!(
                rejected.mutation_outcome(),
                Some(&MutationOutcome::NotCommitted)
            );
        })
    }
}
