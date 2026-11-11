use super::*;
use crate::access::{AdminPageSize, PrincipalName};
use crate::{MutationOutcome, OperationFailure};

#[test]
fn concurrent_identity_policy_writers_expose_only_whole_documents_and_reopen() -> TestResult {
    const READ_POLICY: &str = r#"{ "Version":"2012-10-17", "Statement":{"Effect":"Allow","Action":"s3:GetObject","Resource":"arn:aws:s3:::photos/read/*"}}"#;
    const WRITE_POLICY: &str = r#"{"Version":"2012-10-17","Statement":[{"Effect":"Deny","Action":"s3:PutObject","Resource":"arn:aws:s3:::photos/write/*"},{"Effect":"Allow","Action":"s3:GetObject","Resource":"arn:aws:s3:::photos/other/*"}]}"#;
    let directory = TestDirectory::new()?.retain_on_failure();

    runtime()?.block_on(async {
        let path = directory.path().join("store");
        let writer = Connection::open(path.clone(), None).await?;
        let peer = Connection::open(path.clone(), None).await?;
        let reader = Connection::open(path.clone(), None).await?;

        let principal = writer
            .create_principal(PrincipalName::new("policy-writers".into())?)
            .await?;

        let principal_id = principal.principal_id;

        writer
            .put_principal_policy(principal_id, READ_POLICY.into())
            .await?;

        for round in 0..32 {
            let replace = |connection: Connection, document: &'static str| {
                tokio::spawn(async move {
                    connection
                        .put_principal_policy(principal_id, document.into())
                        .await
                })
            };

            let written = replace(writer.clone(), READ_POLICY);
            let replaced = replace(peer.clone(), WRITE_POLICY);
            let observed = reader.get_principal_policy(principal_id).await;

            written.await??;
            replaced.await??;
            let observed = observed?.document;

            assert!(
                observed == READ_POLICY || observed == WRITE_POLICY,
                "round {round}: reader saw a partial policy"
            );

            let settled = reader.get_principal_policy(principal_id).await?.document;

            assert!(
                settled == READ_POLICY || settled == WRITE_POLICY,
                "round {round}: writers settled a partial policy"
            );
        }

        let final_document = reader.get_principal_policy(principal_id).await?.document;
        writer.close().await?;
        peer.close().await?;
        reader.close().await?;
        let reopened = Connection::open(path, None).await?;

        assert_eq!(
            reopened.get_principal_policy(principal_id).await?.document,
            final_document
        );

        reopened.close().await?;
        Ok::<_, Box<dyn std::error::Error>>(())
    })?;

    directory.finish()?;
    Ok(())
}

#[test]
fn canceled_queued_administration_cannot_create_an_identity() -> TestResult {
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

        let mut create =
            Box::pin(connection.create_principal(PrincipalName::new("abandoned".into())?));

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

        let inventory = connection
            .list_principals(AdminPageSize::default(), None)
            .await?;

        assert_eq!(inventory.principals, []);
        assert_eq!(inventory.next_continuation_token, None);

        connection
            .create_principal(PrincipalName::new("abandoned".into())?)
            .await?;

        connection.close().await?;
        Ok::<_, Box<dyn std::error::Error>>(())
    })?;

    directory.finish()?;
    Ok(())
}

#[test]
fn close_before_key_creation_rejects_mutation_and_peer_can_delete_principal() -> TestResult {
    let directory = TestDirectory::new()?;

    runtime()?.block_on(async {
        let path = directory.path().join("store");
        let connection = Connection::open(path.clone(), None).await?;
        let peer = Connection::open(path, None).await?;
        peer.pause_maintenance();

        let principal = connection
            .create_principal(PrincipalName::new("app".into())?)
            .await?;

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
        let queued_capacity = connection.owner.state.context.sender.capacity();
        let mut create = Box::pin(connection.create_access_key(principal.principal_id));

        std::future::poll_fn(|context| {
            assert!(create.as_mut().poll(context).is_pending());
            Poll::Ready(())
        })
        .await;

        assert_eq!(
            connection.owner.state.context.sender.capacity(),
            queued_capacity - 1
        );

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
            peer.list_access_keys(principal.principal_id, AdminPageSize::default(), None)
                .await?
                .access_keys,
            []
        );

        peer.delete_principal(principal.principal_id).await?;
        peer.close().await?;
        Ok::<_, Box<dyn std::error::Error>>(())
    })?;

    directory.finish()?;
    Ok(())
}
