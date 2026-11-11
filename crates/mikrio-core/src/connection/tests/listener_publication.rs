//! Real payload/configuration effects across the listener commit fence.

use std::io::BufRead as _;
use std::sync::atomic::AtomicBool;

use super::objects::{put, read};
use super::*;
use crate::access::{Bootstrap, BucketAccessRequest, PrincipalName};
use crate::object::BucketName;
use crate::test_process as process;
use crate::{MutationOutcome, OperationFailure, PublicAccessBlock};

const CHILD: &str = "connection::tests::listener_publication::listener_publication_child";
const ADMIN: &str =
    r#"{"Version":"2012-10-17","Statement":{"Effect":"Allow","Action":"s3:*","Resource":"*"}}"#;
static ENTERED: AtomicBool = AtomicBool::new(false);

fn bucket() -> BucketAccessRequest {
    BucketAccessRequest {
        bucket: BucketName::parse("attachments").unwrap(),
        expected_owner: None,
    }
}

fn flags(value: bool) -> PublicAccessBlock {
    PublicAccessBlock {
        block_public_acls: value,
        ignore_public_acls: value,
        block_public_policy: value,
        restrict_public_buckets: value,
    }
}

fn checkpoint(point: &str) -> io::Result<()> {
    let mode = std::env::var("mikrio_probe_mode").map_err(io::Error::other)?;

    let (_, target) = mode
        .split_once(':')
        .ok_or_else(|| io::Error::other("missing checkpoint"))?;

    if point != target || ENTERED.swap(true, Ordering::SeqCst) {
        return Ok(());
    }

    announce("CHECKPOINT")?;
    let mut command = String::new();
    io::stdin().lock().read_line(&mut command)?;

    if command != "exit\n" {
        return Err(io::Error::other("owned checkpoint control closed"));
    }

    Ok(())
}

fn announce(message: &str) -> io::Result<()> {
    let mut output = io::stdout().lock();
    writeln!(output, "PROBE {message}")?;
    output.flush()
}

fn close_publication(credential: &str, point: &str) -> TestResult {
    let _serial = process::process_scenario()?;
    let directory = TestDirectory::new()?.retain_on_failure();

    runtime()?.block_on(async {
        let path = directory.path().join(format!("{credential}-{point}"));
        let peer = Connection::open(path.clone(), None).await?;
        peer.pause_maintenance();
        peer.create_bucket(bucket().bucket).await?;

        peer.put_object(put("selected"), b"predecessor".as_slice())
            .await?;

        let mut child = process::Probe::start(&path, &format!("{credential}:{point}"), CHILD)?;
        assert_eq!(child.observation_async().await?, "CHECKPOINT");
        assert_eq!(child.observation_async().await?, "CLOSE_WAITING");

        let expected = if point == "publication.commit.after" {
            b"replacement".as_slice()
        } else {
            b"predecessor".as_slice()
        };

        let (_, mut body) = peer.get_object(read("selected")).await?;
        assert_eq!(body.read_all(100).await?, expected);

        assert_eq!(
            peer.get_public_access_block(bucket()).await?,
            flags(point != "bucket.public_access_block.put.commit.after")
        );

        child.finish_async().await?;

        let expected = if matches!(point, "commit_fence.entered" | "publication.commit.after") {
            b"replacement".as_slice()
        } else {
            b"predecessor".as_slice()
        };

        let (_, mut body) = peer.get_object(read("selected")).await?;
        assert_eq!(body.read_all(100).await?, expected);

        assert_eq!(
            peer.get_public_access_block(bucket()).await?,
            flags(!point.starts_with("bucket."))
        );

        peer.close().await?;

        let reopened = Connection::open(path, None).await?;
        let (_, mut body) = reopened.get_object(read("selected")).await?;
        assert_eq!(body.read_all(100).await?, expected);

        assert_eq!(
            reopened.get_public_access_block(bucket()).await?,
            flags(!point.starts_with("bucket."))
        );

        reopened
            .head_bucket(BucketName::parse("direct-after-close")?, None)
            .await?;

        reopened.close().await?;

        Ok::<_, Box<dyn std::error::Error>>(())
    })?;

    directory.finish()?;
    Ok(())
}

#[test]
fn application_close_before_fence_preserves_predecessor() -> TestResult {
    close_publication("application", "payload.published")
}

#[test]
fn application_close_after_fence_waits_for_payload_commit() -> TestResult {
    close_publication("application", "commit_fence.entered")
}

