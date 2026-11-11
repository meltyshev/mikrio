//! Batch deletion dispatches and settles one ordinary transaction at a time.

use std::future::{Future, poll_fn};
use std::pin::pin;
use std::sync::Arc;
use tokio::sync::oneshot;

use super::{ADMISSION_TIMEOUT, Command as WorkerCommand, Connection, Operation, objects};
use crate::access::HttpIdentity;
use crate::object::{
    Authority, DeleteObjectProgress, DeleteObjectResult, DeleteObjectsError, DeleteObjectsRequest,
    ReadConditions, ReadRequest, Representation,
};
use crate::{MutationOutcome, OpenedStore, OperationError, OperationFailure};

pub(super) enum Command {
    Prepare {
        operation: Arc<Operation>,
        authority: Authority,
        request: Arc<DeleteObjectsRequest>,
        reply: oneshot::Sender<Result<(), OperationError>>,
    },
    Item(Item),
}

pub(super) struct Item {
    operation: Arc<Operation>,
    authority: Authority,
    request: Arc<DeleteObjectsRequest>,
    index: usize,
    reply: oneshot::Sender<Result<(), OperationError>>,
}

impl Connection {
    /// Delete each trusted-native input in order, preserving duplicate positions.
    ///
    /// # Errors
    /// Invalid whole requests fail before dispatch. Interruption and uncertain
    /// settlement retain every input position; known item failures are results.
    pub async fn delete_objects(
        &self,
        request: DeleteObjectsRequest,
    ) -> Result<Vec<DeleteObjectResult>, DeleteObjectsError> {
        self.delete_batch(request, Authority::Native, std::future::pending())
            .await
    }

    /// Recheck HTTP identity, owner and item conditions in every transaction.
    ///
    /// # Errors
    /// Has the native batch's settlement contract; no entry authorizes another.
    pub async fn delete_objects_http(
        &self,
        identity: Arc<HttpIdentity>,
        request: DeleteObjectsRequest,
    ) -> Result<Vec<DeleteObjectResult>, DeleteObjectsError> {
        self.delete_batch(request, Authority::Http(identity), std::future::pending())
            .await
    }

    /// Signal batch cancellation while retaining its authoritative settlement.
    ///
    /// # Errors
    /// Returns the same indexed interruption context as ordinary batch deletion.
    pub async fn delete_objects_until(
        &self,
        request: DeleteObjectsRequest,
        canceled: impl Future<Output = ()> + Send,
    ) -> Result<Vec<DeleteObjectResult>, DeleteObjectsError> {
        self.delete_batch(request, Authority::Native, canceled)
            .await
    }

    async fn delete_batch(
        &self,
        request: DeleteObjectsRequest,
        authority: Authority,
        canceled: impl Future<Output = ()> + Send,
    ) -> Result<Vec<DeleteObjectResult>, DeleteObjectsError> {
        request.validate().map_err(OperationError::uncommitted)?;

        let operation = Arc::new(
            self.admit()
                .map_err(|error| OperationError::from(error).uncommitted())?,
        );

        let _cancellation = operation.cancel_on_drop();
        let cancellation = operation.cancellation.clone();
        let (reply, response) = oneshot::channel();

        // The driver owns admission until the last submitted transaction settles.
        // Dropping a caller signals cancellation, never releases native work.
        drop(tokio::spawn(async move {
            let result = delete_batch(operation, authority, request).await;
            let _delivery = reply.send(result);
        }));

        let mut response = pin!(response);
        let mut canceled = pin!(canceled);
        let mut signaled = false;

        poll_fn(|context| {
            if !signaled && canceled.as_mut().poll(context).is_ready() {
                signaled = true;
                cancellation.send_replace(true);
            }

            response.as_mut().poll(context)
        })
        .await
        .map_err(|_| {
            DeleteObjectsError::from(
                OperationError::new(OperationFailure::IoError).mutation(MutationOutcome::Unknown),
            )
        })?
    }
}

