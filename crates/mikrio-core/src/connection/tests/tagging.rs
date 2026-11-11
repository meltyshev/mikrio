//! Whole-set visibility and commit-fence ownership over the real tag worker.

use super::objects::{put, read};
use super::*;
use crate::object::ObjectTags;
use crate::{MutationOutcome, OperationFailure};

fn tags(value: &str) -> ObjectTags {
    ObjectTags::new((0..10).map(|index| (format!("key{index}"), value.into()))).unwrap()
}

#[test]
fn corrupt_persisted_tags_refuse_reads_without_defaulting_or_repairing_state() -> TestResult {
    let directory = TestDirectory::new()?.retain_on_failure();
    let path = directory.path().join("store");

    runtime()?.block_on(async {
        let connection = Connection::open(path.clone(), None).await?;
        connection.pause_maintenance();
        connection.create_bucket(read("tagged").bucket).await?;
        let original_tags = tags("private");
        let mut input = put("tagged");
        input.tags = Some(original_tags.clone());
        connection.put_object(input, b"payload".as_slice()).await?;
        let original = connection.head_object(read("tagged")).await?;
        connection.close().await?;

        for invalid in [
            r#"{"visibility":"private","visibility":"public"}"#,
            r#"{"visibility":true}"#,
            r#"{"aws:visibility":"public"}"#,
        ] {
            // Only the owned closed store is changed. Valid JSON with invalid
            // tag semantics must not become an empty or partially decoded set.
            let database = rusqlite::Connection::open(path.join("metadata.sqlite3"))?;

            assert_eq!(
                database.execute(
                    "UPDATE objects SET tags_json = ?1 WHERE key = ?2",
                    rusqlite::params![invalid, b"tagged"],
                )?,
                1
            );

            database.close().map_err(|(_, error)| error)?;
            let connection = Connection::open(path.clone(), None).await?;
            connection.pause_maintenance();

            assert_eq!(
                connection
                    .get_object_tags(read("tagged"))
                    .await
                    .unwrap_err()
                    .failure(),
                OperationFailure::StoreCorrupt
            );

            assert_eq!(
                connection
                    .head_object(read("tagged"))
                    .await
                    .unwrap_err()
                    .failure(),
                OperationFailure::StoreCorrupt
            );

            assert_eq!(
                connection
                    .get_object(read("tagged"))
                    .await
                    .unwrap_err()
                    .failure(),
                OperationFailure::StoreCorrupt
            );

            connection.close().await?;
            let database = rusqlite::Connection::open(path.join("metadata.sqlite3"))?;

            let retained: String = database.query_row(
                "SELECT tags_json FROM objects WHERE key = ?1",
                [b"tagged"],
                |row| row.get(0),
            )?;

            assert_eq!(retained, invalid);

            assert_eq!(
                database.execute(
                    "UPDATE objects SET tags_json = ?1 WHERE key = ?2",
                    rusqlite::params![original_tags.to_json()?, b"tagged"],
                )?,
                1
            );

            database.close().map_err(|(_, error)| error)?;
            let reopened = Connection::open(path.clone(), None).await?;
            let (selected, mut body) = reopened.get_object(read("tagged")).await?;
            assert_eq!(selected.tags, original_tags);
            assert_eq!(selected.generation, original.generation);
            assert_eq!(selected.blob_id, original.blob_id);
            assert_eq!(body.read_all(64).await?, b"payload");
            body.close().await?;
            reopened.close().await?;
        }

        Ok::<_, Box<dyn std::error::Error>>(())
    })?;

    directory.finish()?;
    Ok(())
}

#[test]
fn abandoned_and_close_fenced_tag_mutations_preserve_the_complete_predecessor() -> TestResult {
    let directory = TestDirectory::new()?.retain_on_failure();

    runtime()?.block_on(async {
        let path = directory.path().join("store");
        let peer = Connection::open(path.clone(), None).await?;
        peer.pause_maintenance();
        let request = read("tagged");
        peer.create_bucket(request.bucket.clone()).await?;

        peer.put_object(put("tagged"), b"payload".as_slice())
            .await?;

        let original = tags("before");

        peer.replace_object_tags(request.clone(), original.clone())
            .await?;

        for (delete, close_connection) in
            [(false, false), (true, false), (false, true), (true, true)]
        {
            let connection = Connection::open(path.clone(), None).await?;
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
            let capacity = connection.owner.state.context.sender.capacity();

            let mut change: std::pin::Pin<
                Box<dyn Future<Output = Result<(), crate::OperationError>>>,
            > = if delete {
                Box::pin(connection.delete_object_tags(request.clone()))
            } else {
                Box::pin(connection.replace_object_tags(request.clone(), tags("after")))
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

                assert_eq!(
                    connection.owner.state.activity.lock().unwrap().operations,
                    1
                );

                release.send(())?;
                blocker.await??;
                // This read is queued after the abandoned command has been observed.
                assert_eq!(connection.get_object_tags(request.clone()).await?, original);
                connection.close().await?;
            }

            assert_eq!(peer.get_object_tags(request.clone()).await?, original);
        }

        peer.close().await?;
        let reopened = Connection::open(path, None).await?;
        assert_eq!(reopened.get_object_tags(request).await?, original);
        reopened.close().await?;
        Ok::<_, Box<dyn std::error::Error>>(())
    })?;

    directory.finish()?;
    Ok(())
}

#[test]
fn concurrent_tag_replacements_return_whole_sets_across_connections_and_reopen() -> TestResult {
    let directory = TestDirectory::new()?.retain_on_failure();

    runtime()?.block_on(async {
        let path = directory.path().join("store");
        let first = Connection::open(path.clone(), None).await?;
        let second = Connection::open(path.clone(), None).await?;
        let reader = Connection::open(path.clone(), None).await?;
        let request = read("tagged");
        first.create_bucket(request.bucket.clone()).await?;

        first
            .put_object(put("tagged"), b"payload".as_slice())
            .await?;

        let left = tags("left");
        let right = tags("right");

        first
            .replace_object_tags(request.clone(), left.clone())
            .await?;

        let replace = |connection: Connection, tags: ObjectTags| {
            let request = request.clone();

            tokio::spawn(async move {
                for _ in 0..16 {
                    connection
                        .replace_object_tags(request.clone(), tags.clone())
                        .await?;
                }

                Ok::<_, crate::OperationError>(())
            })
        };

        let left_writer = replace(first.clone(), left.clone());
        let right_writer = replace(second.clone(), right.clone());

        for _ in 0..32 {
            let snapshot = reader.get_object_tags(request.clone()).await?;
            assert!(snapshot == left || snapshot == right);
        }

        left_writer.await??;
        right_writer.await??;
        first.close().await?;
        second.close().await?;
        reader.close().await?;
        let reopened = Connection::open(path, None).await?;
        let snapshot = reopened.get_object_tags(request).await?;
        assert!(snapshot == left || snapshot == right);
        reopened.close().await?;
        Ok::<_, Box<dyn std::error::Error>>(())
    })?;

    directory.finish()?;
    Ok(())
}
