//! Lifecycle configuration commands keep admission and commit ownership on the worker.

use std::sync::Arc;
use tokio::sync::oneshot;

use super::{ADMISSION_TIMEOUT, Command as WorkerCommand, Connection, Operation, mutation_error};
use crate::access::{BucketAccessRequest, HttpIdentity};
use crate::lifecycle::LifecycleConfiguration;
use crate::object::Authority;
use crate::{MutationOutcome, OpenedStore, OperationError, OperationFailure};

pub(super) enum Command {
    Put {
        operation: Operation,
        authority: Authority,
        request: BucketAccessRequest,
        configuration: LifecycleConfiguration,
        reply: oneshot::Sender<Result<(), OperationError>>,
    },
    Get {
        operation: Operation,
        authority: Authority,
        request: BucketAccessRequest,
        reply: oneshot::Sender<Result<Option<LifecycleConfiguration>, OperationError>>,
    },
    Delete {
        operation: Operation,
        authority: Authority,
        request: BucketAccessRequest,
        reply: oneshot::Sender<Result<(), OperationError>>,
    },
}

impl Connection {
    /// Execute trusted native `PutBucketLifecycleConfiguration` on the bounded metadata worker.
    ///
    /// # Errors
    /// Preserves missing buckets, owner/access denial, corrupt state and
    /// lifecycle/storage failures; mutation errors retain their settlement outcome.
    pub async fn put_bucket_lifecycle(
        &self,
        request: BucketAccessRequest,
        configuration: LifecycleConfiguration,
    ) -> Result<(), OperationError> {
        self.put_bucket_lifecycle_with(Authority::Native, request, configuration)
            .await
    }

    /// Authorize current HTTP state and execute `PutBucketLifecycleConfiguration`
    /// on the bounded metadata worker.
    ///
    /// # Errors
    /// Preserves missing buckets, owner/access denial, corrupt state and
    /// lifecycle/storage failures; mutation errors retain their settlement outcome.
    pub async fn put_bucket_lifecycle_http(
        &self,
        identity: Arc<HttpIdentity>,
        request: BucketAccessRequest,
        configuration: LifecycleConfiguration,
    ) -> Result<(), OperationError> {
        self.put_bucket_lifecycle_with(Authority::Http(identity), request, configuration)
            .await
    }

    async fn put_bucket_lifecycle_with(
        &self,
        authority: Authority,
        request: BucketAccessRequest,
        configuration: LifecycleConfiguration,
    ) -> Result<(), OperationError> {
        let operation = self
            .admit()
            .map_err(|error| OperationError::from(error).uncommitted())?;

        let _cancellation = operation.cancel_on_drop();
        let (reply, response) = oneshot::channel();

        self.queue_lifecycle(Command::Put {
            operation,
            authority,
            request,
            configuration,
            reply,
        })
        .await
        .map_err(OperationError::uncommitted)?;

        response.await.map_err(|_| {
            OperationError::new(OperationFailure::IoError).mutation(MutationOutcome::Unknown)
        })?
    }

    /// Read trusted native lifecycle state on the bounded metadata worker.
    /// Absence is explicit; a disabled configuration remains present.
    ///
    /// # Errors
    /// Preserves missing buckets, owner/access denial, corrupt state and
    /// lifecycle/storage failures.
    pub async fn get_bucket_lifecycle(
        &self,
        request: BucketAccessRequest,
    ) -> Result<Option<LifecycleConfiguration>, OperationError> {
        self.get_bucket_lifecycle_with(Authority::Native, request)
            .await
    }

    /// Authorize current HTTP state and execute `GetBucketLifecycleConfiguration`
    /// on the bounded metadata worker.
    ///
    /// # Errors
    /// Preserves missing buckets, owner/access denial, corrupt state and
    /// lifecycle/storage failures.
    pub async fn get_bucket_lifecycle_http(
        &self,
        identity: Arc<HttpIdentity>,
        request: BucketAccessRequest,
    ) -> Result<Option<LifecycleConfiguration>, OperationError> {
        self.get_bucket_lifecycle_with(Authority::Http(identity), request)
            .await
    }

