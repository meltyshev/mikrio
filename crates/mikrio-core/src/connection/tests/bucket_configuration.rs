use super::*;
use crate::access::BucketAccessRequest;
use crate::object::BucketName;
use crate::{MutationOutcome, OperationFailure, PublicAccessBlock};

mod command_loss;
mod cors;
mod lifecycle;
mod outcomes;

const DENY: &str = r#"{"Version":"2012-10-17","Statement":{"Effect":"Deny","Principal":"*","Action":"s3:*","Resource":"*"}}"#;

#[test]
fn invalid_policy_envelopes_report_not_committed_without_replacing_state() -> TestResult {
    let directory = TestDirectory::new()?;

    runtime()?.block_on(async {
        let connection = Connection::open(directory.path().join("store"), None).await?;

        let request = BucketAccessRequest {
            bucket: BucketName::parse("photos")?,
            expected_owner: None,
        };

        connection.create_bucket(request.bucket.clone()).await?;

        connection
            .put_bucket_policy(request.clone(), DENY.into())
            .await?;

        for document in ["{".to_owned(), " ".repeat(20 * 1024 + 1)] {
            let error = connection
                .put_bucket_policy(request.clone(), document)
                .await
                .unwrap_err();

            assert_eq!(error.failure(), OperationFailure::InvalidArgument);
            assert_eq!(error.outcome(), Some(MutationOutcome::NotCommitted));

            assert_eq!(
                connection
                    .get_bucket_policy(request.clone())
                    .await?
                    .document,
                DENY
            );
        }

        connection.close().await?;
        Ok::<_, Box<dyn std::error::Error>>(())
    })?;

    directory.finish()?;
    Ok(())
}

#[test]
fn abandoned_policy_and_close_fenced_flags_preserve_prior_configuration() -> TestResult {
    let directory = TestDirectory::new()?;

    runtime()?.block_on(async {
        let path = directory.path().join("store");
        let connection = Connection::open(path.clone(), None).await?;
        connection.pause_maintenance();
        let peer = Connection::open(path, None).await?;
        peer.pause_maintenance();
        let bucket = BucketName::parse("photos")?;
        connection.create_bucket(bucket.clone()).await?;

        let request = BucketAccessRequest {
            bucket,
            expected_owner: None,
        };

        let original_flags = peer.get_public_access_block(request.clone()).await?;

        for close_connection in [false, true] {
            let (entered, observed) = oneshot::channel();
            let (release, released) = std::sync::mpsc::sync_channel(1);
            let worker_owner = connection.clone();

            let blocker = tokio::spawn(async move {
                worker_owner
                    .probe(move |_| {
                        entered
                            .send(())
                            .map_err(|()| io::Error::other("missing test waiter"))?;

                        released
                            .recv_timeout(Duration::from_secs(10))
                            .map_err(io::Error::other)
                    })
                    .await
            });

            observed.await?;
            let capacity = connection.owner.state.context.sender.capacity();

            let mut change: std::pin::Pin<
                Box<dyn Future<Output = Result<(), crate::OperationError>>>,
            > = if close_connection {
                Box::pin(connection.put_public_access_block(
                    request.clone(),
                    PublicAccessBlock {
                        block_public_acls: false,
                        ignore_public_acls: false,
                        block_public_policy: false,
                        restrict_public_buckets: false,
                    },
                ))
            } else {
                Box::pin(connection.put_bucket_policy(request.clone(), DENY.into()))
            };

            std::future::poll_fn(|context| {
                assert!(change.as_mut().poll(context).is_pending());
                Poll::Ready(())
            })
            .await;

            assert_eq!(
                connection.owner.state.context.sender.capacity(),
                capacity - 1
            );

            if close_connection {
                let mut close = Box::pin(connection.close());

                std::future::poll_fn(|context| {
                    assert!(close.as_mut().poll(context).is_pending());
                    Poll::Ready(())
                })
                .await;

                release.send(())?;
                blocker.await??;
                let error = change.await.unwrap_err();
                assert_eq!(error.failure(), OperationFailure::StoreClosing);
                assert_eq!(error.outcome(), Some(MutationOutcome::NotCommitted));
                close.await?;
            } else {
                drop(change);

                // The queued native command still owns its operation ticket.
                assert_eq!(
                    connection.owner.state.activity.lock().unwrap().operations,
                    1
                );

                release.send(())?;
                blocker.await??;

                assert_eq!(
                    connection
                        .get_bucket_policy(request.clone())
                        .await
                        .unwrap_err()
                        .failure(),
                    OperationFailure::NoSuchBucketPolicy
                );
            }

            assert_eq!(
                peer.get_bucket_policy(request.clone())
                    .await
                    .unwrap_err()
                    .failure(),
                OperationFailure::NoSuchBucketPolicy
            );

            assert_eq!(
                peer.get_public_access_block(request.clone()).await?,
                original_flags
            );
        }

        peer.close().await?;
        Ok::<_, Box<dyn std::error::Error>>(())
    })?;

    directory.finish()?;
    Ok(())
}
