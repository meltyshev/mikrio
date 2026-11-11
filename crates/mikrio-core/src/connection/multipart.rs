//! Multipart mutation commands retain admission through transaction settlement.

use std::sync::Arc;
use tokio::sync::oneshot;

use super::{ADMISSION_TIMEOUT, Command as WorkerCommand, Connection, Operation};
use crate::access::HttpIdentity;
use crate::integrity::PayloadFacts;
use crate::multipart::{
    CreateRequest, CreatedUpload, ListPartsRequest, ListUploadsRequest, PartPage, PartRequest,
    PartReservation, UploadPage, UploadRequest,
};
use crate::object::Authority;
use crate::{BlobId, Guard, MutationOutcome, OpenedStore, OperationError, OperationFailure};

mod abort;
mod completion;
mod copy;
mod listing;
mod parts;

pub(super) enum Command {
    Complete(completion::Command),
    Copy(copy::Command),
    ListUploads {
        operation: Operation,
        authority: Authority,
        request: ListUploadsRequest,
        reply: oneshot::Sender<Result<UploadPage, OperationError>>,
    },
    ListParts {
        operation: Operation,
        authority: Authority,
        request: ListPartsRequest,
        reply: oneshot::Sender<Result<PartPage, OperationError>>,
    },
    Abort {
        operation: Operation,
        authority: Authority,
        request: UploadRequest,
        reply: oneshot::Sender<Result<(), OperationError>>,
    },
    Create {
        operation: Operation,
        authority: Authority,
        request: CreateRequest,
        reply: oneshot::Sender<Result<CreatedUpload, OperationError>>,
    },
    ReservePart {
        operation: Arc<Operation>,
        _guard: Arc<Guard>,
        authority: Authority,
        request: Arc<PartRequest>,
        blob_id: BlobId,
        reply: oneshot::Sender<Result<Option<PartReservation>, OperationError>>,
    },
    PublishPart {
        operation: Arc<Operation>,
        _guard: Arc<Guard>,
        authority: Authority,
        request: Arc<PartRequest>,
        reservation: PartReservation,
        blob_id: BlobId,
        facts: PayloadFacts,
        reply: oneshot::Sender<Result<PayloadFacts, OperationError>>,
    },
}

impl Connection {
    /// Persist a trusted native multipart initiation with explicit checksum selection.
    /// Closing the connection does not abort an acknowledged upload.
    ///
    /// # Errors
    /// Reports owner/bucket/input/storage failures and preserves mutation outcomes.
    pub async fn create_multipart_upload(
        &self,
        request: CreateRequest,
    ) -> Result<CreatedUpload, OperationError> {
        self.create_multipart_upload_with(Authority::Native, request)
            .await
    }

    /// Persist an authenticated initiation with current transaction-local authority.
    ///
    /// # Errors
    /// Rejects anonymous access, revoked authority and owner/bucket/input failures;
    /// storage and response failures retain their mutation outcome.
    pub async fn create_multipart_upload_http(
        &self,
        identity: Arc<HttpIdentity>,
        request: CreateRequest,
    ) -> Result<CreatedUpload, OperationError> {
        self.create_multipart_upload_with(Authority::Http(identity), request)
            .await
    }

    async fn create_multipart_upload_with(
        &self,
        authority: Authority,
        request: CreateRequest,
    ) -> Result<CreatedUpload, OperationError> {
        let operation = self
            .admit()
            .map_err(|error| OperationError::from(error).uncommitted())?;

        let _cancellation = operation.cancel_on_drop();
        let (reply, response) = oneshot::channel();

        queue(
            &self.owner.state.context,
            Command::Create {
                operation,
                authority,
                request,
                reply,
            },
        )
        .await
        .map_err(OperationError::uncommitted)?;

        response.await.map_err(|_| {
            OperationError::new(OperationFailure::IoError).mutation(MutationOutcome::Unknown)
        })?
    }
}

async fn queue(context: &super::Context, command: Command) -> Result<(), OperationError> {
    let permit = tokio::time::timeout(ADMISSION_TIMEOUT, context.sender.reserve())
        .await
        .map_err(|_| OperationError::new(OperationFailure::StoreBusy))?
        .map_err(|_| OperationError::new(OperationFailure::IoError))?;

    permit.send(WorkerCommand::Multipart(command));
    Ok(())
}

pub(super) fn run(command: Command, store: Option<&mut OpenedStore>) {
    match command {
        Command::Complete(command) => completion::run(command, store),
        Command::Copy(command) => copy::run(command, store),
        Command::ListUploads {
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
                    .list_multipart_uploads(&authority, &request)
            })();

            let _delivery = reply.send(result);
        }
        Command::ListParts {
            operation,
            authority,
            request,
            reply,
        } => {
            let result = (|| {
                let _operation = operation;
                let store = store.ok_or_else(|| OperationError::new(OperationFailure::IoError))?;
                store.database_mut().list_parts(&authority, &request)
            })();

            let _delivery = reply.send(result);
        }
        Command::Abort {
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
                    .abort_multipart_upload(&authority, &request)
            })();

            wake.notify_one();
            let _delivery = reply.send(result.map_err(super::mutation_error));
        }
        Command::Create {
            operation,
            authority,
            request,
            reply,
        } => {
            let result = (|| {
                let _commit = operation
                    .enter_mutation()
                    .map_err(OperationError::uncommitted)?;

                let store = store.ok_or_else(|| OperationError::new(OperationFailure::IoError))?;

                store
                    .database_mut()
                    .create_multipart_upload(&authority, &request)
            })();

            let _delivery = reply.send(result.map_err(super::mutation_error));
        }
        Command::ReservePart {
            operation,
            _guard,
            authority,
            request,
            blob_id,
            reply,
        } => {
            let result = (|| {
                // Reservation protects bytes, not a logical part publication.
                let is_canceled = *operation.cancellation.borrow()
                    || *operation.state.closing.borrow()
                    || operation
                        .listener
                        .as_ref()
                        .is_some_and(|scope| *scope.closing.borrow());

                if is_canceled {
                    return Err(OperationError::new(OperationFailure::OperationCanceled));
                }

                let store = store.ok_or_else(|| OperationError::new(OperationFailure::IoError))?;

                store
                    .database_mut()
                    .reserve_part(&authority, &request, blob_id)
            })();

            let _delivery = reply.send(result.map_err(OperationError::uncommitted));
        }
        Command::PublishPart {
            operation,
            _guard,
            authority,
            request,
            reservation,
            blob_id,
            facts,
            reply,
        } => {
            let result = (|| {
                operation
                    .check_commit()
                    .map_err(OperationError::uncommitted)?;

                #[cfg(test)]
                crate::protocol_fault::hit("part.commit_fence", "entered")?;

                let store = store.ok_or_else(|| OperationError::new(OperationFailure::IoError))?;

                store.database_mut().publish_part(
                    &authority,
                    &request,
                    reservation,
                    blob_id,
                    &facts,
                )?;

                Ok(facts)
            })();

            let _delivery = reply.send(result.map_err(super::mutation_error));
        }
    }
}
