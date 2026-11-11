//! Trusted administration commands retain operation ownership through SQL settlement.

use tokio::sync::oneshot;

use super::{ADMISSION_TIMEOUT, Command as WorkerCommand, Connection, Operation, mutation_error};
use crate::access::{
    AccessKeyId, CreateAccessKeyOutput, GetPrincipalPolicyOutput, OwnerInfo, PrincipalId,
    PrincipalInfo, PrincipalName,
};
use crate::access::{AdminPageSize, ListAccessKeysOutput, ListPrincipalsOutput};
use crate::{MutationOutcome, OpenedStore, OperationError, OperationFailure};

pub(super) enum Command {
    ListAccessKeys {
        operation: Operation,
        principal_id: PrincipalId,
        max_results: AdminPageSize,
        continuation_token: Option<String>,
        reply: oneshot::Sender<Result<ListAccessKeysOutput, OperationError>>,
    },
    ListPrincipals {
        operation: Operation,
        max_results: AdminPageSize,
        continuation_token: Option<String>,
        reply: oneshot::Sender<Result<ListPrincipalsOutput, OperationError>>,
    },
    GetOwnerInfo {
        operation: Operation,
        reply: oneshot::Sender<Result<OwnerInfo, OperationError>>,
    },
    CreatePrincipal {
        operation: Operation,
        name: PrincipalName,
        reply: oneshot::Sender<Result<PrincipalInfo, OperationError>>,
    },
    GetPrincipal {
        operation: Operation,
        principal_id: PrincipalId,
        reply: oneshot::Sender<Result<PrincipalInfo, OperationError>>,
    },
    SetPrincipalEnabled {
        operation: Operation,
        principal_id: PrincipalId,
        is_enabled: bool,
        reply: oneshot::Sender<Result<PrincipalInfo, OperationError>>,
    },
    DeletePrincipal {
        operation: Operation,
        principal_id: PrincipalId,
        reply: oneshot::Sender<Result<(), OperationError>>,
    },
    PutPrincipalPolicy {
        operation: Operation,
        principal_id: PrincipalId,
        document: String,
        reply: oneshot::Sender<Result<(), OperationError>>,
    },
    GetPrincipalPolicy {
        operation: Operation,
        principal_id: PrincipalId,
        reply: oneshot::Sender<Result<GetPrincipalPolicyOutput, OperationError>>,
    },
    DeletePrincipalPolicy {
        operation: Operation,
        principal_id: PrincipalId,
        reply: oneshot::Sender<Result<(), OperationError>>,
    },
    CreateAccessKey {
        operation: Operation,
        principal_id: PrincipalId,
        reply: oneshot::Sender<Result<CreateAccessKeyOutput, OperationError>>,
    },
    DeleteAccessKey {
        operation: Operation,
        principal_id: PrincipalId,
        access_key_id: AccessKeyId,
        reply: oneshot::Sender<Result<(), OperationError>>,
    },
}

impl Connection {
    /// Read one live page of a principal's access-key IDs and creation times.
    /// No signing secrets are returned.
    ///
    /// # Errors
    /// Rejects invalid or mismatched tokens, missing principals, corrupt rows,
    /// and storage or connection-lifecycle failures.
    pub async fn list_access_keys(
        &self,
        principal_id: PrincipalId,
        max_results: AdminPageSize,
        continuation_token: Option<String>,
    ) -> Result<ListAccessKeysOutput, OperationError> {
        if continuation_token
            .as_ref()
            .is_some_and(|token| token.is_empty() || token.len() > 260)
        {
            return Err(OperationError::new(OperationFailure::InvalidArgument));
        }

        let operation = self.admit()?;
        let (reply, response) = oneshot::channel();

        self.queue_admin(Command::ListAccessKeys {
            operation,
            principal_id,
            max_results,
            continuation_token,
            reply,
        })
        .await?;

        response
            .await
            .map_err(|_| OperationError::new(OperationFailure::IoError))?
    }

    /// Read one live page in ascending principal-ID order.
    ///
    /// # Errors
    /// Rejects invalid or mismatched tokens, corrupt rows, and storage or
    /// connection-lifecycle failures.
    pub async fn list_principals(
        &self,
        max_results: AdminPageSize,
        continuation_token: Option<String>,
    ) -> Result<ListPrincipalsOutput, OperationError> {
        if continuation_token
            .as_ref()
            .is_some_and(|token| token.is_empty() || token.len() > 260)
        {
            return Err(OperationError::new(OperationFailure::InvalidArgument));
        }

        let operation = self.admit()?;
        let (reply, response) = oneshot::channel();

        self.queue_admin(Command::ListPrincipals {
            operation,
            max_results,
            continuation_token,
            reply,
        })
        .await?;

        response
            .await
            .map_err(|_| OperationError::new(OperationFailure::IoError))?
    }

