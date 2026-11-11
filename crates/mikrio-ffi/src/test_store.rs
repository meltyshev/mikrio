//! Own and close a real store around synchronous FFI test assertions.

use std::panic::{AssertUnwindSafe, catch_unwind, resume_unwind};
use std::sync::Arc;
use std::time::{SystemTime, UNIX_EPOCH};

use mikrio_embed::{CloseGate, HostRuntime};

use crate::handles::{self, Connection};

pub(crate) fn run(name: &str, scenario: impl FnOnce(&Connection)) -> Result<(), String> {
    let sequence = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_err(|error| format!("fixture clock failed: {error}"))?
        .as_nanos();

    let directory = std::env::temp_dir().join(format!(
        "mikrio-ffi-{name}-{}-{sequence}",
        std::process::id()
    ));

    // Exclusive creation establishes ownership before any store work begins.
    std::fs::create_dir(&directory)
        .map_err(|error| format!("fixture directory creation failed: {error}"))?;

    let mut connection = None;

    let outcome = catch_unwind(AssertUnwindSafe(|| -> Result<(), String> {
        let runtime = HostRuntime::shared().map_err(|error| error.to_string())?;
        let options = mikrio::OpenOptions::new(&directory);

        let pending = runtime
            .submit(async move { mikrio::embedding::open(options).await })
            .map_err(|error| error.to_string())?;

        let (store, diagnostics) = handles::wait(pending)
            .map_err(|error| format!("fixture open delivery failed: {error:?}"))?
            .map_err(|error| error.to_string())?;

        let owner = connection.insert(Connection {
            runtime,
            store,
            diagnostics,
            close: CloseGate::default(),
            registered: Arc::default(),
            diagnostic_closed: Arc::default(),
        });

        scenario(owner);
        Ok(())
    }));

    // Join native shutdown even after a failed assertion; Drop alone only
    // requests it. Remove the owned directory only after successful settlement.
    let cleanup = connection.map_or(Ok(()), |owner| {
        owner
            .close()
            .map_err(|error| format!("fixture close failed: {error:?}"))
    });

    let cleanup = cleanup.and_then(|()| {
        std::fs::remove_dir_all(&directory)
            .map_err(|error| format!("fixture directory cleanup failed: {error}"))
    });

    if let Err(error) = &cleanup {
        report_cleanup(&format!("{error}; retained path: {}", directory.display()));
    }

    match outcome {
        Ok(result) => result.and(cleanup),
        Err(panic) => resume_unwind(panic),
    }
}

#[allow(
    clippy::print_stderr,
    reason = "test cleanup failures must remain visible beside the original panic or error"
)]
fn report_cleanup(error: &str) {
    eprintln!("{error}");
}

#[test]
#[allow(
    clippy::panic,
    reason = "controlled assertion failure verifies fixture teardown"
)]
fn panicking_scenario_closes_store_and_removes_its_directory() {
    let mut retained = None;

    let outcome = catch_unwind(AssertUnwindSafe(|| {
        run("cleanup", |owner| {
            retained = Some((Arc::clone(&owner.runtime), owner.store.clone()));
            panic!("controlled fixture failure");
        })
    }));

    let panic = outcome.expect_err("scenario panic must be preserved");

    assert_eq!(
        panic.downcast_ref::<&str>(),
        Some(&"controlled fixture failure")
    );

    let (runtime, store) = retained.expect("scenario acquired the store");
    assert!(!store.info().directory().exists());

    let error = handles::wait(
        runtime
            .submit(async move { store.get_owner_info().await })
            .unwrap(),
    )
    .unwrap()
    .unwrap_err();

    assert_eq!(error.code(), mikrio::ErrorCode::StoreClosed);
}
