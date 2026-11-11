//! Read-only namespace commands retain admission until the snapshot settles.

use std::sync::Arc;
use tokio::sync::oneshot;

use super::{ADMISSION_TIMEOUT, Command as WorkerCommand, Connection, Operation};
use crate::access::HttpIdentity;
use crate::object::{Authority, BucketPage, ListBucketsRequest, ListObjectsRequest, ObjectPage};
use crate::{OpenedStore, OperationError, OperationFailure};

pub(super) enum Command {
    Objects {
        _operation: Operation,
        authority: Authority,
        request: ListObjectsRequest,
        reply: oneshot::Sender<Result<ObjectPage, OperationError>>,
    },
    Buckets {
        _operation: Operation,
        authority: Authority,
        request: ListBucketsRequest,
        reply: oneshot::Sender<Result<BucketPage, OperationError>>,
    },
}

impl Connection {
    /// Read one consistent namespace page using trusted directory authority.
    ///
    /// # Errors
    /// Preserves invalid scope/token, missing bucket, owner, corruption and close errors.
    pub async fn list_objects_v2(
        &self,
        request: ListObjectsRequest,
    ) -> Result<ObjectPage, OperationError> {
        self.list_objects(request, Authority::Native).await
    }

    /// Authorize the whole requested prefix before selecting any result rows.
    ///
    /// # Errors
    /// Current access denial is independent of token possession and prior pages.
    pub async fn list_objects_v2_http(
        &self,
        identity: Arc<HttpIdentity>,
        request: ListObjectsRequest,
    ) -> Result<ObjectPage, OperationError> {
        self.list_objects(request, Authority::Http(identity)).await
    }

    async fn list_objects(
        &self,
        request: ListObjectsRequest,
        authority: Authority,
    ) -> Result<ObjectPage, OperationError> {
        request.validate()?;
        let operation = self.admit()?;
        let (reply, response) = oneshot::channel();

        let permit =
            tokio::time::timeout(ADMISSION_TIMEOUT, self.owner.state.context.sender.reserve())
                .await
                .map_err(|_| OperationError::new(OperationFailure::StoreBusy))?
                .map_err(|_| OperationError::new(OperationFailure::IoError))?;

        permit.send(WorkerCommand::Listing(Command::Objects {
            _operation: operation,
            authority,
            request,
            reply,
        }));

        response
            .await
            .map_err(|_| OperationError::new(OperationFailure::IoError))?
    }

    /// Read one live bucket page using trusted directory authority.
    ///
    /// # Errors
    /// Preserves invalid scope/token, corruption and lifecycle failures.
    pub async fn list_buckets(
        &self,
        request: ListBucketsRequest,
    ) -> Result<BucketPage, OperationError> {
        self.buckets(request, Authority::Native).await
    }

    /// Recheck the account-wide permission in the bucket selection snapshot.
    ///
    /// # Errors
    /// Returns current denial, invalid input, corruption or lifecycle failures.
    pub async fn list_buckets_http(
        &self,
        identity: Arc<HttpIdentity>,
        request: ListBucketsRequest,
    ) -> Result<BucketPage, OperationError> {
        self.buckets(request, Authority::Http(identity)).await
    }

    async fn buckets(
        &self,
        request: ListBucketsRequest,
        authority: Authority,
    ) -> Result<BucketPage, OperationError> {
        request.validate()?;
        let operation = self.admit()?;
        let (reply, response) = oneshot::channel();

        let permit =
            tokio::time::timeout(ADMISSION_TIMEOUT, self.owner.state.context.sender.reserve())
                .await
                .map_err(|_| OperationError::new(OperationFailure::StoreBusy))?
                .map_err(|_| OperationError::new(OperationFailure::IoError))?;

        permit.send(WorkerCommand::Listing(Command::Buckets {
            _operation: operation,
            authority,
            request,
            reply,
        }));

        response
            .await
            .map_err(|_| OperationError::new(OperationFailure::IoError))?
    }
}

pub(super) fn run(command: Command, store: Option<&mut OpenedStore>) {
    match command {
        Command::Objects {
            _operation,
            authority,
            request,
            reply,
        } => {
            let result = store
                .ok_or_else(|| OperationError::new(OperationFailure::IoError))
                .and_then(|store| store.database_mut().list_objects(&authority, &request));

            let _delivery = reply.send(result);
        }
        Command::Buckets {
            _operation,
            authority,
            request,
            reply,
        } => {
            let result = store
                .ok_or_else(|| OperationError::new(OperationFailure::IoError))
                .and_then(|store| store.database_mut().list_buckets(&authority, &request));

            let _delivery = reply.send(result);
        }
    }
}
