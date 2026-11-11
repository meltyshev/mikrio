//! Explicit abort fences publication without waiting for a producer or collector.

use std::sync::Arc;
use tokio::sync::oneshot;

use super::{Authority, Command, Connection, HttpIdentity, UploadRequest, queue};
use crate::{MutationOutcome, OperationError, OperationFailure};

impl Connection {
    /// Retire one active upload through trusted native authority.
    /// Physical bytes remain protected until their actual users settle.
    ///
    /// # Errors
    /// Preserves missing-upload, owner, corruption and storage failures with
    /// explicit mutation outcomes. Repeating an acknowledged abort is missing.
    pub async fn abort_multipart_upload(
        &self,
        request: UploadRequest,
    ) -> Result<(), OperationError> {
        self.abort_multipart_upload_with(Authority::Native, request)
            .await
    }

    /// Retire an upload using current authenticated authority and initiator rights.
    ///
    /// # Errors
    /// Current credential revocation and explicit denies override initiator rights;
    /// owner, missing-upload, corruption and storage failures are preserved.
    pub async fn abort_multipart_upload_http(
        &self,
        identity: Arc<HttpIdentity>,
        request: UploadRequest,
    ) -> Result<(), OperationError> {
        self.abort_multipart_upload_with(Authority::Http(identity), request)
            .await
    }

    async fn abort_multipart_upload_with(
        &self,
        authority: Authority,
        request: UploadRequest,
    ) -> Result<(), OperationError> {
        let operation = self
            .admit()
            .map_err(|error| OperationError::from(error).uncommitted())?;

        let _cancellation = operation.cancel_on_drop();
        let (reply, response) = oneshot::channel();

        queue(
            &self.owner.state.context,
            Command::Abort {
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
