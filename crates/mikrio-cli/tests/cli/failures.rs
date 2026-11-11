//! Startup interruption and failures after authoritative mutation settlement.
#![cfg(unix)]

use std::time::{Duration, Instant};

use rustix::fs::{Mode, OFlags};

use super::field;
use super::support::{Process, TestResult, command, error, run, success};
use super::test_directory::TestDirectory;
use super::test_stores;

#[test]
fn executable_without_its_companion_cannot_report_success() -> TestResult {
    let directory = TestDirectory::new()?;
    let copied = directory.path().join("mikrio");
    std::fs::copy(super::support::executable(), &copied)?;
    let mut invocation = std::process::Command::new(copied);
    invocation.arg("--help").current_dir(directory.path());

    for name in [
        "DYLD_LIBRARY_PATH",
        "DYLD_FALLBACK_LIBRARY_PATH",
        "LD_LIBRARY_PATH",
    ] {
        invocation.env_remove(name);
    }

    let output = Process::spawn(invocation)?.finish()?;
    assert!(!output.status.success());
    assert_eq!(output.stdout, Vec::<u8>::new());
    assert!(!directory.path().join("store").exists());
    directory.finish()?;
    Ok(())
}

#[test]
fn shutdown_while_policy_input_waits_does_not_open_storage() -> TestResult {
    let directory = TestDirectory::new()?;
    let pipe = directory.path().join("policy-input");

    assert!(
        std::process::Command::new("mkfifo")
            .arg(&pipe)
            .status()?
            .success()
    );

    let child = Process::spawn(command(
        directory.path(),
        &[
            "policy",
            "apply",
            "missing",
            "--bucket=uploads",
            "--file=policy-input",
            "--json",
        ],
    ))?;

    let deadline = Instant::now() + Duration::from_secs(20);

    let writer = loop {
        match rustix::fs::open(&pipe, OFlags::WRONLY | OFlags::NONBLOCK, Mode::empty()) {
            Ok(writer) => break writer,
            Err(rustix::io::Errno::NXIO) if Instant::now() < deadline => {
                std::thread::sleep(Duration::from_millis(5));
            }
            Err(error) => return Err(error.into()),
        }
    };

    // Opening the writer proves that the CLI reached input acquisition, which
    // follows signal registration. Keep it open so EOF cannot finish the read.
    child.signal("-TERM")?;
    error(&child.finish()?, 1, "input", "OperationCanceled")?;
    drop(writer);
    assert!(!directory.path().join("missing").exists());
    directory.finish()?;
    Ok(())
}

#[test]
fn broken_result_output_preserves_committed_key_and_reports_outcome() -> TestResult {
    let directory = TestDirectory::new()?;
    let root = directory.path();
    success(&run(root, &["init", "store", "--json"])?)?;

    let principal = success(&run(
        root,
        &["principal", "create", "store", "--name=app", "--json"],
    )?)?;

    let principal_id = field(&principal, "principalId")?;

    let output = Process::broken_stdout(command(
        root,
        &[
            "access-key",
            "create",
            "store",
            "--principal-id",
            principal_id,
            "--json",
        ],
    ))?
    .finish()?;

    let failure = error(&output, 1, "output", "IoError")?;
    assert_eq!(failure["mutationOutcome"], "Committed");
    assert!(field(&failure, "message")?.contains("do not repeat"));

    let inventory = success(&run(
        root,
        &[
            "access-key",
            "list",
            "store",
            "--principal-id",
            principal_id,
            "--json",
        ],
    )?)?;

    assert_eq!(
        inventory["accessKeys"]
            .as_array()
            .ok_or("missing keys")?
            .len(),
        1
    );

    assert!(!inventory.to_string().contains("secretAccessKey"));

    let output = Process::broken_stdout(command(
        root,
        &[
            "access-key",
            "create",
            "store",
            "--principal-id",
            principal_id,
        ],
    ))?
    .finish()?;

    assert_eq!(output.status.code(), Some(1));
    assert_eq!(output.stdout, Vec::<u8>::new());
    let failure = String::from_utf8(output.stderr)?;
    assert!(failure.contains("access-key create"));
    assert!(failure.contains("Committed"));
    assert!(failure.contains("do not repeat"));
    directory.finish()?;
    Ok(())
}

#[test]
fn broken_readiness_output_closes_listener_and_preserves_bootstrap() -> TestResult {
    let directory = TestDirectory::new()?;
    let root = directory.path();

    let output = Process::broken_stdout(command(
        root,
        &["serve", "store", "--bucket=uploads", "--json"],
    ))?
    .finish()?;

    let failure = error(&output, 1, "output", "IoError")?;
    assert!(failure.get("mutationOutcome").is_none());

    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()?;

    test_stores::run(&runtime, async |stores| {
        let store = stores
            .open(mikrio::OpenOptions::new(root.join("store")).create_if_missing(false))
            .await?;

        store
            .head_bucket(mikrio::HeadBucketInput::builder("uploads").build()?)
            .await?;

        Ok(())
    })?;

    directory.finish()?;
    Ok(())
}
