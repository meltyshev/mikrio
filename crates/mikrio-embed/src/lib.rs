//! Internal synchronous-host bridge. No interpreter objects enter this runtime.
//!
//! A weak process-local registry shares one bounded executor while hosts own it.
//! Last-owner shutdown happens on its driver thread, never inside a finalizer.

use std::fmt;
use std::future::Future;
use std::io;
use std::pin::Pin;
use std::sync::atomic::{AtomicU32, Ordering};
use std::sync::{Arc, Mutex, OnceLock, Weak, mpsc};
use std::task::{Context, Poll};
use std::time::Duration;

use tokio::runtime::Handle;
use tokio::sync::{Semaphore, oneshot};
use tokio::task::AbortHandle;

pub mod body;
pub mod chunks;
mod close;
pub mod upload;

pub use close::{CloseGate, CloseOutcome, CloseWait};

// Zero becomes the initializing process ID once and is never reset. Acquire
// checks observe that claim before any inherited mutex can be touched; the
// runtime registry itself remains protected by its mutex.
static PROCESS_ID: AtomicU32 = AtomicU32::new(0);
static RUNTIME: OnceLock<Mutex<Weak<HostRuntime>>> = OnceLock::new();
const PENDING_CALLS: usize = 32;

/// Internal host execution failure; adapters choose their language errors.
#[derive(Debug)]
pub enum HostError {
    /// Post-initialization fork inherited state that must not be used or closed.
    InheritedProcess,
    /// The bounded host call queue has no available capacity.
    Busy,
    /// Runtime/worker setup or result delivery failed.
    Unavailable(io::Error),
    /// An unwinding panic occurred while polling native work.
    Panicked,
}

impl fmt::Display for HostError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(match self {
            Self::InheritedProcess => "native state inherited after fork; use spawn or exec",
            Self::Busy => "native host admission is busy",
            Self::Unavailable(_) => "native host runtime is unavailable",
            Self::Panicked => "native operation panicked",
        })
    }
}

impl std::error::Error for HostError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::Unavailable(error) => Some(error),
            _ => None,
        }
    }
}

/// Refuse inherited state before taking any process-local mutex.
///
/// # Errors
/// Returns `InheritedProcess` after post-initialization fork without exec.
pub fn check_process() -> Result<(), HostError> {
    let owner = PROCESS_ID.load(Ordering::Acquire);

    if owner != 0 && owner != std::process::id() {
        return Err(HostError::InheritedProcess);
    }

    Ok(())
}

/// Own a native value that must never run normal destructors in a forked child.
pub struct ProcessBound<T> {
    process_id: u32,
    value: Option<T>,
}

impl<T> ProcessBound<T> {
    /// Capture the owning process before this value crosses a host boundary.
    pub fn new(value: T) -> Self {
        Self {
            process_id: std::process::id(),
            value: Some(value),
        }
    }

    /// Borrow only in the process that created the value.
    ///
    /// # Errors
    /// Refuses inherited state without locking or destroying the value.
    pub fn get(&self) -> Result<&T, HostError> {
        if self.process_id != std::process::id() {
            return Err(HostError::InheritedProcess);
        }

        self.value
            .as_ref()
            .ok_or_else(|| HostError::Unavailable(io::Error::other("native owner missing")))
    }

    /// Mutably borrow only in the process that created the value.
    ///
    /// # Errors
    /// Refuses inherited state without locking or destroying the value.
    pub fn get_mut(&mut self) -> Result<&mut T, HostError> {
        if self.process_id != std::process::id() {
            return Err(HostError::InheritedProcess);
        }

        self.value
            .as_mut()
            .ok_or_else(|| HostError::Unavailable(io::Error::other("native owner missing")))
    }
}

impl<T> Drop for ProcessBound<T> {
    fn drop(&mut self) {
        if self.process_id != std::process::id() {
            // Child copies have no live owner threads. Normal SQLite/runtime/
            // mutex teardown is forbidden; the child OS reclaims them at exit.
            #[expect(
                clippy::mem_forget,
                reason = "inherited native destructors are unsafe after fork"
            )]
            std::mem::forget(self.value.take());
        }
    }
}

/// Shared bounded runtime for synchronous foreign hosts. Node uses its framework
/// runtime separately. Creating/importing host classes does not call `shared`.
pub struct HostRuntime {
    handle: Handle,
    admission: Arc<Semaphore>,
    stop: Option<oneshot::Sender<()>>,
}

impl HostRuntime {
    /// Obtain the shared runtime, creating it only for actual native work.
    ///
    /// # Errors
    /// Refuses inherited process state and reports runtime/thread setup failures
    /// or a poisoned runtime registry.
    pub fn shared() -> Result<Arc<Self>, HostError> {
        let process_id = std::process::id();
        let owner = PROCESS_ID.compare_exchange(0, process_id, Ordering::AcqRel, Ordering::Acquire);

        if owner.is_err_and(|owner| owner != process_id) {
            return Err(HostError::InheritedProcess);
        }

        let mut shared = RUNTIME
            .get_or_init(Mutex::default)
            .lock()
            .map_err(|_| HostError::Unavailable(io::Error::other("poisoned runtime registry")))?;

        if let Some(runtime) = shared.upgrade() {
            return Ok(runtime);
        }

        let runtime = Self::start(PENDING_CALLS)?;
        *shared = Arc::downgrade(&runtime);
        Ok(runtime)
    }