    /// Read the store's immutable account and canonical owner identifiers.
    ///
    /// # Errors
    /// Reports corrupt owner state and storage or connection-lifecycle failures.
    pub async fn get_owner_info(&self) -> Result<OwnerInfo, OperationError> {
        let operation = self.admit()?;
        let (reply, response) = oneshot::channel();

        self.queue_admin(Command::GetOwnerInfo { operation, reply })
            .await?;

        response
            .await
            .map_err(|_| OperationError::new(OperationFailure::IoError))?
    }

    /// Create an enabled principal with no policy or access keys.
    ///
    /// # Errors
    /// Returns `PrincipalAlreadyExists` for a duplicate name. Entropy, storage
    /// and connection-lifecycle failures retain their mutation outcome.
    pub async fn create_principal(
        &self,
        name: PrincipalName,
    ) -> Result<PrincipalInfo, OperationError> {
        let operation = self
            .admit()
            .map_err(|error| OperationError::from(error).uncommitted())?;

        let _cancellation = operation.cancel_on_drop();
        let (reply, response) = oneshot::channel();

        self.queue_admin(Command::CreatePrincipal {
            operation,
            name,
            reply,
        })
        .await
        .map_err(OperationError::uncommitted)?;

        response.await.map_err(|_| {
            OperationError::new(OperationFailure::IoError).mutation(MutationOutcome::Unknown)
        })?
    }

    /// Read the current metadata for one immutable principal identity.
    ///
    /// # Errors
    /// Returns `NoSuchPrincipal` for an absent identity; preserves corrupt-state,
    /// storage and connection-lifecycle failures.
    pub async fn get_principal(
        &self,
        principal_id: PrincipalId,
    ) -> Result<PrincipalInfo, OperationError> {
        let operation = self.admit()?;
        let (reply, response) = oneshot::channel();

        self.queue_admin(Command::GetPrincipal {
            operation,
            principal_id,
            reply,
        })
        .await?;

        response
            .await
            .map_err(|_| OperationError::new(OperationFailure::IoError))?
    }

    /// Set whether the principal's current access keys can authenticate.
    /// Keys and policy remain stored; reenabling does not restore revoked keys.
    ///
    /// # Errors
    /// Returns `NoSuchPrincipal` for an absent identity. Corrupt-state, storage
    /// and connection-lifecycle failures retain their mutation outcome.
    pub async fn set_principal_enabled(
        &self,
        principal_id: PrincipalId,
        is_enabled: bool,
    ) -> Result<PrincipalInfo, OperationError> {
        let operation = self
            .admit()
            .map_err(|error| OperationError::from(error).uncommitted())?;

        let _cancellation = operation.cancel_on_drop();
        let (reply, response) = oneshot::channel();

        self.queue_admin(Command::SetPrincipalEnabled {
            operation,
            principal_id,
            is_enabled,
            reply,
        })
        .await
        .map_err(OperationError::uncommitted)?;

        response.await.map_err(|_| {
            OperationError::new(OperationFailure::IoError).mutation(MutationOutcome::Unknown)
        })?
    }

    /// Delete a principal and its policy after every access key has been revoked.
    ///
    /// # Errors
    /// Returns `NoSuchPrincipal` for an absent identity or `PrincipalHasAccessKeys`
    /// while keys remain. Other failures retain their mutation outcome.
    pub async fn delete_principal(&self, principal_id: PrincipalId) -> Result<(), OperationError> {
        let operation = self
            .admit()
            .map_err(|error| OperationError::from(error).uncommitted())?;

        let _cancellation = operation.cancel_on_drop();
        let (reply, response) = oneshot::channel();

        self.queue_admin(Command::DeletePrincipal {
            operation,
            principal_id,
            reply,
        })
        .await
        .map_err(OperationError::uncommitted)?;

        response.await.map_err(|_| {
            OperationError::new(OperationFailure::IoError).mutation(MutationOutcome::Unknown)
        })?
    }

    /// Validate and atomically replace the principal's complete policy document.
    ///
    /// # Errors
    /// Rejects invalid or unsupported policies and missing principals.
    /// Storage and connection-lifecycle failures retain their mutation outcome.
    pub async fn put_principal_policy(
        &self,
        principal_id: PrincipalId,
        document: String,
    ) -> Result<(), OperationError> {
        if document.len() > 20 * 1024 {
            return Err(OperationError::new(OperationFailure::InvalidArgument).uncommitted());
        }

        let operation = self
            .admit()
            .map_err(|error| OperationError::from(error).uncommitted())?;

        let _cancellation = operation.cancel_on_drop();
        let (reply, response) = oneshot::channel();

        self.queue_admin(Command::PutPrincipalPolicy {
            operation,
            principal_id,
            document,
            reply,
        })
        .await
        .map_err(OperationError::uncommitted)?;

        response.await.map_err(|_| {
            OperationError::new(OperationFailure::IoError).mutation(MutationOutcome::Unknown)
        })?
    }

