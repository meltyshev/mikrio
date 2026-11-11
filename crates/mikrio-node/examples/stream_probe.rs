//! Explicit private observation addon. Never included in the shipping tarball.

use std::sync::{Arc, Mutex};

use mikrio_embed::contain;
use napi::bindgen_prelude::{BigInt, Buffer, PromiseRaw, Uint8Array};
use napi::{Env, Error, JsError, Result, Status};
use napi_derive::napi;
use tokio::sync::Semaphore;

#[path = "../../mikrio-embed/tests/support/streams.rs"]
mod fixture;

static PENDING: Semaphore = Semaphore::const_new(32);

#[napi]
pub struct StreamProbe {
    stream: Arc<fixture::Stream>,
}

impl Drop for StreamProbe {
    fn drop(&mut self) {
        // Environment/wrapper teardown revokes callbacks without waiting for
        // arbitrary JS. Native futures retain their own ownership until woken.
        let _failure = self.stream.fail(fixture::Failure::Closed);
    }
}

#[napi(object, object_from_js = false)]
pub struct Allocations {
    pub retained_bytes: u32,
    pub peak_bytes: u32,
}

#[napi]
impl StreamProbe {
    /// Construct an observation transport with an optional exact byte count.
    ///
    /// # Errors
    /// Rejects negative or overflowing expected lengths.
    #[napi(constructor, catch_unwind)]
    #[allow(
        clippy::needless_pass_by_value,
        reason = "NAPI owns the decoded BigInt"
    )]
    pub fn new(env: Env, expected: Option<BigInt>) -> Result<Self> {
        let expected = expected
            .map(|value| {
                let (negative, length, lossless) = value.get_u64();

                if negative || !lossless {
                    return Err(failure(env, fixture::Failure::InvalidArgument));
                }

                Ok(length)
            })
            .transpose()?;

        Ok(Self {
            stream: fixture::Stream::new(expected),
        })
    }

    /// Wait for native byte capacity; no caller buffer is retained while waiting.
    ///
    /// # Errors
    /// Reports closed transfer, exhausted call admission or framework dispatch.
    #[napi(catch_unwind)]
    pub fn reserve<'env>(&self, env: &'env Env) -> Result<PromiseRaw<'env, Reservation>> {
        let permit = try_acquire_permit()?;
        let stream = Arc::clone(&self.stream);

        env.spawn_future_with_callback(
            async move {
                let _permit = permit;

                Ok(contain(async move { stream.reserve().await }).await)
            },
            |env, result| {
                let reservation = result
                    .map_err(|_| probe_error())?
                    .map_err(|error| failure(*env, error))?;

                Ok(Reservation(Mutex::new(Some(reservation))))
            },
        )
    }

    /// Consume at most one bounded native chunk.
    ///
    /// # Errors
    /// Reports invalid limits, overlapping/closed reads, producer or dispatch failure.
    #[napi(catch_unwind)]
    pub fn read_chunk<'env>(
        &self,
        env: &'env Env,
        max_bytes: f64,
    ) -> Result<PromiseRaw<'env, Option<Buffer>>> {
        let permit = try_acquire_permit()?;
        let stream = Arc::clone(&self.stream);
        let max_bytes = try_to_max_bytes(*env, max_bytes)?;

        env.spawn_future_with_callback(
            async move {
                let _permit = permit;

                Ok(contain(async move { stream.read(max_bytes).await }).await)
            },
            |env, result| {
                result
                    .map_err(|_| probe_error())?
                    .map(|chunk| chunk.map(Buffer::from))
                    .map_err(|error| failure(*env, error))
            },
        )
    }

    /// Collect the remaining probe bytes within the caller's allocation limit.
    ///
    /// # Errors
    /// Reports invalid limits, excess length, terminal state or dispatch failure.
    #[napi(catch_unwind)]
    pub fn read_all<'env>(
        &self,
        env: &'env Env,
        max_bytes: f64,
    ) -> Result<PromiseRaw<'env, Buffer>> {
        let permit = try_acquire_permit()?;
        let stream = Arc::clone(&self.stream);
        let max_bytes = try_to_max_bytes(*env, max_bytes)?;

        env.spawn_future_with_callback(
            async move {
                let _permit = permit;

                Ok(contain(async move { stream.read_all(max_bytes).await }).await)
            },
            |env, result| {
                result
                    .map_err(|_| probe_error())?
                    .map(Buffer::from)
                    .map_err(|error| failure(*env, error))
            },
        )
    }

    /// Mark validated producer termination without publishing any object.
    ///
    /// # Errors
    /// Reports terminal state or mismatched declared length.
    #[napi(catch_unwind)]
    pub fn end(&self, env: Env) -> Result<()> {
        self.stream.end().map_err(|error| failure(env, error))
    }

    /// Deliver a controlled producer failure to the native transport.
    ///
    /// # Errors
    /// Reports poisoned fixture coordination.
    #[napi(catch_unwind)]
    pub fn fail_producer(&self, env: Env) -> Result<()> {
        self.stream
            .fail(fixture::Failure::Producer)
            .map_err(|error| failure(env, error))
    }

    /// Revoke input and await pending native read settlement.
    ///
    /// # Errors
    /// Reports coordination or framework dispatch failure.
    #[napi(catch_unwind)]
    pub fn close<'env>(&self, env: &'env Env) -> Result<PromiseRaw<'env, ()>> {
        let stream = Arc::clone(&self.stream);

        env.spawn_future_with_callback(
            async move { Ok(contain(async move { stream.close().await }).await) },
            |env, result| {
                result
                    .map_err(|_| probe_error())?
                    .map_err(|error| failure(*env, error))
            },
        )
    }

    /// Inspect tracked native payload allocations; excludes caller buffers.
    ///
    /// # Errors
    /// Reports an allocation count outside the fixed fixture bound.
    #[napi(getter, catch_unwind)]
    pub fn allocations(&self) -> Result<Allocations> {
        let (retained, peak) = self.stream.allocations();

        Ok(Allocations {
            retained_bytes: u32::try_from(retained).map_err(|_| probe_error())?,
            peak_bytes: u32::try_from(peak).map_err(|_| probe_error())?,
        })
    }

    #[napi(getter, catch_unwind)]
    #[must_use]
    pub fn is_reading(&self) -> bool {
        self.stream.is_reading()
    }
}