    /// Submit owned native work without blocking a foreign interpreter.
    /// Dropping the returned waiter cancels this caller future. Native work
    /// already submitted to core retains its own resources until settlement.
    ///
    /// # Errors
    /// Refuses inherited state or exhausted bounded call admission.
    pub fn submit<F, T>(self: &Arc<Self>, future: F) -> Result<Pending<T>, HostError>
    where
        F: Future<Output = T> + Send + 'static,
        T: Send + 'static,
    {
        check_process()?;

        let permit = Arc::clone(&self.admission)
            .try_acquire_owned()
            .map_err(|_| HostError::Busy)?;

        let owner = Arc::clone(self);
        let (reply, response) = mpsc::sync_channel(1);

        let task = self.handle.spawn(async move {
            let _owner = owner;
            let result = Contained(Box::pin(future)).await;

            // A delivered result may immediately start another host call. Its
            // completed predecessor must no longer occupy admission capacity.
            drop(permit);
            let _delivery = reply.send(result);
        });

        let abort = task.abort_handle();
        drop(task);

        Ok(Pending(ProcessBound::new(PendingInner { response, abort })))
    }

    /// Drive a bounded continuation of an already owned upload/body on a
    /// synchronous host thread. No new operation, task or admission slot is
    /// created. The caller retains the pinned future across timed-out waits.
    ///
    /// # Errors
    /// Refuses inherited state or a nested executor wait; contains native poll
    /// panics. `None` means the interval elapsed, not cancellation or EOF.
    pub fn wait_transfer<F>(
        &self,
        future: Pin<&mut F>,
        interval: Duration,
    ) -> Result<Option<F::Output>, HostError>
    where
        F: Future + Send,
        F::Output: Send,
    {
        check_process()?;

        if Handle::try_current().is_ok() {
            return Err(HostError::Unavailable(io::Error::other(
                "host transfer waits require a synchronous thread",
            )));
        }

        self.handle.block_on(async {
            match tokio::time::timeout(interval, contain(future)).await {
                Ok(result) => result.map(Some),
                Err(_) => Ok(None),
            }
        })
    }

    fn start(capacity: usize) -> Result<Arc<Self>, HostError> {
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .max_blocking_threads(4)
            .build()
            .map_err(HostError::Unavailable)?;

        let handle = runtime.handle().clone();
        let (stop, stopped) = oneshot::channel();

        let driver = std::thread::Builder::new()
            .name("mikrio-host".into())
            .spawn(move || {
                let _stopped = runtime.block_on(stopped);

                // Runtime destruction waits for actual blocking work on this thread,
                // never while Python owns its interpreter or finalizes a wrapper.
                drop(runtime);
            })
            .map_err(HostError::Unavailable)?;

        drop(driver);

        Ok(Arc::new(Self {
            handle,
            admission: Arc::new(Semaphore::new(capacity)),
            stop: Some(stop),
        }))
    }
}

impl Drop for HostRuntime {
    fn drop(&mut self) {
        if let Some(stop) = self.stop.take() {
            let _delivery = stop.send(());
        }
    }
}

/// Interruptible synchronous wait. Dropping it cancels the Rust caller future;
/// core continues to own any already submitted native work until settlement.
pub struct Pending<T>(ProcessBound<PendingInner<T>>);

struct PendingInner<T> {
    response: mpsc::Receiver<Result<T, HostError>>,
    abort: AbortHandle,
}

impl<T> Pending<T> {
    /// Wait for one bounded interval, allowing the adapter to check host signals.
    /// `None` means this wait interval elapsed, never that native work stopped.
    ///
    /// # Errors
    /// Reports inherited state, native panic or a disconnected result owner.
    pub fn wait(&mut self, interval: Duration) -> Result<Option<T>, HostError> {
        match self.0.get_mut()?.response.recv_timeout(interval) {
            Ok(result) => result.map(Some),
            Err(mpsc::RecvTimeoutError::Timeout) => Ok(None),
            Err(mpsc::RecvTimeoutError::Disconnected) => Err(HostError::Unavailable(
                io::Error::other("native result unavailable"),
            )),
        }
    }
}

impl<T> Drop for PendingInner<T> {
    fn drop(&mut self) {
        self.abort.abort();
    }
}

struct Contained<F>(Pin<Box<F>>);

/// Contain unwinding panics during every poll on a foreign framework's executor.
/// This does not create a runtime or expose the native panic payload to a host.
///
/// # Errors
/// Returns `Panicked` when a poll unwinds. Actual native work retains its core
/// ownership independently of this caller future.
pub async fn contain<F: Future>(future: F) -> Result<F::Output, HostError> {
    Contained(Box::pin(future)).await
}

impl<F: Future> Future for Contained<F> {
    type Output = Result<F::Output, HostError>;

    fn poll(mut self: Pin<&mut Self>, context: &mut Context<'_>) -> Poll<Self::Output> {
        match std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            self.0.as_mut().poll(context)
        })) {
            Ok(Poll::Pending) => Poll::Pending,
            Ok(Poll::Ready(value)) => Poll::Ready(Ok(value)),
            Err(_) => Poll::Ready(Err(HostError::Panicked)),
        }
    }
}

#[cfg(test)]
mod tests;