#[test]
fn application_close_after_payload_commit_keeps_replacement() -> TestResult {
    close_publication("application", "publication.commit.after")
}

#[test]
fn application_close_waits_for_configuration_commit() -> TestResult {
    close_publication("application", "bucket.public_access_block.put.begun")
}

#[test]
fn application_close_keeps_committed_configuration() -> TestResult {
    close_publication("application", "bucket.public_access_block.put.commit.after")
}

#[test]
fn bootstrap_close_before_fence_preserves_predecessor() -> TestResult {
    close_publication("bootstrap", "payload.published")
}

#[test]
fn bootstrap_close_after_fence_waits_for_payload_commit() -> TestResult {
    close_publication("bootstrap", "commit_fence.entered")
}

#[test]
fn bootstrap_close_after_payload_commit_keeps_replacement() -> TestResult {
    close_publication("bootstrap", "publication.commit.after")
}

#[test]
fn bootstrap_close_waits_for_configuration_commit() -> TestResult {
    close_publication("bootstrap", "bucket.public_access_block.put.begun")
}

#[test]
fn bootstrap_close_keeps_committed_configuration() -> TestResult {
    close_publication("bootstrap", "bucket.public_access_block.put.commit.after")
}

#[test]
#[ignore = "Owned subprocess entry driven by the listener publication scenario"]
fn listener_publication_child() -> TestResult {
    let path =
        PathBuf::from(std::env::var_os("mikrio_probe_directory").ok_or("missing owned directory")?);

    let mode = std::env::var("mikrio_probe_mode")?;
    let (credential, point) = mode.split_once(':').ok_or("missing checkpoint")?;

    runtime()?.block_on(async {
        let connection = Connection::open(path, None).await?;
        connection.pause_maintenance();
        let bootstrap = Bootstrap::generate()?;

        let identity = if credential == "bootstrap" {
            bootstrap
                .resolve(bootstrap.access_key_id())
                .ok_or("missing bootstrap key")?
                .verified()
        } else {
            let principal = connection
                .create_principal(PrincipalName::new("writer".into())?)
                .await?;

            connection
                .put_principal_policy(principal.principal_id, ADMIN.into())
                .await?;

            let key = connection.create_access_key(principal.principal_id).await?;

            connection
                .resolve_application_key(key.access_key.access_key_id.as_str().into())
                .await?
                .verified()
        };

        let lease = connection.reserve_listener()?;
        lease.ready()?;
        let scoped = lease.connection().clone();
        let configuration = point.starts_with("bucket.");
        crate::protocol_fault::install(checkpoint)?;

        let write = tokio::spawn(async move {
            if configuration {
                scoped
                    .put_public_access_block_http(Arc::new(identity), bucket(), flags(false))
                    .await
            } else {
                scoped
                    .put_object_http(
                        Arc::new(identity),
                        put("selected"),
                        b"replacement".as_slice(),
                        || Ok(crate::integrity::ExpectedDigests::default()),
                    )
                    .await
                    .map(|_| ())
            }
        });

        tokio::time::timeout(Duration::from_secs(10), async {
            while !ENTERED.load(Ordering::SeqCst) {
                tokio::task::yield_now().await;
            }
        })
        .await?;

        lease.begin_close();

        for _ in 0..2 {
            let mut close = Box::pin(lease.finish());

            std::future::poll_fn(|context| {
                assert!(close.as_mut().poll(context).is_pending());
                Poll::Ready(())
            })
            .await;

            drop(close);
        }

        assert!(matches!(
            connection.reserve_listener(),
            Err(ConnectionError::ListenerAlreadyRunning)
        ));

        assert!(matches!(
            lease.connection().admit(),
            Err(ConnectionError::StoreClosing)
        ));

        announce("CLOSE_WAITING")?;
        let result = write.await?;

        if point == "payload.published" {
            let error = result.unwrap_err();
            assert_eq!(error.failure(), OperationFailure::OperationCanceled);
            assert_eq!(error.outcome(), Some(MutationOutcome::NotCommitted));
        } else {
            result?;
        }

        tokio::time::timeout(Duration::from_secs(10), lease.finish()).await??;
        bootstrap.close();
        drop(lease);
        let replacement = connection.reserve_listener()?;
        replacement.ready()?;
        replacement.finish().await?;
        drop(replacement);

        connection
            .create_bucket(BucketName::parse("direct-after-close")?)
            .await?;

        connection.close().await?;
        Ok::<_, Box<dyn std::error::Error>>(())
    })
}
