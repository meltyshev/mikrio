//! Private Node value adapter. Public JS imports load this addon lazily.

mod access_configuration;
mod admin;
mod body;
mod cors;
mod deletion;
mod diagnostics;
mod lifecycle;
mod listener;
mod listing;
mod multipart;
mod object_attributes;
mod object_tagging;
mod objects;
mod upload;
mod values;

use std::error::Error as _;
use std::future::{Future, poll_fn};
use std::pin::pin;
use std::task::Poll;

use mikrio::{OpenOptions, Store};
use mikrio_embed::contain;
use napi::bindgen_prelude::{JsObjectValue, PromiseRaw, ToNapiValue, Utf16String};
use napi::{Env, Error, JsError, JsValue, Result, Status};
use napi_derive::napi;
use tokio::sync::{Semaphore, watch};

static PENDING: Semaphore = Semaphore::const_new(32);

#[napi]
pub struct NativeCancellation {
    sender: watch::Sender<bool>,
}

#[napi]
impl NativeCancellation {
    /// Create an unsignaled cancellation handle for a host operation.
    #[napi(constructor, catch_unwind)]
    #[must_use]
    pub fn new() -> Self {
        let (sender, _) = watch::channel(false);

        Self { sender }
    }

    /// Signal cancellation without waiting for native work to settle.
    #[napi(catch_unwind)]
    pub fn cancel(&self) {
        self.sender.send_replace(true);
    }
}

impl Default for NativeCancellation {
    fn default() -> Self {
        Self::new()
    }
}

#[napi(object, object_from_js = false)]
pub struct NativeInfo {
    pub store_id: String,
    pub format_version: u32,
    pub directory: String,
    pub region: String,
}

#[napi]
pub struct NativeStore {
    store: Store,
    diagnostics: mikrio::embedding::Diagnostics,
}

#[napi]
impl NativeStore {
    /// Detached internal host observation handle, with no storage ownership.
    #[napi(getter, catch_unwind)]
    #[must_use]
    pub fn diagnostics(&self) -> diagnostics::NativeDiagnostics {
        diagnostics::NativeDiagnostics(self.diagnostics.clone())
    }

    /// Detached facts.
    ///
    /// # Errors
    /// Rejects a directory that cannot be represented as a JavaScript string.
    #[napi(getter, catch_unwind)]
    pub fn info(&self, env: Env) -> Result<NativeInfo> {
        let facts = self.store.info();

        let directory = facts.directory().to_str().ok_or_else(|| {
            host_error(
                env,
                "InvalidArgument",
                "directory must be representable as UTF-8",
            )
        })?;

        Ok(NativeInfo {
            store_id: facts.store_id().to_owned(),
            format_version: facts.format_version(),
            directory: directory.to_owned(),
            region: facts.region().to_owned(),
        })
    }

    /// Join explicit native shutdown.
    ///
    /// # Errors
    /// Reports framework dispatch and native shutdown errors.
    #[napi(catch_unwind)]
    pub fn close<'env>(&self, env: &'env Env) -> Result<PromiseRaw<'env, ()>> {
        // The JS owner submits at most one close per connection and shares its
        // promise. Shutdown must not compete with blocked acquisition permits:
        // it may be what releases the resources those acquisitions need.
        let store = self.store.clone();

        env.spawn_future_with_callback(
            async move { Ok(contain(async move { store.close().await }).await) },
            |env, result| {
                result
                    .map_err(|_| host_error(*env, "IoError", "native operation panicked"))?
                    .map_err(|error| native_error(*env, &error))
            },
        )
    }
}

/// Open through the facade using NAPI's executor.
///
/// # Errors
/// Reports invalid UTF-16, bounded admission, cancellation and native failures.
#[napi(catch_unwind)]
#[allow(
    clippy::needless_pass_by_value,
    reason = "NAPI decodes an owned UTF-16 input before the asynchronous boundary"
)]
pub fn open<'env>(
    env: &'env Env,
    directory: Utf16String,
    region: Option<Utf16String>,
    create_if_missing: bool,
    cancellation: &NativeCancellation,
) -> Result<PromiseRaw<'env, NativeStore>> {
    let directory = try_to_utf8(*env, &directory, "directory must be representable as UTF-8")?;
    let mut options = OpenOptions::new(directory).create_if_missing(create_if_missing);

    if let Some(region) = region {
        options = options.region(try_to_utf8(
            *env,
            &region,
            "region must be representable as UTF-8",
        )?);
    }

    let permit = PENDING
        .try_acquire()
        .map_err(|_| host_error(*env, "StoreBusy", "native host admission is busy"))?;

    let cancellation = cancellation.sender.clone();
    let mut canceled = cancellation.subscribe();

    env.spawn_future_with_callback(
        async move {
            let _permit = permit;
            // Dropping/collecting the JS cancellation handle is not an abort.
            // Only an explicit signal change cancels a live acquisition.
            let _cancellation = cancellation;

            Ok(contain(async move {
                if *canceled.borrow() {
                    return None;
                }

                let mut change = pin!(canceled.changed());
                let mut work = pin!(mikrio::embedding::open(options));

                poll_fn(|context| {
                    if change.as_mut().poll(context).is_ready() {
                        return Poll::Ready(None);
                    }

                    work.as_mut().poll(context).map(Some)
                })
                .await
            })
            .await)
        },
        |env, result| {
            let opened = result
                .map_err(|_| host_error(*env, "IoError", "native operation panicked"))?
                .ok_or_else(|| host_error(*env, "OperationCanceled", "open was canceled"))?
                .map_err(|error| native_error(*env, &error))?;

            let (store, diagnostics) = opened;
            Ok(NativeStore { store, diagnostics })
        },
    )
}

