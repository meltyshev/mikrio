//! Typed bucket commands keep SQL and settlement on the owning worker.

use std::sync::Arc;
use tokio::sync::oneshot;

use super::{ADMISSION_TIMEOUT, Command as WorkerCommand, Connection, Operation, mutation_error};
use crate::access::HttpIdentity;
use crate::object::{Authority, BucketName};
use crate::{MutationOutcome, OpenedStore, OperationError, OperationFailure};

pub(super) enum Command {
    Create {
        operation: Operation,
        authority: Authority,
        bucket: BucketName,
        reply: oneshot::Sender<Result<(), OperationError>>,
    },
    Head {
        _operation: Operation,
        authority: Authority,
        action: &'static str,
        bucket: BucketName,
        expected_owner: Option<String>,
        reply: oneshot::Sender<Result<String, OperationError>>,
    },
    Delete {
        operation: Operation,
        authority: Authority,
        bucket: BucketName,
        expected_owner: Option<String>,
        reply: oneshot::Sender<Result<(), OperationError>>,
    },
}

impl Connection {
    /// Create a trusted-native bucket, preserving regional repeated-create rules.
    ///
    /// # Errors
    /// Returns an authoritative mutation outcome with namespace/lifecycle failures.
    pub async fn create_bucket(&self, bucket: BucketName) -> Result<(), OperationError> {
        self.create(bucket, Authority::Native).await
    }

    /// Authorize the prospective bucket ARN and create in the same transaction.
    ///
    /// # Errors
    /// Current identity denial has no namespace effect; mutation outcomes persist.
    pub async fn create_bucket_http(
        &self,
        identity: Arc<HttpIdentity>,
        bucket: BucketName,
    ) -> Result<(), OperationError> {
        self.create(bucket, Authority::Http(identity)).await
    }

    async fn create(&self, bucket: BucketName, authority: Authority) -> Result<(), OperationError> {
        let operation = self
            .admit()
            .map_err(|error| OperationError::from(error).uncommitted())?;

        let _cancellation = operation.cancel_on_drop();
        let (reply, response) = oneshot::channel();

        self.queue_bucket(Command::Create {
            operation,
            authority,
            bucket,
            reply,
        })
        .await
        .map_err(OperationError::uncommitted)?;

        response.await.map_err(|_| lost_mutation())?
    }

    /// Resolve a live trusted-native bucket and its canonical region.
    ///
    /// # Errors
    /// Preserves missing, mismatched owner, corruption and lifecycle failures.
    pub async fn head_bucket(
        &self,
        bucket: BucketName,
        expected_owner: Option<String>,
    ) -> Result<String, OperationError> {
        self.head(bucket, expected_owner, Authority::Native, "s3:ListBucket")
            .await
    }

    /// Authorize `HeadBucket` from a current access/namespace snapshot.
    ///
    /// # Errors
    /// Preserves denial, missing namespace and corrupt/unavailable state.
    pub async fn head_bucket_http(
        &self,
        identity: Arc<HttpIdentity>,
        bucket: BucketName,
        expected_owner: Option<String>,
    ) -> Result<String, OperationError> {
        self.head(
            bucket,
            expected_owner,
            Authority::Http(identity),
            "s3:ListBucket",
        )
        .await
    }

    /// Authorize the distinct `GetBucketLocation` permission in the read snapshot.
    ///
    /// # Errors
    /// Preserves denial, missing namespace and corrupt/unavailable state.
    pub async fn get_bucket_location_http(
        &self,
        identity: Arc<HttpIdentity>,
        bucket: BucketName,
        expected_owner: Option<String>,
    ) -> Result<String, OperationError> {
        self.head(
            bucket,
            expected_owner,
            Authority::Http(identity),
            "s3:GetBucketLocation",
        )
        .await
    }

    async fn head(
        &self,
        bucket: BucketName,
        expected_owner: Option<String>,
        authority: Authority,
        action: &'static str,
    ) -> Result<String, OperationError> {
        let operation = self.admit()?;
        let (reply, response) = oneshot::channel();

        self.queue_bucket(Command::Head {
            _operation: operation,
            authority,
            action,
            bucket,
            expected_owner,
            reply,
        })
        .await?;

        response
            .await
            .map_err(|_| OperationError::new(OperationFailure::IoError))?
    }