    async fn get_bucket_lifecycle_with(
        &self,
        authority: Authority,
        request: BucketAccessRequest,
    ) -> Result<Option<LifecycleConfiguration>, OperationError> {
        let operation = self.admit()?;
        let (reply, response) = oneshot::channel();

        self.queue_lifecycle(Command::Get {
            operation,
            authority,
            request,
            reply,
        })
        .await?;

        response
            .await
            .map_err(|_| OperationError::new(OperationFailure::IoError))?
    }

    /// Execute trusted native `DeleteBucketLifecycle` on the bounded metadata worker.
    ///
    /// # Errors
    /// Preserves missing buckets, owner/access denial, corrupt state and
    /// lifecycle/storage failures; mutation errors retain their settlement outcome.
    pub async fn delete_bucket_lifecycle(
        &self,
        request: BucketAccessRequest,
    ) -> Result<(), OperationError> {
        self.delete_bucket_lifecycle_with(Authority::Native, request)
            .await
    }

    /// Authorize current HTTP state and execute `DeleteBucketLifecycle` on the
    /// bounded metadata worker.
    ///
    /// # Errors
    /// Preserves missing buckets, owner/access denial, corrupt state and
    /// lifecycle/storage failures; mutation errors retain their settlement outcome.
    pub async fn delete_bucket_lifecycle_http(
        &self,
        identity: Arc<HttpIdentity>,
        request: BucketAccessRequest,
    ) -> Result<(), OperationError> {
        self.delete_bucket_lifecycle_with(Authority::Http(identity), request)
            .await
    }

    async fn delete_bucket_lifecycle_with(
        &self,
        authority: Authority,
        request: BucketAccessRequest,
    ) -> Result<(), OperationError> {
        let operation = self
            .admit()
            .map_err(|error| OperationError::from(error).uncommitted())?;

        let _cancellation = operation.cancel_on_drop();
        let (reply, response) = oneshot::channel();

        self.queue_lifecycle(Command::Delete {
            operation,
            authority,
            request,
            reply,
        })
        .await
        .map_err(OperationError::uncommitted)?;

        response.await.map_err(|_| {
            OperationError::new(OperationFailure::IoError).mutation(MutationOutcome::Unknown)
        })?
    }

    async fn queue_lifecycle(&self, command: Command) -> Result<(), OperationError> {
        let permit =
            tokio::time::timeout(ADMISSION_TIMEOUT, self.owner.state.context.sender.reserve())
                .await
                .map_err(|_| OperationError::new(OperationFailure::StoreBusy))?
                .map_err(|_| OperationError::new(OperationFailure::IoError))?;

        permit.send(WorkerCommand::Lifecycle(command));
        Ok(())
    }
}

pub(super) fn run(command: Command, store: Option<&mut OpenedStore>) {
    match command {
        Command::Put {
            operation,
            authority,
            request,
            configuration,
            reply,
        } => {
            let wake = Arc::clone(&operation.state.context.wake_maintenance);

            let result = (|| {
                let _commit = operation
                    .enter_mutation()
                    .map_err(OperationError::uncommitted)?;

                let store = store.ok_or_else(|| OperationError::new(OperationFailure::IoError))?;

                store
                    .database_mut()
                    .put_bucket_lifecycle(&authority, &request, &configuration)
            })();

            wake.notify_one();
            let _delivery = reply.send(result.map_err(mutation_error));
        }
        Command::Get {
            operation,
            authority,
            request,
            reply,
        } => {
            let result = (|| {
                let _operation = operation;
                let store = store.ok_or_else(|| OperationError::new(OperationFailure::IoError))?;

                store
                    .database_mut()
                    .get_bucket_lifecycle(&authority, &request)
            })();

            let _delivery = reply.send(result);
        }
        Command::Delete {
            operation,
            authority,
            request,
            reply,
        } => {
            let wake = Arc::clone(&operation.state.context.wake_maintenance);

            let result = (|| {
                let _commit = operation
                    .enter_mutation()
                    .map_err(OperationError::uncommitted)?;

                let store = store.ok_or_else(|| OperationError::new(OperationFailure::IoError))?;

                store
                    .database_mut()
                    .delete_bucket_lifecycle(&authority, &request)
            })();

            wake.notify_one();
            let _delivery = reply.send(result.map_err(mutation_error));
        }
    }
}
