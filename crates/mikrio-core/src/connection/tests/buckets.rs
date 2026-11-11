use super::*;
use crate::object::BucketName;
use crate::{MutationOutcome, OperationFailure};

#[test]
fn abandoned_queued_mutation_settles_without_creating_a_bucket() -> TestResult {
    let directory = TestDirectory::new()?;

    runtime()?.block_on(async {
        let connection = Connection::open(directory.path().join("store"), None).await?;
        connection.pause_maintenance();
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
        let bucket = BucketName::parse("attachments")?;
        let mut create = Box::pin(connection.create_bucket(bucket.clone()));

        std::future::poll_fn(|context| {
            assert!(create.as_mut().poll(context).is_pending());
            Poll::Ready(())
        })
        .await;

        assert_eq!(
            connection.owner.state.context.sender.capacity(),
            QUEUE_CAPACITY - 1
        );

        drop(create);

        assert_eq!(
            connection.owner.state.activity.lock().unwrap().operations,
            1
        );

        release.send(())?;
        blocker.await??;

        assert_eq!(
            connection
                .head_bucket(bucket.clone(), None)
                .await
                .unwrap_err()
                .failure(),
            OperationFailure::NoSuchBucket
        );

        connection.create_bucket(bucket).await?;
        connection.close().await?;
        Ok::<_, Box<dyn std::error::Error>>(())
    })?;

    directory.finish()?;
    Ok(())
}

#[test]
fn close_before_mutation_entry_prevents_commit_and_preserves_peer() -> TestResult {
    let directory = TestDirectory::new()?;

    runtime()?.block_on(async {
        let path = directory.path().join("store");
        let connection = Connection::open(path.clone(), None).await?;
        let peer = Connection::open(path, None).await?;
        connection.pause_maintenance();
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
        let bucket = BucketName::parse("attachments")?;
        let mut create = Box::pin(connection.create_bucket(bucket.clone()));

        std::future::poll_fn(|context| {
            assert!(create.as_mut().poll(context).is_pending());
            Poll::Ready(())
        })
        .await;

        let mut close = Box::pin(connection.close());

        std::future::poll_fn(|context| {
            assert!(close.as_mut().poll(context).is_pending());
            Poll::Ready(())
        })
        .await;

        release.send(())?;
        blocker.await??;
        let error = create.await.unwrap_err();
        assert_eq!(error.failure(), OperationFailure::StoreClosing);
        assert_eq!(error.outcome(), Some(MutationOutcome::NotCommitted));
        close.await?;

        assert_eq!(
            peer.head_bucket(bucket.clone(), None)
                .await
                .unwrap_err()
                .failure(),
            OperationFailure::NoSuchBucket
        );

        peer.create_bucket(bucket).await?;
        peer.close().await?;
        Ok::<_, Box<dyn std::error::Error>>(())
    })?;

    directory.finish()?;
    Ok(())
}
