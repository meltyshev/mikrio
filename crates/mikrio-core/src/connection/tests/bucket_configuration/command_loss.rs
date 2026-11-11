//! An admitted command must classify failure before any database is available.

use super::*;
use crate::connection::bucket_configuration::{Command, run};
use crate::object::Authority;

const REPLACEMENT: &str = r#"{"Version":"2012-10-17","Statement":{"Effect":"Deny","Principal":"*","Action":"s3:GetObject","Resource":"arn:aws:s3:::photos/*"}}"#;

#[test]
fn missing_worker_store_preserves_policy_and_classifies_uncommitted() -> TestResult {
    let directory = TestDirectory::new()?.retain_on_failure();

    runtime()?.block_on(async {
        let path = directory.path().join("store");
        let connection = Connection::open(path.clone(), None).await?;
        connection.pause_maintenance();
        let peer = Connection::open(path.clone(), None).await?;
        peer.pause_maintenance();

        let request = BucketAccessRequest {
            bucket: BucketName::parse("photos")?,
            expected_owner: None,
        };

        connection.create_bucket(request.bucket.clone()).await?;

        connection
            .put_bucket_policy(request.clone(), DENY.into())
            .await?;

        // Exercise the dispatcher's explicit absent-store boundary with a real
        // admitted operation and reply channel. The live connection supplies
        // the independent predecessor oracle; its worker is not corrupted.
        let (reply, response) = oneshot::channel();

        run(
            Command::PutBucketPolicy {
                operation: connection.admit()?,
                authority: Authority::Native,
                request: request.clone(),
                document: REPLACEMENT.into(),
                reply,
            },
            None,
        );

        let error = response.await?.unwrap_err();
        assert_eq!(error.failure(), OperationFailure::IoError);
        assert_eq!(error.outcome(), Some(MutationOutcome::NotCommitted));

        let (reply, response) = oneshot::channel();

        run(
            Command::GetBucketPolicy {
                operation: connection.admit()?,
                authority: Authority::Native,
                request: request.clone(),
                reply,
            },
            None,
        );

        let error = response.await?.unwrap_err();
        assert_eq!(error.failure(), OperationFailure::IoError);
        assert_eq!(error.outcome(), None);

        for reader in [&connection, &peer] {
            assert_eq!(
                reader.get_bucket_policy(request.clone()).await?.document,
                DENY
            );
        }

        peer.close().await?;
        connection.close().await?;
        let reopened = Connection::open(path, None).await?;
        reopened.pause_maintenance();
        assert_eq!(reopened.get_bucket_policy(request).await?.document, DENY);
        reopened.close().await?;
        Ok::<_, Box<dyn std::error::Error>>(())
    })?;

    directory.finish()?;
    Ok(())
}
