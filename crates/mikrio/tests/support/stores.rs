//! Join store cleanup before the test runtime shuts down, including on failure.
//!
//! This owner retains connection aliases. Tests of the last Store handle's drop
//! must own their handles directly instead. Listener handles are never retained.

use std::cell::RefCell;
use std::panic::{AssertUnwindSafe, catch_unwind, resume_unwind};

use mikrio::{OpenOptions, StorageError, Store};
use tokio::runtime::Runtime;

type TestResult<T> = Result<T, Box<dyn std::error::Error>>;

#[derive(Default)]
pub(crate) struct Stores {
    connections: RefCell<Vec<Store>>,
}

impl Stores {
    pub(crate) async fn open(&self, options: OpenOptions) -> Result<Store, StorageError> {
        let store = Store::open(options).await?;
        self.connections.borrow_mut().push(store.clone());
        Ok(store)
    }

    async fn close(self) -> Vec<StorageError> {
        let mut failures = Vec::new();

        for store in self.connections.into_inner().into_iter().rev() {
            if let Err(error) = store.close().await {
                failures.push(error);
            }
        }

        failures
    }
}

pub(crate) fn run<T>(
    runtime: &Runtime,
    scenario: impl AsyncFnOnce(&Stores) -> TestResult<T>,
) -> TestResult<T> {
    run_sync(runtime, |stores| runtime.block_on(scenario(stores)))
}

pub(crate) fn run_sync<T>(
    runtime: &Runtime,
    scenario: impl FnOnce(&Stores) -> TestResult<T>,
) -> TestResult<T> {
    let stores = Stores::default();
    let outcome = catch_unwind(AssertUnwindSafe(|| scenario(&stores)));
    let cleanup_failures = runtime.block_on(stores.close());

    if !cleanup_failures.is_empty() {
        report_cleanup(&cleanup_failures);
    }

    match outcome {
        Err(panic) => resume_unwind(panic),
        Ok(Err(error)) => Err(error),
        Ok(Ok(value)) if cleanup_failures.is_empty() => Ok(value),
        Ok(Ok(_)) => Err(format!("store cleanup failed: {cleanup_failures:?}").into()),
    }
}

#[allow(
    clippy::print_stderr,
    reason = "test cleanup failures must remain visible beside the original error or panic"
)]
fn report_cleanup(failures: &[StorageError]) {
    for error in failures {
        eprintln!("test store cleanup failed: {error}");
    }
}
