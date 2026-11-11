//! Borrow CORS inputs only within entry calls; result views borrow owned outputs.

use super::{
    Error, MutationOutcome, OptionalI64, OptionalU64, Span, StoreHandle, entry, handles, output,
    required, sized,
};

#[derive(Clone, Copy)]
#[repr(C)]
pub(super) struct SpanList {
    data: *const Span,
    count: u64,
}

impl SpanList {
    unsafe fn try_into_strings(self) -> Result<Vec<String>, Error> {
        if self.count > 65_536 || (self.data.is_null() && self.count != 0) {
            return Err(Error::invalid("invalid CORS string array"));
        }

        if self.count == 0 {
            return Ok(Vec::new());
        }

        let count = usize::try_from(self.count)
            .map_err(|_| Error::invalid("CORS count exceeds host range"))?;

        // SAFETY: caller supplies count aligned, readable spans for this call.
        let entries = unsafe { std::slice::from_raw_parts(self.data, count) };
        let mut strings = Vec::new();
        let mut bytes = 0_usize;

        for entry in entries {
            // SAFETY: each span is live and immutable through this entry call.
            let string = unsafe { entry.text()? };
            bytes = bytes.saturating_add(string.len()).saturating_add(5);

            if bytes > 65_536 {
                return Err(Error::invalid("oversized CORS string array"));
            }

            strings.push(string.to_owned());
        }

        Ok(strings)
    }

    unsafe fn try_into_optional_strings(self) -> Result<Option<Vec<String>>, Error> {
        // SAFETY: inherited entry-call array and span lifetime contract.
        let strings = unsafe { self.try_into_strings()? };
        Ok((!self.data.is_null()).then_some(strings))
    }
}

#[repr(C)]
pub(super) struct CorsRuleInput {
    struct_size: u64,
    id: Span,
    allowed_origins: SpanList,
    allowed_methods: SpanList,
    allowed_headers: SpanList,
    expose_headers: SpanList,
    max_age_seconds: OptionalI64,
}

unsafe fn decode_rules(
    pointer: *const CorsRuleInput,
    count: u64,
) -> Result<Vec<mikrio::CorsRule>, Error> {
    if count == 0 || count > 100 || pointer.is_null() {
        return Err(Error::invalid("invalid CORS rule array"));
    }

    let count =
        usize::try_from(count).map_err(|_| Error::invalid("CORS count exceeds host range"))?;

    // SAFETY: caller supplies count aligned readable canonical-size rule elements.
    let entries = unsafe { std::slice::from_raw_parts(pointer, count) };
    let mut result = Vec::new();

    for entry in entries {
        // Array stride is the canonical layout; extensions cannot alter a stride.
        if entry.struct_size
            != u64::try_from(std::mem::size_of::<CorsRuleInput>()).unwrap_or(u64::MAX)
        {
            return Err(Error::invalid("invalid CORS rule element size"));
        }

        // SAFETY: all nested strings and arrays share the entry-call borrow.
        let rule = unsafe {
            let mut builder = mikrio::CorsRule::builder(
                entry.allowed_origins.try_into_strings()?,
                entry.allowed_methods.try_into_strings()?,
            );

            if let Some(id) = entry.id.optional_text()? {
                builder = builder.id(id);
            }

            if let Some(headers) = entry.allowed_headers.try_into_optional_strings()? {
                builder = builder.allowed_headers(headers);
            }

            if let Some(headers) = entry.expose_headers.try_into_optional_strings()? {
                builder = builder.expose_headers(headers);
            }

            if let Some(seconds) = super::optional(
                entry.max_age_seconds.present,
                entry.max_age_seconds.reserved,
                entry.max_age_seconds.value,
            )? {
                builder = builder.max_age_seconds(
                    i32::try_from(seconds).map_err(|_| Error::invalid("invalid CORS max age"))?,
                );
            }

            builder.build()?
        };

        result.push(rule);
    }

    Ok(result)
}

#[derive(Default)]
#[repr(C)]
pub(super) struct GetBucketCorsOutputView {
    cors_rule_count: u64,
}

#[derive(Default)]
#[repr(C)]
pub(super) struct CorsRuleView {
    id: Span,
    allowed_origin_count: u64,
    allowed_method_count: u64,
    allowed_header_count: OptionalU64,
    expose_header_count: OptionalU64,
    max_age_seconds: OptionalI64,
}

fn optional_number(value: Option<u64>) -> OptionalU64 {
    value.map_or_else(OptionalU64::default, |value| OptionalU64 {
        present: 1,
        reserved: 0,
        value,
    })
}

fn count<T>(value: &[T]) -> u64 {
    u64::try_from(value.len()).unwrap_or(u64::MAX)
}