    /// Retire an empty namespace without removing payloads or scanning uploads.
    ///
    /// # Errors
    /// Preserves nonempty/missing/owner failures and authoritative mutation outcomes.
    pub async fn delete_bucket(
        &self,
        bucket: BucketName,
        expected_owner: Option<String>,
    ) -> Result<(), OperationError> {
        self.delete_bucket_with(bucket, expected_owner, Authority::Native)
            .await
    }

    /// Authorize and retire the current empty namespace in one transaction.
    ///
    /// # Errors
    /// Preserves denial, nonempty/missing buckets and authoritative outcomes.
    pub async fn delete_bucket_http(
        &self,
        identity: Arc<HttpIdentity>,
        bucket: BucketName,
        expected_owner: Option<String>,
    ) -> Result<(), OperationError> {
        self.delete_bucket_with(bucket, expected_owner, Authority::Http(identity))
            .await
    }

    async fn delete_bucket_with(
        &self,
        bucket: BucketName,
        expected_owner: Option<String>,
        authority: Authority,
    ) -> Result<(), OperationError> {
        let operation = self
            .admit()
            .map_err(|error| OperationError::from(error).uncommitted())?;

        let _cancellation = operation.cancel_on_drop();
        let (reply, response) = oneshot::channel();

        self.queue_bucket(Command::Delete {
            operation,
            authority,
            bucket,
            expected_owner,
            reply,
        })
        .await
        .map_err(OperationError::uncommitted)?;

        response.await.map_err(|_| lost_mutation())?
    }

    async fn queue_bucket(&self, command: Command) -> Result<(), OperationError> {
        let permit =
            tokio::time::timeout(ADMISSION_TIMEOUT, self.owner.state.context.sender.reserve())
                .await
                .map_err(|_| OperationError::new(OperationFailure::StoreBusy))?
                .map_err(|_| OperationError::new(OperationFailure::IoError))?;

        permit.send(WorkerCommand::Bucket(command));
        Ok(())
    }
}

fn lost_mutation() -> OperationError {
    // A dead worker/reply channel is not proof that COMMIT did not take effect.
    OperationError::new(OperationFailure::IoError).mutation(MutationOutcome::Unknown)
}

pub(super) fn run(command: Command, store: Option<&mut OpenedStore>) {
    match command {
        Command::Create {
            operation,
            authority,
            bucket,
            reply,
        } => {
            let result = (|| {
                let _commit = operation
                    .enter_mutation()
                    .map_err(OperationError::uncommitted)?;

                let store = store
                    .ok_or_else(|| OperationError::new(OperationFailure::IoError).uncommitted())?;

                match authority {
                    Authority::Native => store.database_mut().create_bucket(&bucket),
                    Authority::Http(_) => store
                        .database_mut()
                        .create_bucket_authorized(&authority, &bucket),
                }
            })();

            let _delivery = reply.send(result.map_err(mutation_error));
        }
        Command::Head {
            _operation,
            authority,
            action,
            bucket,
            expected_owner,
            reply,
        } => {
            let result = store
                .ok_or_else(|| OperationError::new(OperationFailure::IoError))
                .and_then(|store| match authority {
                    Authority::Native => store
                        .database_mut()
                        .head_bucket(&bucket, expected_owner.as_deref()),
                    Authority::Http(_) => store.database_mut().head_bucket_authorized(
                        &authority,
                        &bucket,
                        expected_owner.as_deref(),
                        action,
                    ),
                });

            let _delivery = reply.send(result);
        }
        Command::Delete {
            operation,
            authority,
            bucket,
            expected_owner,
            reply,
        } => {
            let result = (|| {
                let _commit = operation
                    .enter_mutation()
                    .map_err(OperationError::uncommitted)?;

                let store = store
                    .ok_or_else(|| OperationError::new(OperationFailure::IoError).uncommitted())?;

                match authority {
                    Authority::Native => store
                        .database_mut()
                        .delete_bucket(&bucket, expected_owner.as_deref()),
                    Authority::Http(_) => store.database_mut().delete_bucket_authorized(
                        &authority,
                        &bucket,
                        expected_owner.as_deref(),
                    ),
                }
            })();

            let _delivery = reply.send(result.map_err(mutation_error));
        }
    }
}
