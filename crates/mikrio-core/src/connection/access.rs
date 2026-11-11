//! Authenticated-path commands reuse the existing bounded metadata worker.
//! No database, SQL callback, or native-authority selector escapes to HTTP.

use std::sync::Arc;

use tokio::sync::oneshot;

use super::{ADMISSION_TIMEOUT, Command as WorkerCommand, Connection, Operation, shutdown};
use crate::access::{CredentialClass, HttpIdentity, PolicyError, SigningKey};
use crate::{AccessError, OpenError, OpenFailure, OpenedStore, OwnedAccessRequest};

pub(super) enum Command {
    ResolveKey {
        _operation: Operation,
        key: String,
        reply: oneshot::Sender<Result<SigningKey, AccessError>>,
    },
    Authorize {
        _operation: Operation,
        identity: Arc<HttpIdentity>,
        requests: Vec<OwnedAccessRequest>,
        reply: oneshot::Sender<Result<(), AccessError>>,
    },
}

impl Connection {
    /// Resolve only an application key on the existing native worker.
    /// This supplies signing material; signature verification and current access
    /// checks remain separate. Caller cancellation never releases an enqueued
    /// command's operation owner before its actual SQL work settles.
    ///
    /// # Errors
    /// Preserves invalid/disabled/missing/corrupt credentials, bounded queue
    /// exhaustion, connection closure and native storage/settlement failures.
    pub async fn resolve_application_key(&self, key: String) -> Result<SigningKey, AccessError> {
        if CredentialClass::parse(&key)? != CredentialClass::Application {
            return Err(AccessError::Denied);
        }

        let operation = self.admit()?;
        let (reply, response) = oneshot::channel();

        self.queue_access(Command::ResolveKey {
            _operation: operation,
            key,
            reply,
        })
        .await?;

        response
            .await
            .map_err(|_| shutdown("signing-key result unavailable"))?
    }

    /// Admit a bounded explicit permission set through one current SQL snapshot.
    /// The opaque identity cannot represent native authority. This result never
    /// replaces a mutation's transaction-local final authorization check.
    ///
    /// # Errors
    /// Returns denied/invalid/corrupt access distinctly from queue, lifecycle or
    /// storage failures. Refuses zero or more than four permission checks.
    pub async fn authorize_http(
        &self,
        identity: Arc<HttpIdentity>,
        requests: Vec<OwnedAccessRequest>,
    ) -> Result<(), AccessError> {
        if requests.is_empty() || requests.len() > 4 {
            return Err(AccessError::Invalid(PolicyError::Invalid));
        }

        let operation = self.admit()?;
        let (reply, response) = oneshot::channel();

        self.queue_access(Command::Authorize {
            _operation: operation,
            identity,
            requests,
            reply,
        })
        .await?;

        response
            .await
            .map_err(|_| shutdown("authorization result unavailable"))?
    }

    async fn queue_access(&self, command: Command) -> Result<(), AccessError> {
        let permit =
            tokio::time::timeout(ADMISSION_TIMEOUT, self.owner.state.context.sender.reserve())
                .await
                .map_err(|_| crate::ConnectionError::from(OpenError::new(OpenFailure::StoreBusy)))?
                .map_err(|_| shutdown("access worker unavailable"))?;

        permit.send(WorkerCommand::Access(command));
        Ok(())
    }
}

pub(super) fn run(command: Command, store: Option<&mut OpenedStore>) {
    match command {
        Command::ResolveKey {
            _operation,
            key,
            reply,
        } => {
            let result = store
                .ok_or_else(|| AccessError::from(shutdown("missing access store")))
                .and_then(|store| store.database_mut().resolve_application_key(&key));

            let _delivery = reply.send(result);
        }
        Command::Authorize {
            _operation,
            identity,
            requests,
            reply,
        } => {
            let result = store
                .ok_or_else(|| AccessError::from(shutdown("missing access store")))
                .and_then(|store| {
                    let requests: Vec<_> = requests
                        .iter()
                        .map(OwnedAccessRequest::as_request)
                        .collect();

                    store
                        .database_mut()
                        .authorize_http_all(&identity, &requests)
                });

            let _delivery = reply.send(result);
        }
    }
}
