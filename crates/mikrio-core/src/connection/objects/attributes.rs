use std::sync::Arc;

use tokio::sync::oneshot;

use super::{Command, queue};
use crate::object::{AttributesRequest, Authority, StoredAttributes};
use crate::{Connection, OperationError, OperationFailure};

impl Connection {
    /// Select detached attribute facts and applicable original-number part pages.
    /// Count-only objects do not read completed-part rows.
    ///
    /// # Errors
    /// Rejects invalid bounds and reports missing, owner, corruption and lifecycle errors.
    pub async fn get_object_attributes(
        &self,
        request: AttributesRequest,
    ) -> Result<StoredAttributes, OperationError> {
        self.attributes(Authority::Native, request).await
    }

    /// Select current attribute facts with both read grants in one snapshot.
    ///
    /// # Errors
    /// Current denial precedes missing-key disclosure; no payload is opened.
    pub async fn get_object_attributes_http(
        &self,
        identity: Arc<crate::access::HttpIdentity>,
        request: AttributesRequest,
    ) -> Result<StoredAttributes, OperationError> {
        self.attributes(Authority::Http(identity), request).await
    }

    async fn attributes(
        &self,
        authority: Authority,
        request: AttributesRequest,
    ) -> Result<StoredAttributes, OperationError> {
        let operation = self.admit()?;
        let (reply, response) = oneshot::channel();

        queue(
            &self.owner.state.context,
            Command::Attributes {
                _operation: operation,
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
