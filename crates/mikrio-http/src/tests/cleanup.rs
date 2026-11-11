//! Explicit test scopes settle connections before releasing runtime and directory.

use std::cell::RefCell;
use std::panic::{AssertUnwindSafe, catch_unwind, resume_unwind};
use std::sync::Arc;

use mikrio_core::Connection;
use tokio::runtime::Runtime;

use super::{TestDirectory, TestResult};

pub(crate) struct Scope {
    pub(crate) runtime: Arc<Runtime>,
    pub(crate) directory: TestDirectory,
    connections: RefCell<Vec<Connection>>,
}

impl Scope {
    pub(crate) fn run<T>(
        directory: TestDirectory,
        scenario: impl FnOnce(&Self) -> Result<T, Box<dyn std::error::Error>>,
    ) -> Result<T, Box<dyn std::error::Error>> {
        let scope = Self {
            runtime: Arc::new(
                tokio::runtime::Builder::new_current_thread()
                    .enable_all()
                    .build()?,
            ),
            directory,
            connections: RefCell::new(Vec::new()),
        };

        // Catch only at the scenario boundary. Its request futures and borrowed
        // fixtures have unwound before explicit native shutdown starts.
        let outcome = catch_unwind(AssertUnwindSafe(|| scenario(&scope)));
        let cleanup = catch_unwind(AssertUnwindSafe(|| scope.close()));

        let directory = if matches!(&cleanup, Ok(Ok(()))) {
            scope.directory
        } else {
            scope.directory.retain_on_failure()
        };

        match outcome {
            Ok(Ok(value)) => {
                match cleanup {
                    Ok(result) => result?,
                    Err(panic) => resume_unwind(panic),
                }

                directory.finish()?;
                Ok(value)
            }
            Ok(Err(error)) => {
                report_cleanup(cleanup);
                Err(error)
            }
            Err(panic) => {
                report_cleanup(cleanup);
                resume_unwind(panic)
            }
        }
    }

    pub(crate) fn track(&self, connection: Connection) -> Connection {
        self.connections.borrow_mut().push(connection.clone());
        connection
    }

    fn close(&self) -> TestResult {
        let connections = self.connections.take();
        let mut failures = Vec::new();

        self.runtime.block_on(async {
            for connection in connections.into_iter().rev() {
                if let Err(error) = connection.close().await {
                    failures.push(error);
                }
            }
        });

        if failures.is_empty() {
            return Ok(());
        }

        Err(format!("test connection cleanup failed: {failures:?}").into())
    }
}

#[allow(
    clippy::print_stderr,
    reason = "test cleanup failures must remain visible beside the original failure"
)]
fn report_cleanup(cleanup: std::thread::Result<TestResult>) {
    match cleanup {
        Ok(Ok(())) => {}
        Ok(Err(error)) => eprintln!("{error}"),
        Err(_) => eprintln!("test connection cleanup panicked"),
    }
}

#[test]
fn failed_scenario_settles_connections_before_directory_cleanup() {
    let mut retained = None;

    let outcome = catch_unwind(AssertUnwindSafe(|| {
        super::Fixture::run(false, |fixture| {
            retained = Some((
                fixture.connection.clone(),
                Arc::clone(&fixture.runtime),
                fixture.directory.path().to_path_buf(),
            ));

            resume_unwind(Box::new("controlled fixture failure"));
        })
    }));

    assert_eq!(
        outcome.unwrap_err().downcast_ref::<&str>(),
        Some(&"controlled fixture failure")
    );

    let (connection, _runtime, path) = retained.unwrap();

    assert!(matches!(
        connection.reserve_listener(),
        Err(mikrio_core::ConnectionError::StoreClosed)
    ));

    assert!(!path.exists(), "fixture directory remains after cleanup");
}

#[test]
fn early_error_settles_fixture_connections_without_replacing_the_error() {
    let mut retained = None;

    let outcome = super::Fixture::run(false, |fixture| {
        retained = Some((
            fixture.connection.clone(),
            Arc::clone(&fixture.runtime),
            fixture.directory.path().to_path_buf(),
        ));

        Err("controlled scenario error".into())
    });

    assert_eq!(
        outcome.unwrap_err().to_string(),
        "controlled scenario error"
    );

    let (connection, _runtime, path) = retained.unwrap();

    assert!(matches!(
        connection.reserve_listener(),
        Err(mikrio_core::ConnectionError::StoreClosed)
    ));

    assert!(!path.exists(), "fixture directory remains after cleanup");
}

#[test]
fn poisoned_observations_release_active_upload_without_another_panic() {
    let observed = Arc::new(std::sync::Mutex::new(super::Observations::default()));
    let upload = super::ActiveUpload::new(&observed);

    let outcome = catch_unwind(AssertUnwindSafe(|| {
        let _guard = observed.lock().unwrap();
        resume_unwind(Box::new("controlled observation failure"));
    }));

    assert_eq!(
        outcome.unwrap_err().downcast_ref::<&str>(),
        Some(&"controlled observation failure")
    );

    drop(upload);

    let observed = observed
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);

    assert_eq!(observed.active_uploads, 0);
}
