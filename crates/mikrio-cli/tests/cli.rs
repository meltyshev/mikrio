//! The actual product executable, independent process state and facade readback.

use std::fs;

use serde_json::{Value, json};

#[path = "../../mikrio-core/tests/support/directory.rs"]
mod test_directory;
#[path = "../../mikrio/tests/support/stores.rs"]
mod test_stores;

#[path = "cli/administration.rs"]
mod administration;
#[cfg(unix)]
#[path = "cli/diagnostics.rs"]
mod diagnostics;
#[path = "cli/failures.rs"]
mod failures;
#[path = "cli/listener.rs"]
mod listener;
#[path = "cli/support.rs"]
mod support;

use support::{Process, TestResult, command, error, run, success};
use test_directory::TestDirectory;

#[test]
fn every_command_help_matches_the_reviewed_baseline() -> TestResult {
    let directory = TestDirectory::new()?;

    for (path, expected) in [
        ("", include_str!("help/root.txt")),
        ("init", include_str!("help/init.txt")),
        ("serve", include_str!("help/serve.txt")),
        ("principal", include_str!("help/principal.txt")),
        (
            "principal create",
            include_str!("help/principal-create.txt"),
        ),
        ("principal get", include_str!("help/principal-get.txt")),
        ("principal list", include_str!("help/principal-list.txt")),
        (
            "principal enable",
            include_str!("help/principal-enable.txt"),
        ),
        (
            "principal disable",
            include_str!("help/principal-disable.txt"),
        ),
        (
            "principal delete",
            include_str!("help/principal-delete.txt"),
        ),
        ("access-key", include_str!("help/access-key.txt")),
        (
            "access-key create",
            include_str!("help/access-key-create.txt"),
        ),
        ("access-key list", include_str!("help/access-key-list.txt")),
        (
            "access-key revoke",
            include_str!("help/access-key-revoke.txt"),
        ),
        ("policy", include_str!("help/policy.txt")),
        ("policy apply", include_str!("help/policy-apply.txt")),
        ("policy get", include_str!("help/policy-get.txt")),
        ("policy delete", include_str!("help/policy-delete.txt")),
    ] {
        let mut arguments = path.split_whitespace().collect::<Vec<_>>();
        arguments.extend(["--json", "--help"]);
        let output = run(directory.path(), &arguments)?;
        assert!(output.status.success(), "{path}");
        assert_eq!(output.stderr, Vec::<u8>::new());
        assert_eq!(String::from_utf8(output.stdout)?, expected, "{path}");
    }

    directory.finish()?;
    Ok(())
}

#[test]
fn help_and_version_ignore_configuration_and_have_no_store_effects() -> TestResult {
    let directory = TestDirectory::new()?;
    let root = directory.path();

    for arguments in [
        vec!["--help"],
        vec!["--json", "--help"],
        vec!["-h"],
        vec!["--version"],
        vec!["--json", "-V"],
        vec!["principal", "--help"],
        vec!["access-key", "-h"],
        vec!["policy", "--json", "--help"],
        vec!["policy", "apply", "--json", "--help"],
        vec!["help", "policy", "apply"],
        vec!["--json", "help", "serve"],
        vec!["help", "--json", "policy", "apply"],
        vec!["help", "policy", "--json", "apply"],
        vec!["help", "policy", "apply", "--json"],
        vec!["principal", "help", "--json", "list"],
        vec!["policy", "help", "apply", "--json"],
    ] {
        let mut invocation = command(root, &arguments);

        invocation
            .env("mikrio_region", "INVALID")
            .env("mikrio_port", "not-a-port")
            .env("mikrio_secret_access_key", "sensitive-unpaired-secret");

        let output = Process::spawn(invocation)?.finish()?;
        assert!(output.status.success(), "help failed for {arguments:?}");
        assert_eq!(output.stderr, Vec::<u8>::new());
        let stdout = String::from_utf8(output.stdout)?;
        assert!(stdout.starts_with("mikrio") || stdout.contains("Usage:"));
        assert!(!stdout.contains("sensitive-unpaired-secret"));
    }

    assert!(!root.join("store").exists());
    directory.finish()?;
    Ok(())
}