pub(super) async fn delete_batch(
    operation: Arc<Operation>,
    authority: Authority,
    request: DeleteObjectsRequest,
) -> Result<Vec<DeleteObjectResult>, DeleteObjectsError> {
    let request = Arc::new(request);
    let (reply, response) = oneshot::channel();

    submit(
        &operation,
        Command::Prepare {
            operation: Arc::clone(&operation),
            authority: authority.clone(),
            request: Arc::clone(&request),
            reply,
        },
    )
    .await?;

    response
        .await
        .map_err(|_| OperationError::new(OperationFailure::IoError).uncommitted())?
        .map_err(OperationError::uncommitted)?;

    let mut items: Vec<_> = request
        .objects
        .iter()
        .map(|object| DeleteObjectProgress {
            key: object.key.clone(),
            outcome: MutationOutcome::NotCommitted,
            failure: None,
        })
        .collect();

    for index in 0..request.objects.len() {
        let result = objects::contain(delete_item(
            Arc::clone(&operation),
            authority.clone(),
            Arc::clone(&request),
            index,
        ))
        .await;

        match result {
            Ok(()) => items[index].outcome = MutationOutcome::Committed,
            Err(error) => {
                items[index].outcome = error.outcome().unwrap_or(MutationOutcome::Unknown);
                items[index].failure = Some(error.failure());

                let must_stop = items[index].outcome != MutationOutcome::NotCommitted
                    || matches!(
                        error.failure(),
                        OperationFailure::OperationCanceled
                            | OperationFailure::StoreClosing
                            | OperationFailure::StoreClosed
                    );

                if must_stop {
                    return Err(DeleteObjectsError {
                        error,
                        items: Some(items),
                    });
                }
            }
        }

        // Other commands, including revocation, can run between entries.
        tokio::task::yield_now().await;
    }

    Ok(items
        .into_iter()
        .map(|item| DeleteObjectResult {
            key: item.key,
            failure: item.failure,
        })
        .collect())
}

async fn delete_item(
    operation: Arc<Operation>,
    authority: Authority,
    request: Arc<DeleteObjectsRequest>,
    index: usize,
) -> Result<(), OperationError> {
    let (reply, response) = oneshot::channel();

    submit(
        &operation,
        Command::Item(Item {
            operation: Arc::clone(&operation),
            authority,
            request,
            index,
            reply,
        }),
    )
    .await?;

    // Once submitted, wait for the transaction's actual settlement even if the
    // caller cancels. A lost worker reply has no attempt-specific delete witness.
    response.await.map_err(|_| {
        OperationError::new(OperationFailure::IoError).mutation(MutationOutcome::Unknown)
    })?
}

async fn submit(operation: &Operation, command: Command) -> Result<(), OperationError> {
    let permit = operation
        .interrupted(tokio::time::timeout(
            ADMISSION_TIMEOUT,
            operation.state.context.sender.reserve(),
        ))
        .await
        .map_err(OperationError::uncommitted)?
        .map_err(|_| OperationError::new(OperationFailure::StoreBusy).uncommitted())?
        .map_err(|_| OperationError::new(OperationFailure::IoError).uncommitted())?;

    permit.send(WorkerCommand::DeleteBatch(command));
    Ok(())
}

pub(super) fn run(command: Command, store: Option<&mut OpenedStore>) {
    let command = match command {
        Command::Prepare {
            operation,
            authority,
            request,
            reply,
        } => {
            let result = operation.check_commit().and_then(|()| {
                let store = store.ok_or_else(|| OperationError::new(OperationFailure::IoError))?;

                store
                    .database_mut()
                    .prepare_delete_objects(&authority, &request)
            });

            let _delivery = reply.send(result.map_err(OperationError::uncommitted));
            return;
        }
        Command::Item(command) => command,
    };

    let result = (|| {
        command
            .operation
            .check_commit()
            .map_err(OperationError::uncommitted)?;

        let store =
            store.ok_or_else(|| OperationError::new(OperationFailure::IoError).uncommitted())?;

        let object = command
            .request
            .objects
            .get(command.index)
            .ok_or_else(|| OperationError::new(OperationFailure::IoError).uncommitted())?;

        let request = ReadRequest {
            bucket: command.request.bucket.clone(),
            key: object.key.clone(),
            expected_owner: command.request.expected_owner.clone(),
            conditions: ReadConditions::default(),
            range: None,
            part_number: None,
            response: Representation::default(),
        };

        store
            .database_mut()
            .delete_object(&command.authority, &request, object.etag.as_deref())
    })();

    let _delivery = command.reply.send(result);
}