/// # Safety
/// A nonnull pointer must identify the matching live, aligned library allocation
/// and remain immutable and unreleased for this call.
#[unsafe(no_mangle)]
pub(super) unsafe extern "C" fn mikrio_v1_get_bucket_cors_output_view(
    value: *const mikrio::GetBucketCorsOutput,
) -> GetBucketCorsOutputView {
    // SAFETY: caller retains this immutable output for the duration of all views.
    unsafe { value.as_ref() }.map_or_else(GetBucketCorsOutputView::default, |value| {
        GetBucketCorsOutputView {
            cors_rule_count: count(value.cors_rules()),
        }
    })
}

release!(
    mikrio_v1_get_bucket_cors_output_release,
    mikrio::GetBucketCorsOutput
);

unsafe fn rule<'a>(
    value: *const mikrio::GetBucketCorsOutput,
    index: u64,
) -> Option<&'a mikrio::CorsRule> {
    // SAFETY: caller keeps this immutable result live through all view use.
    unsafe { value.as_ref() }?
        .cors_rules()
        .get(usize::try_from(index).ok()?)
}

/// # Safety
/// A nonnull pointer must identify the matching live, aligned library allocation.
/// Keep it immutable and unreleased until all returned borrowed views expire.
#[unsafe(no_mangle)]
pub(super) unsafe extern "C" fn mikrio_v1_get_bucket_cors_output_rule(
    value: *const mikrio::GetBucketCorsOutput,
    index: u64,
) -> CorsRuleView {
    // SAFETY: caller keeps this immutable result live through all view use.
    let Some(rule) = (unsafe { rule(value, index) }) else {
        return CorsRuleView::default();
    };

    CorsRuleView {
        id: Span::optional(rule.id()),
        allowed_origin_count: count(rule.allowed_origins()),
        allowed_method_count: count(rule.allowed_methods()),
        allowed_header_count: optional_number(rule.allowed_headers().map(count)),
        expose_header_count: optional_number(rule.expose_headers().map(count)),
        max_age_seconds: rule
            .max_age_seconds()
            .map_or_else(OptionalI64::default, |value| OptionalI64 {
                present: 1,
                reserved: 0,
                value: i64::from(value),
            }),
    }
}

/// # Safety
/// A nonnull pointer must identify the matching live, aligned library allocation.
/// Keep it immutable and unreleased until all returned borrowed views expire.
#[unsafe(no_mangle)]
pub(super) unsafe extern "C" fn mikrio_v1_get_bucket_cors_output_allowed_origin(
    value: *const mikrio::GetBucketCorsOutput,
    rule_index: u64,
    index: u64,
) -> Span {
    // SAFETY: caller keeps this immutable result live through all view use.
    let selected = unsafe { rule(value, rule_index) };

    let value = selected
        .map(mikrio::CorsRule::allowed_origins)
        .and_then(|items| {
            usize::try_from(index)
                .ok()
                .and_then(|index| items.get(index))
        });

    Span::optional(value.map(String::as_str))
}

/// # Safety
/// A nonnull pointer must identify the matching live, aligned library allocation.
/// Keep it immutable and unreleased until all returned borrowed views expire.
#[unsafe(no_mangle)]
pub(super) unsafe extern "C" fn mikrio_v1_get_bucket_cors_output_allowed_method(
    value: *const mikrio::GetBucketCorsOutput,
    rule_index: u64,
    index: u64,
) -> Span {
    // SAFETY: caller keeps this immutable result live through all view use.
    let selected = unsafe { rule(value, rule_index) };

    let value = selected
        .map(mikrio::CorsRule::allowed_methods)
        .and_then(|items| {
            usize::try_from(index)
                .ok()
                .and_then(|index| items.get(index))
        });

    Span::optional(value.map(String::as_str))
}

/// # Safety
/// A nonnull pointer must identify the matching live, aligned library allocation.
/// Keep it immutable and unreleased until all returned borrowed views expire.
#[unsafe(no_mangle)]
pub(super) unsafe extern "C" fn mikrio_v1_get_bucket_cors_output_allowed_header(
    value: *const mikrio::GetBucketCorsOutput,
    rule_index: u64,
    index: u64,
) -> Span {
    // SAFETY: caller keeps this immutable result live through all view use.
    let selected = unsafe { rule(value, rule_index) };

    let value = selected
        .and_then(mikrio::CorsRule::allowed_headers)
        .and_then(|items| {
            usize::try_from(index)
                .ok()
                .and_then(|index| items.get(index))
        });

    Span::optional(value.map(String::as_str))
}

/// # Safety
/// A nonnull pointer must identify the matching live, aligned library allocation.
/// Keep it immutable and unreleased until all returned borrowed views expire.
#[unsafe(no_mangle)]
pub(super) unsafe extern "C" fn mikrio_v1_get_bucket_cors_output_expose_header(
    value: *const mikrio::GetBucketCorsOutput,
    rule_index: u64,
    index: u64,
) -> Span {
    // SAFETY: caller keeps this immutable result live through all view use.
    let selected = unsafe { rule(value, rule_index) };

    let value = selected
        .and_then(mikrio::CorsRule::expose_headers)
        .and_then(|items| {
            usize::try_from(index)
                .ok()
                .and_then(|index| items.get(index))
        });

    Span::optional(value.map(String::as_str))
}

