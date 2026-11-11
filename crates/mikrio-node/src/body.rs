//! JavaScript body methods over the shared host cursor owner.

use std::future::{Future, poll_fn};
use std::pin::pin;
use std::task::Poll;

use mikrio_embed::body::{Body, BodyError};
use mikrio_embed::contain;
use napi::bindgen_prelude::{Buffer, PromiseRaw, ToNapiValue};
use napi::{Env, Result};
use napi_derive::napi;
use tokio::sync::Semaphore;

use crate::{NativeCancellation, host_error, native_error};

// Read work is bounded independently of ordinary calls that may wait on guards
// retained by an existing body. Close continues to bypass both budgets.
static READS: Semaphore = Semaphore::const_new(32);

#[napi]
pub struct NativeBody {
    pub(super) body: Body,
}

impl Drop for NativeBody {
    fn drop(&mut self) {
        self.body.cancel();
    }
}

#[napi]
impl NativeBody {
    /// Read a bounded chunk from this cursor.
    ///
    /// # Errors
    /// Rejects invalid limits, concurrent reads, cancellation and native failures.
    #[napi(catch_unwind)]
    pub fn read_chunk<'env>(
        &self,
        env: &'env Env,
        max_bytes: f64,
        cancellation: &NativeCancellation,
    ) -> Result<PromiseRaw<'env, Option<Buffer>>> {
        let limit = try_to_max_bytes(*env, max_bytes)?;

        if limit == 0 {
            return Err(host_error(
                *env,
                "InvalidArgument",
                "chunk size must be positive",
            ));
        }

        let permit = try_acquire_permit(*env)?;

        let mut reading = self
            .body
            .start_read()
            .map_err(|error| failure(*env, &error))?;

        run(env, cancellation, permit, async move {
            let chunk = reading.next(limit).await?;
            reading.finish();

            Ok(chunk.map(Buffer::from))
        })
    }

    /// Collect within a caller-supplied bound; transfer the final Vec to Node.
    ///
    /// # Errors
    /// Reports collection limit, cursor, cancellation, allocation or native errors.
    #[napi(catch_unwind)]
    pub fn read_all<'env>(
        &self,
        env: &'env Env,
        max_bytes: f64,
        cancellation: &NativeCancellation,
    ) -> Result<PromiseRaw<'env, Buffer>> {
        let limit = try_to_max_bytes(*env, max_bytes)?;
        let permit = try_acquire_permit(*env)?;

        let (mut reading, length) = self
            .body
            .collection(limit)
            .map_err(|error| failure(*env, &error))?;

        run(env, cancellation, permit, async move {
            let mut bytes = Vec::new();

            bytes
                .try_reserve_exact(length)
                .map_err(|_| BodyError::Unavailable)?;

            while let Some(chunk) = reading.next(mikrio_embed::chunks::CHUNK_BYTES).await? {
                if chunk.len() > length.saturating_sub(bytes.len()) {
                    return Err(BodyError::Unavailable);
                }

                bytes.extend_from_slice(&chunk);
            }

            if bytes.len() != length {
                return Err(BodyError::Unavailable);
            }

            Ok(Buffer::from(bytes))
        })
    }

    /// Revoke pending reads immediately without waiting for host callbacks.
    #[napi(catch_unwind)]
    pub fn cancel(&self) {
        self.body.cancel();
    }

    /// Join native cleanup independently of ordinary-call admission.
    ///
    /// # Errors
    /// Reports actual cleanup or executor failure; a later close can retry.
    #[napi(catch_unwind)]
    pub fn close<'env>(&self, env: &'env Env) -> Result<PromiseRaw<'env, ()>> {
        self.body.cancel();
        let body = self.body.clone();

        env.spawn_future_with_callback(
            async move { Ok(contain(body.close()).await) },
            |env, result| {
                result
                    .map_err(|_| host_error(*env, "IoError", "native body cleanup panicked"))?
                    .map_err(|error| failure(*env, &error))
            },
        )
    }
}

fn run<'env, T, F>(
    env: &'env Env,
    cancellation: &NativeCancellation,
    permit: tokio::sync::SemaphorePermit<'static>,
    future: F,
) -> Result<PromiseRaw<'env, T>>
where
    F: Future<Output = std::result::Result<T, BodyError>> + Send + 'static,
    T: ToNapiValue + Send + 'static,
{
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

                let mut changed = pin!(canceled.changed());
                let mut work = pin!(future);

                poll_fn(|context| {
                    if changed.as_mut().poll(context).is_ready() {
                        return Poll::Ready(None);
                    }

                    work.as_mut().poll(context).map(Some)
                })
                .await
            })
            .await)
        },
        |env, result| {
            result
                .map_err(|_| host_error(*env, "IoError", "native body poll failed"))?
                .ok_or_else(|| host_error(*env, "OperationCanceled", "body read was canceled"))?
                .map_err(|error| failure(*env, &error))
        },
    )
}

fn try_acquire_permit(env: Env) -> Result<tokio::sync::SemaphorePermit<'static>> {
    READS
        .try_acquire()
        .map_err(|_| host_error(env, "StoreBusy", "native body-read admission is busy"))
}

fn try_to_max_bytes(env: Env, value: f64) -> Result<usize> {
    if !value.is_finite()
        || value.fract() != 0.0
        || !(0.0..=9_007_199_254_740_991.0).contains(&value)
    {
        return Err(host_error(
            env,
            "InvalidArgument",
            "byte bound must be a nonnegative safe integer",
        ));
    }

    #[expect(
        clippy::as_conversions,
        clippy::cast_possible_truncation,
        clippy::cast_sign_loss,
        reason = "finite integral value is checked inside the exact nonnegative JS integer range"
    )]
    let count = value as u64;

    usize::try_from(count).map_err(|_| {
        host_error(
            env,
            "InvalidArgument",
            "byte bound is outside the host range",
        )
    })
}

fn failure(env: Env, error: &BodyError) -> napi::Error {
    if let BodyError::Native(error) = error {
        return native_error(env, error);
    }

    host_error(env, error.code(), &error.to_string())
}
