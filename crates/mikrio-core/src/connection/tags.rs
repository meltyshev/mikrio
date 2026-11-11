//! Tag commands share current authorization, object generation and settlement.

use std::sync::Arc;

use tokio::sync::oneshot;

use super::{ADMISSION_TIMEOUT, Command as WorkerCommand, Operation, mutation_error};
use crate::access::HttpIdentity;
use crate::object::{Authority, ObjectTags, ReadRequest};
use crate::{Connection, MutationOutcome, OpenedStore, OperationError, OperationFailure};

pub(super) enum Command {
    Get {
        _operation: Operation,
        authority: Authority,
        request: ReadRequest,
        reply: oneshot::Sender<Result<ObjectTags, OperationError>>,
    },
    Change {
        operation: Operation,
        authority: Authority,
        request: ReadRequest,
        tags: Option<ObjectTags>,
        reply: oneshot::Sender<Result<(), OperationError>>,
    },
}

impl Connection {
    /// Read the current whole tag set in the same snapshot as its object.
    ///
    /// # Errors
    /// Reports missing targets, owner mismatch, lifecycle and corrupt storage.
    pub async fn get_object_tags(
        &self,
        request: ReadRequest,
    ) -> Result<ObjectTags, OperationError> {
        self.object_tags(Authority::Native, request).await
    }

    /// Read tags with their separate permission in the current object snapshot.
    ///
    /// # Errors
    /// Current denial precedes missing-object disclosure; corruption fails closed.
    pub async fn get_object_tags_http(
        &self,
        identity: Arc<HttpIdentity>,
        request: ReadRequest,
    ) -> Result<ObjectTags, OperationError> {
        self.object_tags(Authority::Http(identity), request).await
    }

    /// Replace the whole tag set and generation, preserving all payload facts.
    /// An empty set clears tags. This is a trusted-native boundary.
    ///
    /// # Errors
    /// Reports missing targets and storage/lifecycle errors with mutation outcome.
    pub async fn replace_object_tags(
        &self,
        request: ReadRequest,
        tags: ObjectTags,
    ) -> Result<(), OperationError> {
        self.change_object_tags(Authority::Native, request, Some(tags))
            .await
    }

    /// Authorize old and requested tags and replace the whole set atomically.
    ///
    /// # Errors
    /// Preserves current denial, missing targets and truthful mutation outcomes.
    pub async fn replace_object_tags_http(
        &self,
        identity: Arc<HttpIdentity>,
        request: ReadRequest,
        tags: ObjectTags,
    ) -> Result<(), OperationError> {
        self.change_object_tags(Authority::Http(identity), request, Some(tags))
            .await
    }

    /// Clear tags without deleting the current object or changing its payload.
    ///
    /// # Errors
    /// Reports missing targets and storage/lifecycle errors with mutation outcome.
    pub async fn delete_object_tags(&self, request: ReadRequest) -> Result<(), OperationError> {
        self.change_object_tags(Authority::Native, request, None)
            .await
    }

    /// Clear tags using the distinct delete-tag permission and current old set.
    ///
    /// # Errors
    /// Preserves current denial, missing targets and truthful mutation outcomes.
    pub async fn delete_object_tags_http(
        &self,
        identity: Arc<HttpIdentity>,
        request: ReadRequest,
    ) -> Result<(), OperationError> {
        self.change_object_tags(Authority::Http(identity), request, None)
            .await
    }

    async fn object_tags(
        &self,
        authority: Authority,
        request: ReadRequest,
    ) -> Result<ObjectTags, OperationError> {
        let operation = self.admit()?;
        let _cancellation = operation.cancel_on_drop();
        let (reply, response) = oneshot::channel();

        self.queue_tags(Command::Get {
            _operation: operation,
            authority,
            request,
            reply,
        })
        .await?;

        response
            .await
            .map_err(|_| OperationError::new(OperationFailure::IoError))?
    }

    async fn change_object_tags(
        &self,
        authority: Authority,
        request: ReadRequest,
        tags: Option<ObjectTags>,
    ) -> Result<(), OperationError> {
        let operation = self
            .admit()
            .map_err(|error| OperationError::from(error).uncommitted())?;

        let _cancellation = operation.cancel_on_drop();
        let (reply, response) = oneshot::channel();

        self.queue_tags(Command::Change {
            operation,
            authority,
            request,
            tags,
            reply,
        })
        .await
        .map_err(OperationError::uncommitted)?;

        response.await.map_err(|_| {
            OperationError::new(OperationFailure::IoError).mutation(MutationOutcome::Unknown)
        })?
    }

    async fn queue_tags(&self, command: Command) -> Result<(), OperationError> {
        let permit =
            tokio::time::timeout(ADMISSION_TIMEOUT, self.owner.state.context.sender.reserve())
                .await
                .map_err(|_| OperationError::new(OperationFailure::StoreBusy))?
                .map_err(|_| OperationError::new(OperationFailure::IoError))?;

        permit.send(WorkerCommand::Tags(command));
        Ok(())
    }
}

pub(super) fn run(command: Command, store: Option<&mut OpenedStore>) {
    match command {
        Command::Get {
            _operation,
            authority,
            request,
            reply,
        } => {
            let result = store
                .ok_or_else(|| OperationError::new(OperationFailure::IoError))
                .and_then(|store| store.database_mut().get_object_tags(&authority, &request));

            let _delivery = reply.send(result);
        }
        Command::Change {
            operation,
            authority,
            request,
            tags,
            reply,
        } => {
            let result = (|| {
                let _commit = operation
                    .enter_mutation()
                    .map_err(OperationError::uncommitted)?;

                let store = store
                    .ok_or_else(|| OperationError::new(OperationFailure::IoError).uncommitted())?;

                store
                    .database_mut()
                    .change_object_tags(&authority, &request, tags.as_ref())
            })();

            let _delivery = reply.send(result.map_err(mutation_error));
        }
    }
}