#[derive(Clone, Copy)]
#[repr(C)]
pub(super) struct PutBucketCorsInput {
    struct_size: u64,
    bucket: Span,
    expected_bucket_owner: Span,
    cors_rules: *const CorsRuleInput,
    cors_rule_count: u64,
}

input_layout!(PutBucketCorsInput);

/// # Safety
/// Follow `include/mikrio.h`: keep handles and declared input bytes live and
/// immutable for this call. Input records and arrays must be aligned; output
/// slots must be aligned, writable and disjoint. Do not race handle release.
#[unsafe(no_mangle)]
pub(super) unsafe extern "C" fn mikrio_v1_store_put_bucket_cors(
    store: *const StoreHandle,
    input: *const PutBucketCorsInput,
    error: *mut *mut Error,
) -> u32 {
    // SAFETY: live handle, fully readable sized input, and disjoint writable output slots.
    unsafe {
        entry(error, Some(MutationOutcome::NotCommitted), || {
            let owner = required(store)?.0.get()?;
            let input = sized(input)?;

            let mut builder = mikrio::PutBucketCorsInput::builder(
                input.bucket.text()?,
                decode_rules(input.cors_rules, input.cors_rule_count)?,
            );

            if let Some(account) = input.expected_bucket_owner.optional_text()? {
                builder = builder.expected_bucket_owner(account);
            }

            let input = builder.build()?;

            let store = owner.store.clone();

            let pending = owner
                .runtime
                .submit(async move { store.put_bucket_cors(input).await })?;

            handles::wait_mutation(pending)??;
            Ok(())
        })
    }
}

#[derive(Clone, Copy)]
#[repr(C)]
pub(super) struct GetBucketCorsInput {
    struct_size: u64,
    bucket: Span,
    expected_bucket_owner: Span,
}

input_layout!(GetBucketCorsInput, expected_bucket_owner);

/// # Safety
/// Follow `include/mikrio.h`: keep handles and declared input bytes live and
/// immutable for this call. Input records and arrays must be aligned; output
/// slots must be aligned, writable and disjoint. Do not race handle release.
#[unsafe(no_mangle)]
pub(super) unsafe extern "C" fn mikrio_v1_store_get_bucket_cors(
    store: *const StoreHandle,
    input: *const GetBucketCorsInput,
    result: *mut *mut mikrio::GetBucketCorsOutput,
    error: *mut *mut Error,
) -> u32 {
    // SAFETY: live handle, fully readable sized input, and disjoint writable output slots.
    unsafe {
        entry(error, None, || {
            let result = output(result)?;
            let owner = required(store)?.0.get()?;
            let input = sized(input)?;

            let mut builder = mikrio::GetBucketCorsInput::builder(input.bucket.text()?);

            if let Some(account) = input.expected_bucket_owner.optional_text()? {
                builder = builder.expected_bucket_owner(account);
            }

            let input = builder.build()?;

            let store = owner.store.clone();

            let pending = owner
                .runtime
                .submit(async move { store.get_bucket_cors(input).await })?;

            let value = handles::wait(pending)??;
            *result = Box::into_raw(Box::new(value));
            Ok(())
        })
    }
}

#[derive(Clone, Copy)]
#[repr(C)]
pub(super) struct DeleteBucketCorsInput {
    struct_size: u64,
    bucket: Span,
    expected_bucket_owner: Span,
}

input_layout!(DeleteBucketCorsInput, expected_bucket_owner);

/// # Safety
/// Follow `include/mikrio.h`: keep handles and declared input bytes live and
/// immutable for this call. Input records and arrays must be aligned; output
/// slots must be aligned, writable and disjoint. Do not race handle release.
#[unsafe(no_mangle)]
pub(super) unsafe extern "C" fn mikrio_v1_store_delete_bucket_cors(
    store: *const StoreHandle,
    input: *const DeleteBucketCorsInput,
    error: *mut *mut Error,
) -> u32 {
    // SAFETY: live handle, fully readable sized input, and disjoint writable output slots.
    unsafe {
        entry(error, Some(MutationOutcome::NotCommitted), || {
            let owner = required(store)?.0.get()?;
            let input = sized(input)?;

            let mut builder = mikrio::DeleteBucketCorsInput::builder(input.bucket.text()?);

            if let Some(account) = input.expected_bucket_owner.optional_text()? {
                builder = builder.expected_bucket_owner(account);
            }

            let input = builder.build()?;

            let store = owner.store.clone();

            let pending = owner
                .runtime
                .submit(async move { store.delete_bucket_cors(input).await })?;

            handles::wait_mutation(pending)??;
            Ok(())
        })
    }
}
