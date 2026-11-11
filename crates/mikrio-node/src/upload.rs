//! Native demand handles. JavaScript retains producers and lends only one
//! bounded byte slice to a synchronous copy after native capacity is available.

use std::sync::{Arc, Mutex};

use mikrio::BodySource;
use mikrio_embed::contain;
use mikrio_embed::upload::{Request, Upload};
use napi::bindgen_prelude::{PromiseRaw, Uint8Array};
use napi::{Env, Result};
use napi_derive::napi;
use tokio::sync::Semaphore;

use crate::host_error;

// A foreign producer that never returns keeps this admission, even if its
// native operation has canceled. Repeated abandoned callbacks stay bounded.
static PRODUCERS: Semaphore = Semaphore::const_new(32);

#[napi]
pub struct NativeUpload {
    upload: Arc<tokio::sync::Mutex<Upload>>,
    source: Mutex<Option<BodySource>>,
    permit: Mutex<Option<Arc<tokio::sync::SemaphorePermit<'static>>>>,
}

#[napi]
impl NativeUpload {
    /// Reserve a host transfer before invoking any producer.
    ///
    /// # Errors
    /// Refuses excess pending producers without retaining their buffers.
    #[napi(constructor, catch_unwind)]
    pub fn new(env: Env) -> Result<Self> {
        let permit = PRODUCERS
            .try_acquire()
            .map_err(|_| host_error(env, "StoreBusy", "native producer admission is busy"))?;

        let (upload, source) = Upload::channel();

        Ok(Self {
            upload: Arc::new(tokio::sync::Mutex::new(upload)),
            source: Mutex::new(Some(source)),
            permit: Mutex::new(Some(Arc::new(permit))),
        })
    }

    /// Wait for a native pull without borrowing JavaScript memory.
    ///
    /// # Errors
    /// Refuses overlapping pulls or unavailable framework dispatch.
    #[napi(catch_unwind)]
    pub fn next<'env>(&self, env: &'env Env) -> Result<PromiseRaw<'env, Option<NativeRequest>>> {
        let mut upload = Arc::clone(&self.upload)
            .try_lock_owned()
            .map_err(|_| host_error(*env, "BodyInUse", "upload pull is already pending"))?;

        // One poll per admitted producer is bounded by its cursor and original
        // permit. Retain that permit through actual polling, including host
        // teardown; draining a PUT cannot require another ordinary-call slot.
        let permit = self
            .permit
            .lock()
            .map_err(|_| host_error(*env, "IoError", "producer admission state is poisoned"))?
            .as_ref()
            .cloned()
            .ok_or_else(|| host_error(*env, "BodyClosed", "producer admission is released"))?;

        env.spawn_future_with_callback(
            async move {
                let _permit = permit;

                Ok(contain(async move { upload.next().await }).await)
            },
            |env, result| {
                Ok(result
                    .map_err(|_| host_error(*env, "IoError", "native upload poll failed"))?
                    .map(|request| NativeRequest(Mutex::new(Some(request)))))
            },
        )
    }

    /// Release host admission after the producer pump has actually settled.
    /// Pending native polls retain their own shared permit through completion.
    ///
    /// # Errors
    /// Reports a poisoned admission state without relying on JavaScript GC.
    #[napi(catch_unwind)]
    pub fn release(&self, env: Env) -> Result<()> {
        let permit = self
            .permit
            .lock()
            .map_err(|_| host_error(env, "IoError", "producer admission state is poisoned"))?
            .take();

        drop(permit);

        Ok(())
    }
}

impl NativeUpload {
    pub(super) fn take_source(&self, env: Env) -> Result<BodySource> {
        self.source
            .lock()
            .map_err(|_| host_error(env, "IoError", "upload coordination failed"))?
            .take()
            .ok_or_else(|| host_error(env, "BodyClosed", "upload already consumed"))
    }
}

#[napi]
pub struct NativeRequest(Mutex<Option<Request>>);

#[napi]
impl NativeRequest {
    /// Copy one bounded host slice; no foreign reference outlives this call.
    ///
    /// # Errors
    /// Refuses oversized data or a callback that returned after native shutdown.
    #[napi(catch_unwind)]
    #[allow(
        clippy::needless_pass_by_value,
        reason = "NAPI lends the typed array only for this synchronous copy"
    )]
    pub fn write(&self, env: Env, bytes: Uint8Array) -> Result<()> {
        self.take(env)?
            .write(bytes.as_ref())
            .map_err(|_| host_error(env, "BodyClosed", "upload request is closed or oversized"))
    }

    /// Signal actual source completion, distinct from an empty chunk.
    ///
    /// # Errors
    /// Refuses a consumed request or failed host coordination.
    #[napi(catch_unwind)]
    pub fn finish(&self, env: Env) -> Result<()> {
        self.take(env)?.finish();

        Ok(())
    }

    /// Deliver a producer failure; its original JS cause stays with the wrapper.
    ///
    /// # Errors
    /// Refuses a consumed request or failed host coordination.
    #[napi(catch_unwind)]
    pub fn fail(&self, env: Env) -> Result<()> {
        self.take(env)?.fail();

        Ok(())
    }
}

impl NativeRequest {
    fn take(&self, env: Env) -> Result<Request> {
        self.0
            .lock()
            .map_err(|_| host_error(env, "IoError", "upload coordination failed"))?
            .take()
            .ok_or_else(|| host_error(env, "BodyClosed", "upload request already consumed"))
    }
}
