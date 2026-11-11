use super::*;
use crate::cors::{CorsConfiguration, CorsRule};

fn configuration(origin: &str, seconds: i32) -> CorsConfiguration {
    CorsConfiguration::new(vec![CorsRule {
        id: Some(origin.into()),
        allowed_origins: vec![origin.into()],
        allowed_methods: vec!["GET".into(), "HEAD".into()],
        allowed_headers: None,
        expose_headers: Some(vec!["ETag".into()]),
        max_age_seconds: Some(seconds),
    }])
    .unwrap()
}

#[test]
fn abandoned_and_close_fenced_cors_mutations_preserve_the_complete_predecessor() -> TestResult {
    let directory = TestDirectory::new()?.retain_on_failure();

    runtime()?.block_on(async {
        let path = directory.path().join("store");
        let peer = Connection::open(path.clone(), None).await?;
        peer.pause_maintenance();

        let request = BucketAccessRequest {
            bucket: BucketName::parse("photos")?,
            expected_owner: None,
        };

        peer.create_bucket(request.bucket.clone()).await?;
        let original = configuration("https://before.example", i32::MIN);

        peer.put_bucket_cors(request.clone(), original.clone())
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
                Box::pin(connection.delete_bucket_cors(request.clone()))
            } else {
                Box::pin(connection.put_bucket_cors(
                    request.clone(),
                    configuration("https://after.example", i32::MAX),
                ))
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
                assert_eq!(connection.get_bucket_cors(request.clone()).await?, original);
                connection.close().await?;
            }

            assert_eq!(peer.get_bucket_cors(request.clone()).await?, original);
        }

        peer.close().await?;
        let reopened = Connection::open(path, None).await?;
        assert_eq!(reopened.get_bucket_cors(request).await?, original);
        reopened.close().await?;
        Ok::<_, Box<dyn std::error::Error>>(())
    })?;

    directory.finish()?;
    Ok(())
}

#[test]
fn concurrent_cors_replacements_return_whole_values_across_independent_connections() -> TestResult {
    let directory = TestDirectory::new()?.retain_on_failure();

    runtime()?.block_on(async {
        let path = directory.path().join("store");
        let first = Connection::open(path.clone(), None).await?;
        let second = Connection::open(path.clone(), None).await?;
        let reader = Connection::open(path.clone(), None).await?;

        let request = BucketAccessRequest {
            bucket: BucketName::parse("photos")?,
            expected_owner: None,
        };

        first.create_bucket(request.bucket.clone()).await?;
        let left = configuration("https://left.example", i32::MIN);
        let right = configuration("https://right.example", i32::MAX);
        first.put_bucket_cors(request.clone(), left.clone()).await?;

        let replace = |connection: Connection, configuration: CorsConfiguration| {
            let request = request.clone();

            tokio::spawn(async move {
                for _ in 0..32 {
                    connection
                        .put_bucket_cors(request.clone(), configuration.clone())
                        .await?;
                }

                Ok::<_, crate::OperationError>(())
            })
        };

        let left_writer = replace(first.clone(), left.clone());
        let right_writer = replace(second.clone(), right.clone());

        for _ in 0..64 {
            let snapshot = reader.get_bucket_cors(request.clone()).await?;
            assert!(snapshot == left || snapshot == right);
        }

        left_writer.await??;
        right_writer.await??;
        first.close().await?;
        second.close().await?;
        reader.close().await?;
        let reopened = Connection::open(path, None).await?;
        let snapshot = reopened.get_bucket_cors(request).await?;
        assert!(snapshot == left || snapshot == right);
        reopened.close().await?;
        Ok::<_, Box<dyn std::error::Error>>(())
    })?;

    directory.finish()?;
    Ok(())
}