    /// Read the principal's complete stored policy document.
    ///
    /// # Errors
    /// Returns `NoSuchPrincipal` for an absent identity or `NoSuchPrincipalPolicy`
    /// for an existing identity without a policy. Preserves corrupt-state,
    /// storage and connection-lifecycle failures.
    pub async fn get_principal_policy(
        &self,
        principal_id: PrincipalId,
    ) -> Result<GetPrincipalPolicyOutput, OperationError> {
        let operation = self.admit()?;
        let (reply, response) = oneshot::channel();

        self.queue_admin(Command::GetPrincipalPolicy {
            operation,
            principal_id,
            reply,
        })
        .await?;

        response
            .await
            .map_err(|_| OperationError::new(OperationFailure::IoError))?
    }

    /// Remove the principal's policy; an already absent policy is a successful no-op.
    ///
    /// # Errors
    /// Returns `NoSuchPrincipal` for an absent identity. Storage and
    /// connection-lifecycle failures retain their mutation outcome.
    pub async fn delete_principal_policy(
        &self,
        principal_id: PrincipalId,
    ) -> Result<(), OperationError> {
        let operation = self
            .admit()
            .map_err(|error| OperationError::from(error).uncommitted())?;

        let _cancellation = operation.cancel_on_drop();
        let (reply, response) = oneshot::channel();

        self.queue_admin(Command::DeletePrincipalPolicy {
            operation,
            principal_id,
            reply,
        })
        .await
        .map_err(OperationError::uncommitted)?;

        response.await.map_err(|_| {
            OperationError::new(OperationFailure::IoError).mutation(MutationOutcome::Unknown)
        })?
    }

    /// Create a persistent key for an existing principal, including a disabled one.
    /// Only this confirmed creation result discloses the secret; inventory cannot
    /// recover it after a lost response.
    ///
    /// # Errors
    /// Returns `NoSuchPrincipal` for an absent identity. Entropy, storage and
    /// connection-lifecycle failures retain their mutation outcome.
    pub async fn create_access_key(
        &self,
        principal_id: PrincipalId,
    ) -> Result<CreateAccessKeyOutput, OperationError> {
        let operation = self
            .admit()
            .map_err(|error| OperationError::from(error).uncommitted())?;

        let _cancellation = operation.cancel_on_drop();
        let (reply, response) = oneshot::channel();

        self.queue_admin(Command::CreateAccessKey {
            operation,
            principal_id,
            reply,
        })
        .await
        .map_err(OperationError::uncommitted)?;

        response.await.map_err(|_| {
            OperationError::new(OperationFailure::IoError).mutation(MutationOutcome::Unknown)
        })?
    }

    /// Revoke one principal's access key; an already absent key is a successful no-op.
    ///
    /// # Errors
    /// Returns `NoSuchPrincipal` for an absent identity or `InvalidArgument`
    /// when the key belongs to another principal. Other failures retain their
    /// mutation outcome.
    pub async fn delete_access_key(
        &self,
        principal_id: PrincipalId,
        access_key_id: AccessKeyId,
    ) -> Result<(), OperationError> {
        let operation = self
            .admit()
            .map_err(|error| OperationError::from(error).uncommitted())?;

        let _cancellation = operation.cancel_on_drop();
        let (reply, response) = oneshot::channel();

        self.queue_admin(Command::DeleteAccessKey {
            operation,
            principal_id,
            access_key_id,
            reply,
        })
        .await
        .map_err(OperationError::uncommitted)?;

        response.await.map_err(|_| {
            OperationError::new(OperationFailure::IoError).mutation(MutationOutcome::Unknown)
        })?
    }

    async fn queue_admin(&self, command: Command) -> Result<(), OperationError> {
        let permit =
            tokio::time::timeout(ADMISSION_TIMEOUT, self.owner.state.context.sender.reserve())
                .await
                .map_err(|_| OperationError::new(OperationFailure::StoreBusy))?
                .map_err(|_| OperationError::new(OperationFailure::IoError))?;

        permit.send(WorkerCommand::Admin(command));
        Ok(())
    }
}

