//! Queued reservations recheck real cancellation and close signals before SQL.

use super::objects::{put, read};
use super::*;
use crate::connection::objects::{Command as ObjectCommand, acquire};
use crate::object::{Authority, BucketName};
use crate::{BlobId, MutationOutcome, OperationFailure};

#[derive(Clone, Copy)]
enum Interruption {
    Caller,
    Parent,
    Listener,
}

fn refuses_queued_reservation(interruption: Interruption) -> TestResult {
    let directory = TestDirectory::new()?.retain_on_failure();

    runtime()?.block_on(async {
        let path = directory.path().join("store");
        let connection = Connection::open(path.clone(), None).await?;
        connection.pause_maintenance();

        connection
            .create_bucket(BucketName::parse("attachments")?)
            .await?;

        connection
            .put_object(put("selected"), b"acknowledged predecessor".as_slice())
            .await?;

        let predecessor = connection.head_object(read("selected")).await?;
        let peer = Connection::open(path.clone(), None).await?;
        peer.pause_maintenance();
        let listener = connection.reserve_listener()?;
        listener.ready()?;

        let operation = Arc::new(match interruption {
            Interruption::Listener => listener.connection().admit()?,
            Interruption::Caller | Interruption::Parent => connection.admit()?,
        });

        let blob_id = BlobId::from_bytes([0x35; 16]);
        assert_ne!(blob_id, predecessor.blob_id);
        let guard = acquire(Arc::clone(&operation), path.clone(), blob_id).await?;
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

        // Use the real bounded worker and admitted operation while its worker
        // is held at a deterministic barrier, before reservation SQL can run.
        let (reply, response) = oneshot::channel();

        operation
            .state
            .context
            .sender
            .reserve()
            .await?
            .send(Command::Object(ObjectCommand::Reserve {
                operation: Arc::clone(&operation),
                _guard: Arc::clone(&guard),
                authority: Authority::Native,
                request: Arc::new(put("selected")),
                blob_id,
                reply,
            }));

        assert_eq!(
            connection.owner.state.context.sender.capacity(),
            QUEUE_CAPACITY - 1
        );

        let mut closing = Box::pin(connection.close());

        match interruption {
            Interruption::Caller => drop(operation.cancel_on_drop()),
            Interruption::Parent => {
                std::future::poll_fn(|context| {
                    assert!(closing.as_mut().poll(context).is_pending());
                    Poll::Ready(())
                })
                .await;
            }
            Interruption::Listener => listener.begin_close(),
        }

        assert_eq!(
            *operation.cancellation.borrow(),
            matches!(interruption, Interruption::Caller)
        );

        assert_eq!(
            *operation.state.closing.borrow(),
            matches!(interruption, Interruption::Parent)
        );

        assert_eq!(
            operation.listener.is_some(),
            matches!(interruption, Interruption::Listener)
        );

        if let Some(scope) = &operation.listener {
            assert!(*scope.closing.borrow());
        }

        release.send(())?;
        blocker.await??;
        let error = response.await?.unwrap_err();
        assert_eq!(error.failure(), OperationFailure::OperationCanceled);
        assert_eq!(error.outcome(), Some(MutationOutcome::NotCommitted));

        let sql = rusqlite::Connection::open_with_flags(
            path.join("metadata.sqlite3"),
            rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY,
        )?;

        let reservations: i64 = sql.query_row(
            "SELECT COUNT(*) FROM blobs WHERE blob_id = ?1",
            [blob_id.as_bytes().as_slice()],
            |row| row.get(0),
        )?;

        assert_eq!(reservations, 0);
        drop(sql);

        for tree in ["objects", "staging"] {
            assert!(
                !path
                    .join(tree)
                    .join(format!("{:02x}", blob_id.shard()))
                    .join(blob_id.to_string())
                    .try_exists()?
            );
        }

        let (selected, mut body) = peer.get_object(read("selected")).await?;
        assert_eq!(selected.blob_id, predecessor.blob_id);
        assert_eq!(body.read_all(64).await?, b"acknowledged predecessor");
        drop(guard);
        drop(operation);
        listener.finish().await?;
        drop(listener);
        closing.await?;

        peer.put_object(put("selected"), b"independent peer update".as_slice())
            .await?;

        peer.close().await?;

        let reopened = Connection::open(path, None).await?;
        let (_, mut body) = reopened.get_object(read("selected")).await?;
        assert_eq!(body.read_all(64).await?, b"independent peer update");
        reopened.close().await?;
        Ok::<_, Box<dyn std::error::Error>>(())
    })?;

    directory.finish()?;
    Ok(())
}

#[test]
fn caller_cancellation_prevents_queued_reservation_and_preserves_predecessor() -> TestResult {
    refuses_queued_reservation(Interruption::Caller)
}

#[test]
fn parent_close_prevents_queued_reservation_and_preserves_peer() -> TestResult {
    refuses_queued_reservation(Interruption::Parent)
}

#[test]
fn listener_close_prevents_queued_reservation_without_closing_direct_peer() -> TestResult {
    refuses_queued_reservation(Interruption::Listener)
}