#[test]
fn invalid_grammar_is_structured_and_never_opens_storage() -> TestResult {
    let directory = TestDirectory::new()?;

    for arguments in [
        vec!["--json"],
        vec!["--json", "principal"],
        vec!["--json", "unknown"],
        vec!["--json", "serve"],
        vec!["serve", "store", "--json", "--port", "0", "--port=1"],
        vec!["--json", "principal", "list", "store", "--json"],
        vec!["principal", "--json", "list", "store", "--json"],
        vec!["--json", "help", "serve", "--json"],
        vec!["--json", "--help", "--json"],
        vec!["--json", "--version", "--json"],
        vec!["--json", "help", "--", "--json"],
        vec!["--json", "init", "store", "--json=true"],
        vec!["--json", "serve", "store", "--bind", "127.0.0.1"],
        vec!["--json", "serve", "store", "--reg", "eu-west-1"],
        vec!["--json", "init", "store", "--host", "127.0.0.1"],
        vec!["--json", "serve", "--version"],
        vec!["--json", "policy", "apply", "store", "--bucket=uploads"],
        vec![
            "--json",
            "principal",
            "--name",
            "wrong-level",
            "create",
            "store",
        ],
        vec![
            "--json",
            "principal",
            "get",
            "store",
            "--principal-id",
            "0",
            "--region",
            "us-east-1",
        ],
        vec![
            "--json",
            "principal",
            "create",
            "store",
            "--name",
            "one",
            "--name",
            "two",
        ],
        vec!["--json", "policy", "get", "store"],
        vec![
            "--json",
            "policy",
            "get",
            "store",
            "--bucket",
            "uploads",
            "--principal-id",
            "0",
        ],
        vec![
            "--json", "policy", "get", "store", "--bucket", "uploads", "--bucket", "other",
        ],
        vec!["--json", "init", "store", "extra"],
    ] {
        let output = run(directory.path(), &arguments)?;
        error(&output, 2, "arguments", "InvalidArgument")?;
        assert!(
            !directory.path().join("store").exists(),
            "effects for {arguments:?}"
        );
    }

    let output = run(directory.path(), &["--json"])?;
    assert!(
        error(&output, 2, "arguments", "InvalidArgument")?
            .get("command")
            .is_none()
    );

    let output = run(directory.path(), &["principal"])?;
    assert_eq!(output.status.code(), Some(2));
    assert!(String::from_utf8(output.stderr)?.contains("Usage:"));
    directory.finish()?;
    Ok(())
}

#[test]
fn grammar_errors_redact_private_arguments_and_keep_static_usage() -> TestResult {
    let directory = TestDirectory::new()?;
    let root = directory.path().join("workspace");
    fs::create_dir(&root)?;
    let private = "private-argument-canary\n\u{1b}[31m";

    let cases = [
        (vec![private], None),
        (vec!["principal", private], None),
        (vec!["help", private], None),
        (vec!["init", "store", private], Some("init")),
        (
            vec!["serve", "store", "--private-option-canary=value"],
            Some("serve"),
        ),
        (
            vec!["serve", private, "--port=0", "--port=1"],
            Some("serve"),
        ),
        (
            vec![
                "policy",
                "get",
                private,
                "--bucket=uploads",
                "--principal-id=0",
            ],
            Some("policy get"),
        ),
    ];

    for json_output in [false, true] {
        for (arguments, expected_command) in &cases {
            let mut arguments = arguments.clone();

            if json_output {
                arguments.insert(0, "--json");
            }

            let invocation = command(&root, &arguments);

            #[cfg(unix)]
            let invocation = {
                use std::os::unix::process::CommandExt;

                let mut invocation = invocation;
                invocation.arg0("private-executable-canary");
                invocation
            };

            let output = Process::spawn(invocation)?.finish()?;
            assert_eq!(output.status.code(), Some(2), "{arguments:?}");
            assert_eq!(output.stdout, Vec::<u8>::new());
            let stderr = std::str::from_utf8(&output.stderr)?;
            assert!(!stderr.contains("canary"), "{arguments:?}: {stderr}");
            assert!(!stderr.contains('\u{1b}'));
            assert!(stderr.contains("Usage: mikrio"));

            if json_output {
                let failure = error(&output, 2, "arguments", "InvalidArgument")?;
                assert_eq!(failure["command"].as_str(), *expected_command);
            }
        }
    }

    assert_eq!(fs::read_dir(&root)?.count(), 0);
    directory.finish()?;
    Ok(())
}

