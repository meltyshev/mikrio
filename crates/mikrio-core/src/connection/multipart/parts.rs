//! Multipart parts share the bounded file writer and retain its guard through settlement.

use std::path::PathBuf;
use std::sync::Arc;

use tokio::io::AsyncRead;
use tokio::sync::oneshot;

use super::{Command, queue};
use crate::connection::objects::{STREAM_SLOTS, acquire, contain, unknown, write};
use crate::connection::{ADMISSION_TIMEOUT, Operation, mutation_error};
use crate::integrity::{ExpectedDigests, PayloadFacts};
use crate::multipart::PartRequest;
use crate::object::Authority;
use crate::{BlobId, Connection, MetadataError, OperationError, OperationFailure, StoreId};

impl Connection {
    /// Durably replace one part of a trusted native upload without changing its lifetime.
    ///
    /// # Errors
    /// Reports identity, state, size, integrity and source failures with mutation outcomes.
    pub async fn upload_part<R: AsyncRead + Send + 'static>(
        &self,
        request: PartRequest,
        source: R,
    ) -> Result<PayloadFacts, OperationError> {
        self.upload_part_with(request, source, Authority::Native, || {
            Ok(ExpectedDigests::default())
        })
        .await
    }

    /// Replace an authenticated part with current authority rechecked at publication.
    /// The terminal decoder validates framing and any trailers before durable publication.
    ///
    /// # Errors
    /// Preserves source, integrity and storage outcomes; revoked authority cannot publish.
    pub async fn upload_part_http<R: AsyncRead + Send + 'static>(
        &self,
        identity: Arc<crate::access::HttpIdentity>,
        request: PartRequest,
        source: R,
        terminal: impl FnOnce() -> std::io::Result<ExpectedDigests> + Send + 'static,
    ) -> Result<PayloadFacts, OperationError> {
        self.upload_part_with(request, source, Authority::Http(identity), terminal)
            .await
    }

    async fn upload_part_with<R: AsyncRead + Send + 'static>(
        &self,
        request: PartRequest,
        source: R,
        authority: Authority,
        terminal: impl FnOnce() -> std::io::Result<ExpectedDigests> + Send + 'static,
    ) -> Result<PayloadFacts, OperationError> {
        let operation = Arc::new(
            self.admit()
                .map_err(|error| OperationError::from(error).uncommitted())?,
        );

        let _cancellation = operation.cancel_on_drop();

        let stream_slot = operation
            .interrupted(tokio::time::timeout(
                ADMISSION_TIMEOUT,
                STREAM_SLOTS.acquire(),
            ))
            .await
            .map_err(OperationError::uncommitted)?
            .map_err(|_| OperationError::new(OperationFailure::StoreBusy).uncommitted())?
            .map_err(|_| OperationError::new(OperationFailure::IoError).uncommitted())?;

        let path = self.info.directory.clone();
        let request = Arc::new(request);
        let (reply, response) = oneshot::channel();

        drop(tokio::spawn(async move {
            let result = contain(run_part(
                Arc::clone(&operation),
                path,
                authority,
                request,
                source,
                terminal,
                stream_slot,
            ))
            .await;

            operation.state.context.wake_maintenance.notify_one();
            let _delivery = reply.send(result.map_err(mutation_error));
        }));

        response.await.map_err(|_| unknown())?
    }
}

async fn run_part<R: AsyncRead + Send + 'static>(
    operation: Arc<Operation>,
    path: PathBuf,
    authority: Authority,
    request: Arc<PartRequest>,
    source: R,
    terminal: impl FnOnce() -> std::io::Result<ExpectedDigests> + Send + 'static,
    _stream_slot: tokio::sync::SemaphorePermit<'static>,
) -> Result<PayloadFacts, OperationError> {
    let mut reserved = None;

    for _ in 0..4 {
        let blob_id = BlobId::from_bytes(
            *StoreId::generate()
                .map_err(MetadataError::Entropy)?
                .as_bytes(),
        );

        let guard = acquire(Arc::clone(&operation), path.clone(), blob_id).await?;

        let output = write::Output {
            operation: Arc::clone(&operation),
            path: path.clone(),
            blob_id,
            guard,
        };

        if !output.available().await? {
            continue;
        }

        let (reply, response) = oneshot::channel();

        queue(
            &operation.state.context,
            Command::ReservePart {
                operation: Arc::clone(&operation),
                _guard: Arc::clone(&output.guard),
                authority: authority.clone(),
                request: Arc::clone(&request),
                blob_id,
                reply,
            },
        )
        .await?;

        // No source is consumed or file created without a confirmed reservation.
        if let Some(reservation) = response
            .await
            .map_err(|_| OperationError::new(OperationFailure::IoError))??
        {
            reserved = Some((output, reservation));
            break;
        }
    }

    let (output, reservation) =
        reserved.ok_or_else(|| OperationError::new(OperationFailure::IoError))?;

    let verifier = request.verifier(reservation.checksum)?;
    let blob_id = output.blob_id;
    let (guard, facts) = output.publish(source, verifier, terminal).await?;
    let (reply, response) = oneshot::channel();

    queue(
        &operation.state.context,
        Command::PublishPart {
            operation: Arc::clone(&operation),
            _guard: guard,
            authority,
            request,
            reservation,
            blob_id,
            facts,
            reply,
        },
    )
    .await?;

    response.await.map_err(|_| unknown())?
}
