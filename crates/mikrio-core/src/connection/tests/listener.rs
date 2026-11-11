use super::*;
use crate::object::BucketName;
use crate::{MutationOutcome, OperationFailure};

#[test]
fn listener_close_fences_queued_mutation_and_waits_for_native_settlement() -> TestResult {
    let directory = TestDirectory::new()?;

    runtime()?.block_on(async {
        let connection = Connection::open(directory.path().join("store"), None).await?;
        connection.pause_maintenance();
        let listener = connection.reserve_listener()?;
        listener.ready()?;
        let (entered, observed) = oneshot::channel();
        let (release, released) = std::sync::mpsc::sync_channel(1);
        let worker_owner = connection.clone();

        let blocker = tokio::spawn(async move {
            worker_owner
                .probe(move |_| {
                    entered
                        .send(())
                        .map_err(|()| io::Error::other("missing waiter"))?;

                    released
                        .recv_timeout(Duration::from_secs(10))
                        .map_err(io::Error::other)
                })
                .await
        });

        observed.await?;

        let mut create = Box::pin(
            listener
                .connection()
                .create_bucket(BucketName::parse("canceled")?),
        );

        std::future::poll_fn(|cx| {
            assert!(create.as_mut().poll(cx).is_pending());
            Poll::Ready(())
        })
        .await;

        assert_eq!(
            connection.owner.state.context.sender.capacity(),
            QUEUE_CAPACITY - 1
        );

        listener.begin_close();
        let mut closing = Box::pin(listener.finish());

        std::future::poll_fn(|cx| {
            assert!(closing.as_mut().poll(cx).is_pending());
            Poll::Ready(())
        })
        .await;

        assert!(matches!(
            connection.reserve_listener(),
            Err(ConnectionError::ListenerAlreadyRunning)
        ));

        // Direct admission remains open while the scoped command is queued.
        drop(connection.admit()?);
        release.send(())?;
        blocker.await??;
        let error = create.await.unwrap_err();
        assert_eq!(error.failure(), OperationFailure::OperationCanceled);
        assert_eq!(error.outcome(), Some(MutationOutcome::NotCommitted));
        closing.await?;
        drop(listener);

        assert_eq!(
            connection
                .head_bucket(BucketName::parse("canceled")?, None)
                .await
                .unwrap_err()
                .failure(),
            OperationFailure::NoSuchBucket
        );

        let replacement = connection.reserve_listener()?;
        replacement.ready()?;
        replacement.finish().await?;
        drop(replacement);

        connection
            .create_bucket(BucketName::parse("direct")?)
            .await?;

        connection.close().await?;
        Ok::<_, Box<dyn std::error::Error>>(())
    })?;

    directory.finish()?;
    Ok(())
}

#[test]
fn entered_listener_commit_retains_ownership_and_last_store_drop_fences_children() -> TestResult {
    let directory = TestDirectory::new()?;

    runtime()?.block_on(async {
        let path = directory.path().join("store");
        let connection = Connection::open(path.clone(), None).await?;
        let peer = Connection::open(path, None).await?;
        let listener = connection.reserve_listener()?;
        let commit = listener.connection().admit()?.enter_mutation()?;
        let scoped_alias = listener.connection().clone();
        drop(connection);
        listener.closing().await;

        assert!(matches!(
            scoped_alias.admit(),
            Err(ConnectionError::StoreClosing)
        ));

        listener.begin_close();
        let mut finish = Box::pin(listener.finish());

        std::future::poll_fn(|cx| {
            assert!(finish.as_mut().poll(cx).is_pending());
            Poll::Ready(())
        })
        .await;

        drop(commit);
        finish.await?;
        drop(scoped_alias);
        drop(listener);

        peer.create_bucket(BucketName::parse("independent")?)
            .await?;

        peer.close().await?;
        Ok::<_, Box<dyn std::error::Error>>(())
    })?;

    directory.finish()?;
    Ok(())
}

