//! Owned page commands share the same bounded admission as other metadata reads.

use std::sync::Arc;
use tokio::sync::oneshot;

use super::{
    Authority, Command, Connection, HttpIdentity, ListPartsRequest, ListUploadsRequest, PartPage,
    UploadPage, queue,
};
use crate::{OperationError, OperationFailure};

impl Connection {
    /// Read one trusted native page of active uploads and common prefixes.
    ///
    /// # Errors
    /// Preserves owner, bucket, corruption and storage failures.
    pub async fn list_multipart_uploads(
        &self,
        request: ListUploadsRequest,
    ) -> Result<UploadPage, OperationError> {
        self.list_multipart_uploads_with(Authority::Native, request)
            .await
    }

    /// Read active uploads with current bucket-wide HTTP listing permission.
    ///
    /// # Errors
    /// No initiator default applies. Preserves current denial, owner, bucket,
    /// corruption and storage failures.
    pub async fn list_multipart_uploads_http(
        &self,
        identity: Arc<HttpIdentity>,
        request: ListUploadsRequest,
    ) -> Result<UploadPage, OperationError> {
        self.list_multipart_uploads_with(Authority::Http(identity), request)
            .await
    }

    async fn list_multipart_uploads_with(
        &self,
        authority: Authority,
        request: ListUploadsRequest,
    ) -> Result<UploadPage, OperationError> {
        let operation = self.admit()?;
        let (reply, response) = oneshot::channel();

        queue(
            &self.owner.state.context,
            Command::ListUploads {
                operation,
                authority,
                request,
                reply,
            },
        )
        .await?;

        response
            .await
            .map_err(|_| OperationError::new(OperationFailure::IoError))?
    }

    /// Read one trusted native page of currently committed parts.
    ///
    /// # Errors
    /// Reports owner, missing-upload, corruption and storage failures.
    pub async fn list_parts(&self, request: ListPartsRequest) -> Result<PartPage, OperationError> {
        self.list_parts_with(Authority::Native, request).await
    }

    /// Read one page using current authenticated authority and initiator rights.
    ///
    /// # Errors
    /// Current credential revocation and explicit denies override initiator rights;
    /// owner, missing-upload, corruption and storage failures are preserved.
    pub async fn list_parts_http(
        &self,
        identity: Arc<HttpIdentity>,
        request: ListPartsRequest,
    ) -> Result<PartPage, OperationError> {
        self.list_parts_with(Authority::Http(identity), request)
            .await
    }

    async fn list_parts_with(
        &self,
        authority: Authority,
        request: ListPartsRequest,
    ) -> Result<PartPage, OperationError> {
        let operation = self.admit()?;
        let (reply, response) = oneshot::channel();

        queue(
            &self.owner.state.context,
            Command::ListParts {
                operation,
                authority,
                request,
                reply,
            },
        )
        .await?;

        response
            .await
            .map_err(|_| OperationError::new(OperationFailure::IoError))?
    }
}
