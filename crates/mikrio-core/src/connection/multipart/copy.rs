//! A copied part uses the ordinary part reservation, publication and upload fence.

use std::path::PathBuf;
use std::sync::Arc;

use tokio::sync::oneshot;

use crate::connection::objects::{STREAM_SLOTS, contain, copy, source, unknown};
use crate::connection::{ADMISSION_TIMEOUT, Operation, mutation_error};
use crate::integrity::PayloadFacts;
use crate::multipart::{CopiedPart, CopyPartRequest, PartRequest, PartReservation};
use crate::object::Authority;
use crate::{BlobId, Connection, Guard, OpenedStore, OperationError, OperationFailure};

pub(in crate::connection) enum Command {
    Reserve {
        operation: Arc<Operation>,
        _guards: Arc<[Arc<Guard>]>,
        authority: Authority,
        request: Arc<PartRequest>,
        blob_id: BlobId,
        reply: oneshot::Sender<Result<Option<PartReservation>, OperationError>>,
    },
    Publish {
        operation: Arc<Operation>,
        _guards: Arc<[Arc<Guard>]>,
        authority: Authority,
        request: Arc<PartRequest>,
        reservation: PartReservation,
        blob_id: BlobId,
        facts: PayloadFacts,
        reply: oneshot::Sender<Result<CopiedPart, OperationError>>,
    },
}

impl Connection {
    /// Copy a complete local source into an independent part of a native upload.
    /// Each call selects its own source generation and uses the upload's checksum.
    ///
    /// # Errors
    /// Reports source, size, upload-state, integrity and lifecycle failures with
    /// the destination part's mutation outcome.
    pub async fn upload_part_copy(
        &self,
        request: CopyPartRequest,
    ) -> Result<CopiedPart, OperationError> {
        self.upload_part_copy_with(Authority::Native, request, None)
            .await
    }

    /// Copy a complete source with separate current source/destination authority.
    ///
    /// # Errors
    /// Preserves part outcomes; revoked authority and a non-OPEN upload cannot publish.
    pub async fn upload_part_copy_http(
        &self,
        identity: Arc<crate::access::HttpIdentity>,
        request: CopyPartRequest,
    ) -> Result<CopiedPart, OperationError> {
        self.upload_part_copy_with(Authority::Http(identity), request, None)
            .await
    }

    /// Copy a part while reporting completed native work to the HTTP stall monitor.
    ///
    /// # Errors
    /// Preserves the same authorization, fencing and mutation outcomes as part copy.
    pub async fn upload_part_copy_http_with_progress(
        &self,
        identity: Arc<crate::access::HttpIdentity>,
        request: CopyPartRequest,
        progress: crate::WorkProgress,
    ) -> Result<CopiedPart, OperationError> {
        self.upload_part_copy_with(Authority::Http(identity), request, Some(progress))
            .await
    }

    async fn upload_part_copy_with(
        &self,
        authority: Authority,
        request: CopyPartRequest,
        progress: Option<crate::WorkProgress>,
    ) -> Result<CopiedPart, OperationError> {
        if matches!(
            &authority,
            Authority::Http(identity) if matches!(identity.0, crate::access::Identity::Anonymous)
        ) {
            return Err(OperationError::new(OperationFailure::AccessDenied).uncommitted());
        }

        let mut admitted = self
            .admit()
            .map_err(|error| OperationError::from(error).uncommitted())?;

        admitted.progress = progress;

        let operation = Arc::new(admitted);

        let _cancellation = operation.cancel_on_drop();

        let slot = operation
            .interrupted(tokio::time::timeout(
                ADMISSION_TIMEOUT,
                STREAM_SLOTS.acquire(),
            ))
            .await
            .map_err(OperationError::uncommitted)?
            .map_err(|_| OperationError::new(OperationFailure::StoreBusy).uncommitted())?
            .map_err(|_| OperationError::new(OperationFailure::IoError).uncommitted())?;

        let path = self.info.directory.clone();
        let (reply, response) = oneshot::channel();

        drop(tokio::spawn(async move {
            let _slot = slot;
            let result = contain(copy_part(Arc::clone(&operation), path, authority, request)).await;
            operation.state.context.wake_maintenance.notify_one();
            let _delivery = reply.send(result.map_err(mutation_error));
        }));

        response.await.map_err(|_| unknown())?
    }
}

async fn copy_part(
    operation: Arc<Operation>,
    path: PathBuf,
    authority: Authority,
    request: CopyPartRequest,
) -> Result<CopiedPart, OperationError> {
    for _ in 0..4 {
        let Some(selected) = copy::select(
            Arc::clone(&operation),
            path.clone(),
            authority.clone(),
            &request.source_request(),
        )
        .await?
        else {
            continue;
        };

        let input = Arc::new(request.destination(selected.object.length)?);
        let stored_input = selected.input()?;
        let output = selected.output;
        let guards = selected.guards;
        let blob_id = output.blob_id;
        let (reply, response) = oneshot::channel();

        queue(
            &operation,
            Command::Reserve {
                operation: Arc::clone(&operation),
                _guards: Arc::clone(&guards),
                authority: authority.clone(),
                request: Arc::clone(&input),
                blob_id,
                reply,
            },
        )
        .await?;

        let Some(reservation) = response
            .await
            .map_err(|_| OperationError::new(OperationFailure::IoError))??
        else {
            continue;
        };

        operation.progressed();

        #[cfg(test)]
        crate::protocol_fault::hit("part.copy", "admitted")?;

        let verifier = input.verifier(reservation.checksum)?;

        let source = source::Source::new(
            Arc::clone(&operation),
            path,
            std::iter::once(stored_input),
            Arc::clone(&guards),
        )
        .with_range(request.source_range);

        let (_output_guard, facts) = output.publish_stored(source, verifier).await?;
        let (reply, response) = oneshot::channel();

        queue(
            &operation,
            Command::Publish {
                operation: Arc::clone(&operation),
                _guards: guards,
                authority,
                request: input,
                reservation,
                blob_id,
                facts,
                reply,
            },
        )
        .await?;

        return response.await.map_err(|_| unknown())?;
    }

    Err(OperationError::new(OperationFailure::StoreBusy))
}

async fn queue(operation: &Operation, command: Command) -> Result<(), OperationError> {
    super::queue(&operation.state.context, super::Command::Copy(command)).await
}

pub(super) fn run(command: Command, store: Option<&mut OpenedStore>) {
    match command {
        Command::Reserve {
            operation,
            _guards,
            authority,
            request,
            blob_id,
            reply,
        } => {
            let result = (|| {
                operation.check_commit()?;
                let store = store.ok_or_else(|| OperationError::new(OperationFailure::IoError))?;

                store
                    .database_mut()
                    .reserve_part(&authority, &request, blob_id)
            })();

            let _delivery = reply.send(result.map_err(OperationError::uncommitted));
        }
        Command::Publish {
            operation,
            _guards,
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

                let last_modified = store.database_mut().publish_part(
                    &authority,
                    &request,
                    reservation,
                    blob_id,
                    &facts,
                )?;

                Ok(CopiedPart {
                    facts,
                    last_modified,
                })
            })();

            let _delivery = reply.send(result.map_err(mutation_error));
        }
    }
}
