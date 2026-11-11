//! Copy admits a guarded source once and rechecks destination authority at publication.

use std::path::PathBuf;
use std::sync::Arc;

use tokio::sync::oneshot;

use super::{ADMISSION_TIMEOUT, STREAM_SLOTS, contain, mutation_error, source, unknown};

use crate::connection::Operation;
use crate::integrity::PayloadFacts;
use crate::object::{Authority, CopiedObject, CopyRequest, PutRequest};
use crate::{BlobId, Connection, Guard, OpenedStore, OperationError, OperationFailure};

mod selection;
pub(in crate::connection) use selection::select;

pub(in crate::connection) enum Command {
    Reserve {
        operation: Arc<Operation>,
        _guards: Arc<[Arc<Guard>]>,
        authority: Authority,
        request: Arc<CopyRequest>,
        input: Arc<PutRequest>,
        blob_id: BlobId,
        reply: oneshot::Sender<Result<Option<[u8; 16]>, OperationError>>,
    },
    Publish {
        operation: Arc<Operation>,
        _guards: Arc<[Arc<Guard>]>,
        authority: Authority,
        request: Arc<CopyRequest>,
        input: Arc<PutRequest>,
        bucket_id: [u8; 16],
        blob_id: BlobId,
        facts: PayloadFacts,
        reply: oneshot::Sender<Result<CopiedObject, OperationError>>,
    },
}

impl Connection {
    /// Copy a guarded local source into an independent ordinary object.
    /// Source bytes and metadata stay fixed after admission; destination authority
    /// and conditions are checked again in the publishing transaction.
    ///
    /// # Errors
    /// Reports invalid inputs, missing/conditional sources, corruption, storage
    /// and lifecycle failures with the destination mutation outcome.
    pub async fn copy_object(&self, request: CopyRequest) -> Result<CopiedObject, OperationError> {
        self.copy_object_with(Authority::Native, request, None)
            .await
    }

    /// Copy with authenticated source selection and current destination access.
    ///
    /// # Errors
    /// Preserves copy/outcome errors and denies anonymous or revoked publication.
    pub async fn copy_object_http(
        &self,
        identity: Arc<crate::access::HttpIdentity>,
        request: CopyRequest,
    ) -> Result<CopiedObject, OperationError> {
        self.copy_object_with(Authority::Http(identity), request, None)
            .await
    }

    /// Copy with real worker-progress observations for the authenticated adapter.
    ///
    /// # Errors
    /// Preserves the same authorization, lifecycle and mutation-outcome errors.
    pub async fn copy_object_http_with_progress(
        &self,
        identity: Arc<crate::access::HttpIdentity>,
        request: CopyRequest,
        progress: crate::WorkProgress,
    ) -> Result<CopiedObject, OperationError> {
        self.copy_object_with(Authority::Http(identity), request, Some(progress))
            .await
    }

    async fn copy_object_with(
        &self,
        authority: Authority,
        request: CopyRequest,
        progress: Option<crate::WorkProgress>,
    ) -> Result<CopiedObject, OperationError> {
        if matches!(
            &authority,
            Authority::Http(identity) if matches!(identity.0, crate::access::Identity::Anonymous)
        ) {
            return Err(OperationError::new(OperationFailure::AccessDenied).uncommitted());
        }

        request.validate().map_err(OperationError::uncommitted)?;

        let mut operation = self
            .admit()
            .map_err(|error| OperationError::from(error).uncommitted())?;

        operation.progress = progress;

        let operation = Arc::new(operation);

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

            let result = contain(copy(
                Arc::clone(&operation),
                path,
                authority,
                Arc::new(request),
            ))
            .await;

            operation.state.context.wake_maintenance.notify_one();
            let _delivery = reply.send(result.map_err(mutation_error));
        }));

        response.await.map_err(|_| unknown())?
    }
}

async fn copy(
    operation: Arc<Operation>,
    path: PathBuf,
    authority: Authority,
    request: Arc<CopyRequest>,
) -> Result<CopiedObject, OperationError> {
    for _ in 0..4 {
        let Some(selected) = select(
            Arc::clone(&operation),
            path.clone(),
            authority.clone(),
            &request.source_request(),
        )
        .await?
        else {
            continue;
        };

        let input = Arc::new(request.destination(&selected.object)?);

        let verifier = input
            .integrity
            .verifier(Some(selected.object.length.get()))?;

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
                request: Arc::clone(&request),
                input: Arc::clone(&input),
                blob_id,
                reply,
            },
        )
        .await?;

        let Some(bucket_id) = response
            .await
            .map_err(|_| OperationError::new(OperationFailure::IoError))??
        else {
            continue;
        };

        operation.progressed();

        #[cfg(test)]
        crate::protocol_fault::hit("copy", "admitted")?;

        let source = source::Source::new(
            Arc::clone(&operation),
            path,
            std::iter::once(stored_input),
            Arc::clone(&guards),
        );

        let (_output_guard, facts) = output.publish_stored(source, verifier).await?;
        let (reply, response) = oneshot::channel();

        queue(
            &operation,
            Command::Publish {
                operation: Arc::clone(&operation),
                _guards: guards,
                authority,
                request,
                input,
                bucket_id,
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
            input,
            blob_id,
            reply,
        } => {
            let result = (|| {
                operation.check_commit()?;
                let store = store.ok_or_else(|| OperationError::new(OperationFailure::IoError))?;

                store
                    .database_mut()
                    .reserve_copy(&authority, &request, &input, blob_id)
            })();

            let _delivery = reply.send(result.map_err(OperationError::uncommitted));
        }
        Command::Publish {
            operation,
            _guards,
            authority,
            request,
            input,
            bucket_id,
            blob_id,
            facts,
            reply,
        } => {
            let result = (|| {
                operation
                    .check_commit()
                    .map_err(OperationError::uncommitted)?;

                #[cfg(test)]
                crate::protocol_fault::hit("copy", "commit_fence")?;

                let store = store.ok_or_else(|| OperationError::new(OperationFailure::IoError))?;

                let last_modified = store
                    .database_mut()
                    .publish_copy(&authority, &request, &input, bucket_id, blob_id, &facts)?;

                Ok(CopiedObject {
                    facts,
                    last_modified,
                })
            })();

            let _delivery = reply.send(result.map_err(mutation_error));
        }
    }
}