#[napi]
pub struct Reservation(Mutex<Option<fixture::Reservation>>);

#[napi]
impl Reservation {
    /// Consume a reservation by synchronously copying a bounded input slice.
    ///
    /// # Errors
    /// Reports already consumed/closed reservation, excess slice or length mismatch.
    #[napi(catch_unwind)]
    #[allow(
        clippy::needless_pass_by_value,
        reason = "NAPI lends the typed array only during this synchronous copy"
    )]
    pub fn write(&self, env: Env, bytes: Uint8Array) -> Result<()> {
        let reservation = self
            .0
            .lock()
            .map_err(|_| probe_error())?
            .take()
            .ok_or_else(|| failure(env, fixture::Failure::Closed))?;

        // No foreign reference survives this call or crosses onto Tokio.
        reservation
            .write(bytes.as_ref())
            .map_err(|error| failure(env, error))
    }
}

fn try_acquire_permit() -> Result<tokio::sync::SemaphorePermit<'static>> {
    PENDING
        .try_acquire()
        .map_err(|_| Error::new(Status::QueueFull, "probe admission is full"))
}

/// Inspect fixture work still owned by native futures across environments.
///
/// # Errors
/// Reports a counter outside the fixed probe admission bound.
#[napi(catch_unwind)]
pub fn pending_calls() -> Result<u32> {
    u32::try_from(32 - PENDING.available_permits()).map_err(|_| probe_error())
}

fn try_to_max_bytes(env: Env, value: f64) -> Result<usize> {
    if !value.is_finite() || value.fract() != 0.0 || value < 0.0 || value > f64::from(u32::MAX) {
        return Err(failure(env, fixture::Failure::InvalidArgument));
    }

    #[expect(
        clippy::as_conversions,
        clippy::cast_possible_truncation,
        clippy::cast_sign_loss,
        reason = "finite integral value validated inside the u32 range above"
    )]
    let checked = value as u32;

    usize::try_from(checked).map_err(|_| failure(env, fixture::Failure::InvalidArgument))
}

fn probe_error() -> Error {
    Error::new(Status::GenericFailure, "native probe failure")
}

fn failure(env: Env, error: fixture::Failure) -> Error {
    Error::from_unknown_without_coercion(
        JsError::from(Error::new(error.code(), error.to_string())).into_unknown(env),
    )
}