#[test]
fn json_inheritance_option_order_and_terminators_preserve_literal_targets() -> TestResult {
    let directory = TestDirectory::new()?;
    let root = directory.path();

    success(&run(
        root,
        &["init", "--bucket=uploads", "--json", "--", "-store"],
    )?)?;

    for arguments in [
        vec!["--json", "principal", "list", "--", "-store"],
        vec!["principal", "--json", "list", "--", "-store"],
        vec!["principal", "list", "--json", "--", "-store"],
        vec!["principal", "list", "./-store", "--json"],
    ] {
        assert_eq!(success(&run(root, &arguments)?)?, json!({"principals": []}));
    }

    let principal = success(&run(
        root,
        &[
            "principal",
            "create",
            "--name=literal",
            "./-store",
            "--json",
        ],
    )?)?;

    let principal_id = principal["principalId"]
        .as_str()
        .ok_or("missing principal ID")?;

    let policy = r#"{"Version":"2012-10-17","Statement":{"Effect":"Allow","Action":"s3:GetObject","Resource":"arn:aws:s3:::uploads/*"}}"#;
    fs::write(root.join("-policy"), policy)?;
    fs::write(root.join("-"), policy)?;

    for file in ["--file=-policy", "--file=./-"] {
        success(&run(
            root,
            &[
                "policy",
                "apply",
                "./-store",
                "--principal-id",
                principal_id,
                file,
                "--json",
            ],
        )?)?;
    }

    let output = run(
        root,
        &["policy", "get", "./-store", "--principal-id", principal_id],
    )?;

    assert!(output.status.success());
    assert_eq!(output.stdout, policy.as_bytes());
    directory.finish()?;
    Ok(())
}

#[test]
fn selected_invalid_configuration_fails_before_initialization() -> TestResult {
    let directory = TestDirectory::new()?;

    for (name, value) in [
        ("mikrio_host", "localhost"),
        ("mikrio_port", "65536"),
        ("mikrio_port", "+1"),
        ("mikrio_port", ""),
        ("mikrio_public_endpoint", ""),
        ("mikrio_region", "INVALID"),
        ("mikrio_access_key_id", ""),
        ("mikrio_secret_access_key", "one-secret"),
    ] {
        let mut invocation = command(directory.path(), &["serve", "store", "--json"]);
        invocation.env(name, value);
        let output = Process::spawn(invocation)?.finish()?;
        error(&output, 2, "arguments", "InvalidArgument")?;
        assert!(!directory.path().join("store").exists());
        assert!(!String::from_utf8(output.stderr)?.contains("one-secret"));
    }

    let output = run(
        directory.path(),
        &[
            "init", "store", "--bucket", "uploads", "--bucket", "INVALID", "--json",
        ],
    )?;

    error(&output, 2, "arguments", "InvalidBucketName")?;
    assert!(!directory.path().join("store").exists());
    directory.finish()?;
    Ok(())
}

#[test]
fn initialization_is_idempotent_and_administration_never_creates_storage() -> TestResult {
    let directory = TestDirectory::new()?;
    let root = directory.path();

    error(
        &run(root, &["principal", "list", "missing/child", "--json"])?,
        1,
        "open",
        "InvalidStoreDirectory",
    )?;

    assert!(!root.join("missing").exists());
    fs::create_dir(root.join("empty"))?;

    error(
        &run(root, &["principal", "list", "empty", "--json"])?,
        1,
        "open",
        "InvalidStoreDirectory",
    )?;

    assert_eq!(fs::read_dir(root.join("empty"))?.count(), 0);

    let mut invocation = command(
        root,
        &[
            "init",
            "store",
            "--region=eu-west-1",
            "--bucket",
            "uploads",
            "--bucket",
            "uploads",
            "--json",
        ],
    );

    invocation
        .env("mikrio_region", "INVALID")
        .env("mikrio_port", "INVALID")
        .env("mikrio_secret_access_key", "ignored-secret");

    let first = success(&Process::spawn(invocation)?.finish()?)?;
    assert_eq!(first["region"], "eu-west-1");
    assert_eq!(first.as_object().ok_or("expected object")?.len(), 6);

    assert_eq!(
        first,
        success(&run(
            root,
            &["init", "store", "--bucket=uploads", "--json"]
        )?)?
    );

    error(
        &run(root, &["init", "store", "--region=us-east-1", "--json"])?,
        1,
        "open",
        "RegionMismatch",
    )?;

    let mut invocation = command(root, &["principal", "list", "store", "--json"]);

    invocation
        .env("mikrio_region", "INVALID")
        .env("mikrio_secret_access_key", "ignored-secret");

    assert_eq!(
        success(&Process::spawn(invocation)?.finish()?)?,
        json!({"principals": []})
    );

    fs::create_dir(root.join("unrelated"))?;
    fs::write(root.join("unrelated/sentinel"), "preserve")?;

    error(
        &run(root, &["init", "unrelated", "--json"])?,
        1,
        "open",
        "InvalidStoreDirectory",
    )?;

    assert_eq!(
        fs::read_to_string(root.join("unrelated/sentinel"))?,
        "preserve"
    );

    directory.finish()?;
    Ok(())
}

fn field<'a>(value: &'a Value, name: &str) -> TestResult<&'a str> {
    value[name]
        .as_str()
        .ok_or_else(|| format!("missing {name}").into())
}
