//! One shutdown task per synchronous-host connection, independent of admission.

use std::future::Future;
use std::io;
use std::sync::{Arc, Condvar, Mutex};
use std::time::Duration;

use crate::{HostError, HostRuntime, check_process, contain};

/// Detached native shutdown result, shared without copying its error chain.
pub type CloseOutcome<E = mikrio::StorageError> = Result<Result<(), E>, HostError>;

/// Synchronous-host ownership of one close attempt. Core still owns the actual
/// connection state machine. Completed failure permits a new attempt; success
/// remains available to repeated callers. No ordinary-call permit is needed.
pub struct CloseGate<E = mikrio::StorageError> {
    attempt: Mutex<Option<Arc<Attempt<E>>>>,
}

struct Attempt<E> {
    result: Mutex<Option<Arc<CloseOutcome<E>>>>,
    completed: Condvar,
}

impl<E> Default for CloseGate<E> {
    fn default() -> Self {
        Self {
            attempt: Mutex::new(None),
        }
    }
}

impl<E> Default for Attempt<E> {
    fn default() -> Self {
        Self {
            result: Mutex::new(None),
            completed: Condvar::new(),
        }
    }
}

impl<E: Send + Sync + 'static> CloseGate<E> {
    /// Join shutdown or submit its single owned future on the existing runtime.
    /// The supplied future is dropped without polling when joining an attempt.
    ///
    /// # Errors
    /// Refuses inherited state and reports poisoned host coordination state.
    pub fn begin<F>(&self, runtime: &Arc<HostRuntime>, future: F) -> Result<CloseWait<E>, HostError>
    where
        F: Future<Output = Result<(), E>> + Send + 'static,
    {
        check_process()?;

        let mut current = self.attempt.lock().map_err(|_| poisoned())?;

        if let Some(attempt) = current.as_ref() {
            let result = attempt.result.lock().map_err(|_| poisoned())?;

            let can_retry = result
                .as_ref()
                .is_some_and(|outcome| !matches!(outcome.as_ref(), Ok(Ok(()))));

            if !can_retry {
                return Ok(CloseWait(Arc::clone(attempt)));
            }
        }

        let attempt = Arc::new(Attempt::default());
        let completion = Arc::clone(&attempt);
        let owner = Arc::clone(runtime);

        let task = runtime.handle.spawn(async move {
            let _owner = owner;
            completion.finish(contain(future).await);
        });

        // The attempt and native task own settlement. Dropping a host waiter
        // must not abort shutdown or destroy a runtime on its finalizer thread.
        drop(task);
        *current = Some(Arc::clone(&attempt));
        Ok(CloseWait(attempt))
    }
}

impl<E> Attempt<E> {
    fn finish(&self, outcome: CloseOutcome<E>) {
        match self.result.lock() {
            Ok(mut result) => *result = Some(Arc::new(outcome)),
            Err(poison) => {
                // Do not clear poison or claim native success. Waiters also
                // observe this coordination failure when taking the mutex.
                *poison.into_inner() = Some(Arc::new(Err(poisoned())));
            }
        }

        self.completed.notify_all();
    }
}

/// An interruptible wait on shared shutdown. Drop releases only this waiter.
pub struct CloseWait<E = mikrio::StorageError>(Arc<Attempt<E>>);

impl<E> CloseWait<E> {
    /// Wait outside the interpreter for one interval, then let it check signals.
    /// `None` means the interval elapsed. A completed outcome remains available
    /// to this waiter even if a later caller retries a failed attempt.
    ///
    /// # Errors
    /// Refuses inherited process state or poisoned host coordination state.
    pub fn wait(&self, interval: Duration) -> Result<Option<Arc<CloseOutcome<E>>>, HostError> {
        check_process()?;

        let result = self.0.result.lock().map_err(|_| poisoned())?;

        let (result, _) = self
            .0
            .completed
            .wait_timeout_while(result, interval, |result| result.is_none())
            .map_err(|_| poisoned())?;

        Ok(result.as_ref().map(Arc::clone))
    }
}

fn poisoned() -> HostError {
    HostError::Unavailable(io::Error::other("poisoned host shutdown coordination"))
}

#[cfg(test)]
mod tests {
    use tokio::sync::oneshot;

    use super::*;

    #[test]
    #[expect(clippy::panic, reason = "duplicate close futures must never be polled")]
    fn saturated_ordinary_admission_cannot_block_shared_shutdown() -> Result<(), HostError> {
        let runtime = HostRuntime::start(1)?;
        let ordinary = runtime.submit(std::future::pending::<()>())?;
        assert!(matches!(runtime.submit(async {}), Err(HostError::Busy)));

        let close = CloseGate::<mikrio::StorageError>::default();
        let (finish, finished) = oneshot::channel();

        let first = close.begin(&runtime, async move {
            finished.await.unwrap();
            Ok(())
        })?;

        let second = close.begin(&runtime, async { panic!("duplicate shutdown submission") })?;
        assert!(first.wait(Duration::from_millis(1))?.is_none());

        drop(first);
        finish.send(()).unwrap();
        let outcome = second.wait(Duration::from_secs(5))?.unwrap();
        assert!(matches!(outcome.as_ref(), Ok(Ok(()))));

        let repeated = close.begin(&runtime, async {
            panic!("shutdown repeated after success")
        })?;

        assert!(matches!(
            repeated.wait(Duration::ZERO)?.unwrap().as_ref(),
            Ok(Ok(()))
        ));

        drop(ordinary);
        Ok(())
    }

    #[test]
    #[expect(clippy::panic, reason = "exercise failed close polling and retry")]
    fn failed_poll_can_retry_without_replacing_an_earlier_waiters_error() -> Result<(), HostError> {
        let runtime = HostRuntime::start(1)?;
        let close = CloseGate::<mikrio::StorageError>::default();

        let failed = close.begin(&runtime, async {
            tokio::task::yield_now().await;
            panic!("controlled shutdown panic");
        })?;

        let original = failed.wait(Duration::from_secs(5))?.unwrap();
        assert!(matches!(original.as_ref(), Err(HostError::Panicked)));

        let retried = close.begin(&runtime, async { Ok(()) })?;

        assert!(matches!(
            retried.wait(Duration::from_secs(5))?.unwrap().as_ref(),
            Ok(Ok(()))
        ));

        assert!(matches!(
            failed.wait(Duration::ZERO)?.unwrap().as_ref(),
            Err(HostError::Panicked)
        ));

        Ok(())
    }
}
