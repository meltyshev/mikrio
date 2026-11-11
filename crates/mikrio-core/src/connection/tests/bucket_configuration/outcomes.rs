//! Metadata settlement must survive the connection worker's error projection.

use std::error::Error as _;
use std::io::BufRead as _;
use std::sync::atomic::{AtomicUsize, Ordering};

use super::*;
use crate::test_process as process;

const CHILD: &str =
    "connection::tests::bucket_configuration::outcomes::configuration_outcome_child";
const REPLACEMENT: &str = r#"{"Version":"2012-10-17","Statement":{"Effect":"Deny","Principal":"*","Action":"s3:GetObject","Resource":"arn:aws:s3:::photos/*"}}"#;
const CAUSE: &str = "controlled configuration transaction acknowledgment failure";
static HITS: AtomicUsize = AtomicUsize::new(0);

fn checkpoint(point: &str) -> io::Result<()> {
    let mode = std::env::var("mikrio_probe_mode").map_err(io::Error::other)?;

    let target = match mode.as_str() {
        "put-before" => "bucket.policy.put.commit.before",
        "put-after" => "bucket.policy.put.commit.after",
        "delete-before" => "bucket.policy.delete.commit.before",
        "delete-after" => "bucket.policy.delete.commit.after",
        _ => return Err(io::Error::other("unknown configuration outcome scenario")),
    };

    if point != target {
        return Ok(());
    }

    assert_eq!(HITS.fetch_add(1, Ordering::SeqCst), 0);
    Err(io::Error::other(CAUSE))
}

async fn assert_policy(connection: &Connection, mode: &str) -> TestResult {
    let request = BucketAccessRequest {
        bucket: BucketName::parse("photos")?,
        expected_owner: None,
    };

    if mode == "delete-after" {
        let error = connection.get_bucket_policy(request).await.unwrap_err();
        assert_eq!(error.failure(), OperationFailure::NoSuchBucketPolicy);
        assert_eq!(error.outcome(), None);
        return Ok(());
    }

    let expected = if mode == "put-after" {
        REPLACEMENT
    } else {
        DENY
    };

    assert_eq!(
        connection.get_bucket_policy(request).await?.document,
        expected
    );

    Ok(())
}

fn scenario(mode: &str) -> TestResult {
    let _serial = process::process_scenario()?;
    let directory = TestDirectory::new()?.retain_on_failure();

    runtime()?.block_on(async {
        let path = directory.path().join("store");
        let connection = Connection::open(path.clone(), None).await?;
        connection.pause_maintenance();

        let request = BucketAccessRequest {
            bucket: BucketName::parse("photos")?,
            expected_owner: None,
        };

        connection.create_bucket(request.bucket.clone()).await?;

        connection
            .put_bucket_policy(request.clone(), DENY.into())
            .await?;

        connection.close().await?;

        // Layout initialization is outside the unchanged child observation bound.
        let mut child = process::Probe::start(directory.path(), mode, CHILD)?;
        assert_eq!(child.observation_async().await?, "CONFIGURATION_SETTLED");
        child.finish_async().await?;

        let reopened = Connection::open(path, None).await?;
        reopened.pause_maintenance();
        assert_policy(&reopened, mode).await?;

        reopened
            .put_bucket_policy(request.clone(), DENY.into())
            .await?;

        assert_eq!(reopened.get_bucket_policy(request).await?.document, DENY);
        reopened.close().await?;
        Ok::<_, Box<dyn std::error::Error>>(())
    })?;

    directory.finish()?;
    Ok(())
}

#[test]
fn policy_replacement_before_commit_preserves_prior_state_and_not_committed() -> TestResult {
    scenario("put-before")
}

#[test]
fn policy_replacement_lost_ack_preserves_unknown_and_committed_state() -> TestResult {
    scenario("put-after")
}

#[test]
fn policy_deletion_before_commit_preserves_prior_state_and_not_committed() -> TestResult {
    scenario("delete-before")
}

#[test]
fn policy_deletion_lost_ack_preserves_unknown_and_committed_state() -> TestResult {
    scenario("delete-after")
}

#[test]
#[ignore = "owned subprocess for configuration transaction outcomes"]
fn configuration_outcome_child() -> TestResult {
    let path =
        PathBuf::from(std::env::var_os("mikrio_probe_directory").ok_or("missing directory")?)
            .join("store");

    let mode = std::env::var("mikrio_probe_mode")?;

    runtime()?.block_on(async {
        let connection = Connection::open(path.clone(), None).await?;
        connection.pause_maintenance();
        let peer = Connection::open(path, None).await?;
        peer.pause_maintenance();
        crate::protocol_fault::install(checkpoint)?;

        let request = BucketAccessRequest {
            bucket: BucketName::parse("photos")?,
            expected_owner: None,
        };

        let error = if mode.starts_with("put-") {
            connection
                .put_bucket_policy(request, REPLACEMENT.into())
                .await
                .unwrap_err()
        } else {
            connection.delete_bucket_policy(request).await.unwrap_err()
        };

        assert_eq!(error.failure(), OperationFailure::IoError);

        let expected = if mode.ends_with("-before") {
            MutationOutcome::NotCommitted
        } else {
            MutationOutcome::Unknown
        };

        assert_eq!(error.outcome(), Some(expected));

        let cause = error
            .source()
            .and_then(|cause| cause.downcast_ref::<io::Error>())
            .ok_or("missing original typed I/O cause")?;

        assert_eq!(cause.kind(), io::ErrorKind::Other);
        assert_eq!(cause.to_string(), CAUSE);
        assert_eq!(HITS.load(Ordering::SeqCst), 1);
        assert_policy(&peer, &mode).await?;
        peer.close().await?;
        connection.close().await?;
        Ok::<_, Box<dyn std::error::Error>>(())
    })?;

    {
        let mut output = io::stdout().lock();
        writeln!(output, "PROBE CONFIGURATION_SETTLED")?;
        output.flush()?;
    }

    let mut line = String::new();
    io::stdin().lock().read_line(&mut line)?;
    assert_eq!(line, "exit\n");
    Ok(())
}