pub(super) fn run(command: Command, store: Option<&mut OpenedStore>) {
    match command {
        Command::ListAccessKeys {
            operation,
            principal_id,
            max_results,
            continuation_token,
            reply,
        } => {
            let _operation = operation;

            let result = store
                .ok_or_else(|| OperationError::new(OperationFailure::IoError))
                .and_then(|store| {
                    store.database_mut().list_access_keys(
                        principal_id,
                        max_results,
                        continuation_token.as_deref(),
                    )
                });

            let _delivery = reply.send(result);
        }

        Command::ListPrincipals {
            operation,
            max_results,
            continuation_token,
            reply,
        } => {
            let _operation = operation;

            let result = store
                .ok_or_else(|| OperationError::new(OperationFailure::IoError))
                .and_then(|store| {
                    store
                        .database_mut()
                        .list_principals(max_results, continuation_token.as_deref())
                });

            let _delivery = reply.send(result);
        }

        Command::GetOwnerInfo { operation, reply } => {
            let result = (|| {
                let _operation = operation;
                let store = store.ok_or_else(|| OperationError::new(OperationFailure::IoError))?;
                store.database_mut().get_owner_info()
            })();

            let _delivery = reply.send(result);
        }
        Command::CreatePrincipal {
            operation,
            name,
            reply,
        } => {
            let result = (|| {
                let _commit = operation
                    .enter_mutation()
                    .map_err(OperationError::uncommitted)?;

                let store = store.ok_or_else(|| OperationError::new(OperationFailure::IoError))?;
                store.database_mut().create_principal(name)
            })();

            let _delivery = reply.send(result.map_err(mutation_error));
        }
        Command::GetPrincipal {
            operation,
            principal_id,
            reply,
        } => {
            let result = (|| {
                let _operation = operation;
                let store = store.ok_or_else(|| OperationError::new(OperationFailure::IoError))?;
                store.database_mut().get_principal(principal_id)
            })();

            let _delivery = reply.send(result);
        }
        Command::SetPrincipalEnabled {
            operation,
            principal_id,
            is_enabled,
            reply,
        } => {
            let result = (|| {
                let _commit = operation
                    .enter_mutation()
                    .map_err(OperationError::uncommitted)?;

                let store = store.ok_or_else(|| OperationError::new(OperationFailure::IoError))?;

                store
                    .database_mut()
                    .set_principal_enabled(principal_id, is_enabled)
            })();

            let _delivery = reply.send(result.map_err(mutation_error));
        }
        Command::DeletePrincipal {
            operation,
            principal_id,
            reply,
        } => {
            let result = (|| {
                let _commit = operation
                    .enter_mutation()
                    .map_err(OperationError::uncommitted)?;

                let store = store.ok_or_else(|| OperationError::new(OperationFailure::IoError))?;
                store.database_mut().delete_principal(principal_id)
            })();

            let _delivery = reply.send(result.map_err(mutation_error));
        }
        Command::PutPrincipalPolicy {
            operation,
            principal_id,
            document,
            reply,
        } => {
            let result = (|| {
                let _commit = operation
                    .enter_mutation()
                    .map_err(OperationError::uncommitted)?;

                let store = store.ok_or_else(|| OperationError::new(OperationFailure::IoError))?;

                store
                    .database_mut()
                    .put_principal_policy(principal_id, &document)
            })();

            let _delivery = reply.send(result.map_err(mutation_error));
        }
        Command::GetPrincipalPolicy {
            operation,
            principal_id,
            reply,
        } => {
            let result = (|| {
                let _operation = operation;
                let store = store.ok_or_else(|| OperationError::new(OperationFailure::IoError))?;
                store.database_mut().get_principal_policy(principal_id)
            })();

            let _delivery = reply.send(result);
        }
        Command::DeletePrincipalPolicy {
            operation,
            principal_id,
            reply,
        } => {
            let result = (|| {
                let _commit = operation
                    .enter_mutation()
                    .map_err(OperationError::uncommitted)?;

                let store = store.ok_or_else(|| OperationError::new(OperationFailure::IoError))?;
                store.database_mut().delete_principal_policy(principal_id)
            })();

            let _delivery = reply.send(result.map_err(mutation_error));
        }
        Command::CreateAccessKey {
            operation,
            principal_id,
            reply,
        } => {
            let result = (|| {
                let _commit = operation
                    .enter_mutation()
                    .map_err(OperationError::uncommitted)?;

                let store = store.ok_or_else(|| OperationError::new(OperationFailure::IoError))?;
                store.database_mut().create_access_key(principal_id)
            })();

            let _delivery = reply.send(result.map_err(mutation_error));
        }
        Command::DeleteAccessKey {
            operation,
            principal_id,
            access_key_id,
            reply,
        } => {
            let result = (|| {
                let _commit = operation
                    .enter_mutation()
                    .map_err(OperationError::uncommitted)?;

                let store = store.ok_or_else(|| OperationError::new(OperationFailure::IoError))?;

                store
                    .database_mut()
                    .delete_access_key(principal_id, &access_key_id)
            })();

            let _delivery = reply.send(result.map_err(mutation_error));
        }
    }
}