#[test]
fn repeated_old_listener_finish_cannot_release_a_replacement_slot() -> TestResult {
    let directory = TestDirectory::new()?.retain_on_failure();

    runtime()?.block_on(async {
        let path = directory.path().join("store");
        let connection = Connection::open(path.clone(), None).await?;
        connection.pause_maintenance();
        let old = connection.reserve_listener()?;
        old.ready()?;
        old.finish().await?;

        let replacement = connection.reserve_listener()?;
        replacement.ready()?;
        old.finish().await?;

        assert!(matches!(
            connection.reserve_listener(),
            Err(ConnectionError::ListenerAlreadyRunning)
        ));

        let error = old
            .connection()
            .create_bucket(BucketName::parse("old-listener")?)
            .await
            .unwrap_err();

        // This new call is refused at admission after the listener fence.
        assert_eq!(error.failure(), OperationFailure::StoreClosing);
        assert_eq!(error.outcome(), Some(MutationOutcome::NotCommitted));

        replacement
            .connection()
            .create_bucket(BucketName::parse("replacement")?)
            .await?;

        replacement.ready()?;
        replacement.finish().await?;

        // Both retained old leases may finish again without stealing this slot.
        let latest = connection.reserve_listener()?;
        latest.ready()?;
        old.finish().await?;
        replacement.finish().await?;

        assert!(matches!(
            connection.reserve_listener(),
            Err(ConnectionError::ListenerAlreadyRunning)
        ));

        latest.ready()?;
        latest.finish().await?;
        drop(latest);
        drop(replacement);
        drop(old);
        connection.close().await?;

        let reopened = Connection::open(path, None).await?;

        reopened
            .head_bucket(BucketName::parse("replacement")?, None)
            .await?;

        assert_eq!(
            reopened
                .head_bucket(BucketName::parse("old-listener")?, None)
                .await
                .unwrap_err()
                .failure(),
            OperationFailure::NoSuchBucket
        );

        reopened.close().await?;
        Ok::<_, Box<dyn std::error::Error>>(())
    })?;

    directory.finish()?;
    Ok(())
}

async fn observe_parent_close(connection: &Connection, wait_first: bool) -> TestResult {
    let listener = connection.reserve_listener()?;
    listener.ready()?;
    let mut observation = Box::pin(listener.connection_closing());

    if wait_first {
        std::future::poll_fn(|context| {
            assert!(observation.as_mut().poll(context).is_pending());
            Poll::Ready(())
        })
        .await;
    }

    let mut close = Box::pin(connection.close());

    std::future::poll_fn(|context| {
        assert!(close.as_mut().poll(context).is_pending());
        Poll::Ready(())
    })
    .await;

    assert!(matches!(
        connection.reserve_listener(),
        Err(ConnectionError::StoreClosing)
    ));

    // A completed parent fence must be visible whether subscription preceded
    // or followed it. No second notification or timer is required.
    std::future::poll_fn(|context| {
        assert!(observation.as_mut().poll(context).is_ready());
        Poll::Ready(())
    })
    .await;

    drop(observation);
    listener.finish().await?;
    drop(listener);
    close.await?;

    assert!(matches!(
        connection.reserve_listener(),
        Err(ConnectionError::StoreClosed)
    ));

    Ok(())
}

fn parent_close_scenario(wait_first: bool) -> TestResult {
    let directory = TestDirectory::new()?.retain_on_failure();

    runtime()?.block_on(async {
        let path = directory.path().join("store");
        let connection = Connection::open(path.clone(), None).await?;
        connection.pause_maintenance();
        let peer = Connection::open(path, None).await?;
        peer.pause_maintenance();
        observe_parent_close(&connection, wait_first).await?;

        peer.create_bucket(BucketName::parse("independent")?)
            .await?;

        peer.close().await?;
        Ok::<_, Box<dyn std::error::Error>>(())
    })?;

    directory.finish()?;
    Ok(())
}

#[test]
fn parent_close_is_observed_by_an_already_waiting_listener() -> TestResult {
    parent_close_scenario(true)
}

#[test]
fn parent_close_is_observed_by_a_listener_that_starts_waiting_later() -> TestResult {
    parent_close_scenario(false)
}