fn try_to_utf8(env: Env, value: &[u16], message: &str) -> Result<String> {
    String::from_utf16(value).map_err(|_| host_error(env, "InvalidArgument", message))
}

fn host_error(env: Env, code: &str, message: &str) -> Error {
    let error = JsError::from(Error::new(code, message.to_owned()));
    Error::from_unknown_without_coercion(error.into_unknown(env))
}

fn native_error(env: Env, error: &mikrio::StorageError) -> Error {
    let mut converted = Error::new(error.code().as_str(), error.to_string());

    if let Some(source) = error.source() {
        converted.set_cause(local_cause(source, 16));
    }

    let value = JsError::from(converted).into_unknown(env);

    if let Some(progress) = error.partial_result() {
        let attached =
            deletion::NativeDeleteObjectsProgress::try_from(progress).and_then(|progress| {
                value
                    .coerce_to_object()?
                    .set_named_property("partialResult", progress)
            });

        if let Err(error) = attached {
            return error;
        }
    }

    let converted = Error::from_unknown_without_coercion(value);

    match error.mutation_outcome() {
        Some(outcome) => with_outcome(env, converted, outcome.as_str()),
        None => converted,
    }
}

fn with_outcome(env: Env, error: Error, outcome: &str) -> Error {
    let value = JsError::from(error).into_unknown(env);

    // This is our own error object; object conversion cannot invoke user input.
    let result = value
        .coerce_to_object()
        .and_then(|mut object| object.set_named_property("mutationOutcome", outcome));

    match result {
        Ok(()) => Error::from_unknown_without_coercion(value),
        Err(error) => error,
    }
}

// Only input-construction failures are known not to have dispatched a mutation.
fn mutation_input_error(env: Env, error: &mikrio::StorageError) -> Error {
    with_outcome(env, native_error(env, error), "NotCommitted")
}

fn dispatch<'env, F, O, T>(
    env: &'env Env,
    cancellation: &NativeCancellation,
    mutation: bool,
    future: F,
    project: fn(Env, O) -> Result<T>,
) -> Result<PromiseRaw<'env, T>>
where
    F: Future<Output = std::result::Result<O, mikrio::StorageError>> + Send + 'static,
    O: Send + 'static,
    T: ToNapiValue + 'static,
{
    let entry_error = |code, message| {
        let error = host_error(*env, code, message);

        if mutation {
            with_outcome(*env, error, "NotCommitted")
        } else {
            error
        }
    };

    if *cancellation.sender.borrow() {
        return Err(entry_error("OperationCanceled", "operation was canceled"));
    }

    let permit = PENDING
        .try_acquire()
        .map_err(|_| entry_error("StoreBusy", "native host admission is busy"))?;

    let cancellation = cancellation.sender.clone();
    let mut canceled = cancellation.subscribe();

    env.spawn_future_with_callback(
        async move {
            let _permit = permit;
            let _cancellation = cancellation;

            Ok(contain(async move {
                if *canceled.borrow() {
                    return None;
                }

                let mut change = pin!(canceled.changed());
                let mut work = pin!(future);

                poll_fn(|context| {
                    if change.as_mut().poll(context).is_ready() {
                        return Poll::Ready(None);
                    }

                    work.as_mut().poll(context).map(Some)
                })
                .await
            })
            .await)
        },
        move |env, result| {
            let uncertain = |code, message| {
                let error = host_error(*env, code, message);

                if mutation {
                    with_outcome(*env, error, "Unknown")
                } else {
                    error
                }
            };

            let value = result
                .map_err(|_| uncertain("IoError", "native operation panicked"))?
                .ok_or_else(|| uncertain("OperationCanceled", "operation was canceled"))?
                .map_err(|error| native_error(*env, &error))?;

            project(*env, value)
        },
    )
}

fn local_cause(error: &dyn std::error::Error, remaining: usize) -> Error {
    let mut converted = Error::new(Status::GenericFailure, error.to_string());

    if remaining > 0
        && let Some(source) = error.source()
    {
        converted.set_cause(local_cause(source, remaining - 1));
    }

    converted
}
