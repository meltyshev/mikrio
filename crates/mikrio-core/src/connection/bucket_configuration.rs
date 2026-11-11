//! Typed bucket configuration commands retain ownership through transaction settlement.

use std::sync::Arc;
use tokio::sync::oneshot;

use super::{ADMISSION_TIMEOUT, Command as WorkerCommand, Connection, Operation, mutation_error};
use crate::access::{BucketAccessRequest, GetBucketPolicyOutput, HttpIdentity, OwnerInfo};
use crate::cors::CorsConfiguration;
use crate::object::{Authority, BucketName};
use crate::{MutationOutcome, OpenedStore, OperationError, OperationFailure, PublicAccessBlock};

pub(super) enum Command {
    PutBucketCors {
        operation: Operation,
        authority: Authority,
        request: BucketAccessRequest,
        configuration: CorsConfiguration,
        reply: oneshot::Sender<Result<(), OperationError>>,
    },
    GetBucketCors {
        operation: Operation,
        authority: Authority,
        request: BucketAccessRequest,
        reply: oneshot::Sender<Result<CorsConfiguration, OperationError>>,
    },
    DeleteBucketCors {
        operation: Operation,
        authority: Authority,
        request: BucketAccessRequest,
        reply: oneshot::Sender<Result<(), OperationError>>,
    },
    CorsForResponse {
        operation: Operation,
        bucket: BucketName,
        reply: oneshot::Sender<Result<Option<CorsConfiguration>, OperationError>>,
    },

    PutBucketPolicy {
        operation: Operation,
        authority: Authority,
        request: BucketAccessRequest,
        document: String,
        reply: oneshot::Sender<Result<(), OperationError>>,
    },
    GetBucketPolicy {
        operation: Operation,
        authority: Authority,
        request: BucketAccessRequest,
        reply: oneshot::Sender<Result<GetBucketPolicyOutput, OperationError>>,
    },
    DeleteBucketPolicy {
        operation: Operation,
        authority: Authority,
        request: BucketAccessRequest,
        reply: oneshot::Sender<Result<(), OperationError>>,
    },
    GetBucketPolicyStatus {
        operation: Operation,
        authority: Authority,
        request: BucketAccessRequest,
        reply: oneshot::Sender<Result<bool, OperationError>>,
    },
    PutPublicAccessBlock {
        operation: Operation,
        authority: Authority,
        request: BucketAccessRequest,
        configuration: PublicAccessBlock,
        reply: oneshot::Sender<Result<(), OperationError>>,
    },
    GetPublicAccessBlock {
        operation: Operation,
        authority: Authority,
        request: BucketAccessRequest,
        reply: oneshot::Sender<Result<PublicAccessBlock, OperationError>>,
    },
    DeletePublicAccessBlock {
        operation: Operation,
        authority: Authority,
        request: BucketAccessRequest,
        reply: oneshot::Sender<Result<(), OperationError>>,
    },
    PutBucketOwnershipControls {
        operation: Operation,
        authority: Authority,
        request: BucketAccessRequest,
        reply: oneshot::Sender<Result<(), OperationError>>,
    },
    GetBucketOwnershipControls {
        operation: Operation,
        authority: Authority,
        request: BucketAccessRequest,
        reply: oneshot::Sender<Result<(), OperationError>>,
    },
    GetBucketAcl {
        operation: Operation,
        authority: Authority,
        request: BucketAccessRequest,
        reply: oneshot::Sender<Result<OwnerInfo, OperationError>>,
    },
}

impl Connection {
    /// Replace the complete CORS configuration.
    /// Uses trusted native authority on the bounded metadata worker.
    ///
    /// # Errors
    /// Preserves missing buckets, owner/access denial, corrupt state and
    /// storage or connection-lifecycle failures. Mutation errors retain their
    /// settlement outcome.
    pub async fn put_bucket_cors(
        &self,
        request: BucketAccessRequest,
        configuration: CorsConfiguration,
    ) -> Result<(), OperationError> {
        self.put_bucket_cors_with(Authority::Native, request, configuration)
            .await
    }

    /// Replace the complete CORS configuration.
    /// Authorizes against current HTTP identity and bucket state.
    ///
    /// # Errors
    /// Preserves missing buckets, owner/access denial, corrupt state and
    /// storage or connection-lifecycle failures. Mutation errors retain their
    /// settlement outcome.
    pub async fn put_bucket_cors_http(
        &self,
        identity: Arc<HttpIdentity>,
        request: BucketAccessRequest,
        configuration: CorsConfiguration,
    ) -> Result<(), OperationError> {
        self.put_bucket_cors_with(Authority::Http(identity), request, configuration)
            .await
    }

    async fn put_bucket_cors_with(
        &self,
        authority: Authority,
        request: BucketAccessRequest,
        configuration: CorsConfiguration,
    ) -> Result<(), OperationError> {
        let operation = self
            .admit()
            .map_err(|error| OperationError::from(error).uncommitted())?;

        let _cancellation = operation.cancel_on_drop();
        let (reply, response) = oneshot::channel();

        self.queue_bucket_configuration(Command::PutBucketCors {
            operation,
            authority,
            request,
            configuration,
            reply,
        })
        .await
        .map_err(OperationError::uncommitted)?;

        response.await.map_err(|_| {
            OperationError::new(OperationFailure::IoError).mutation(MutationOutcome::Unknown)
        })?
    }

    /// Read the current CORS configuration.
    /// Uses trusted native authority on the bounded metadata worker.
    ///
    /// # Errors
    /// Returns `NoSuchCorsConfiguration` when configuration is absent.
    /// Preserves missing buckets, owner/access denial, corrupt state and
    /// storage or connection-lifecycle failures.
    pub async fn get_bucket_cors(
        &self,
        request: BucketAccessRequest,
    ) -> Result<CorsConfiguration, OperationError> {
        self.get_bucket_cors_with(Authority::Native, request).await
    }

    /// Read the current CORS configuration.
    /// Authorizes against current HTTP identity and bucket state.
    ///
    /// # Errors
    /// Returns `NoSuchCorsConfiguration` when configuration is absent.
    /// Preserves missing buckets, owner/access denial, corrupt state and
    /// storage or connection-lifecycle failures.
    pub async fn get_bucket_cors_http(
        &self,
        identity: Arc<HttpIdentity>,
        request: BucketAccessRequest,
    ) -> Result<CorsConfiguration, OperationError> {
        self.get_bucket_cors_with(Authority::Http(identity), request)
            .await
    }

    async fn get_bucket_cors_with(
        &self,
        authority: Authority,
        request: BucketAccessRequest,
    ) -> Result<CorsConfiguration, OperationError> {
        let operation = self.admit()?;
        let (reply, response) = oneshot::channel();

        self.queue_bucket_configuration(Command::GetBucketCors {
            operation,
            authority,
            request,
            reply,
        })
        .await?;

        response
            .await
            .map_err(|_| OperationError::new(OperationFailure::IoError))?
    }

    /// Remove CORS configuration; absence is a successful no-op.
    /// Uses trusted native authority on the bounded metadata worker.
    ///
    /// # Errors
    /// Preserves missing buckets, owner/access denial, corrupt state and
    /// storage or connection-lifecycle failures. Mutation errors retain their
    /// settlement outcome.
    pub async fn delete_bucket_cors(
        &self,
        request: BucketAccessRequest,
    ) -> Result<(), OperationError> {
        self.delete_bucket_cors_with(Authority::Native, request)
            .await
    }

    /// Remove CORS configuration; absence is a successful no-op.
    /// Authorizes against current HTTP identity and bucket state.
    ///
    /// # Errors
    /// Preserves missing buckets, owner/access denial, corrupt state and
    /// storage or connection-lifecycle failures. Mutation errors retain their
    /// settlement outcome.
    pub async fn delete_bucket_cors_http(
        &self,
        identity: Arc<HttpIdentity>,
        request: BucketAccessRequest,
    ) -> Result<(), OperationError> {
        self.delete_bucket_cors_with(Authority::Http(identity), request)
            .await
    }

    async fn delete_bucket_cors_with(
        &self,
        authority: Authority,
        request: BucketAccessRequest,
    ) -> Result<(), OperationError> {
        let operation = self
            .admit()
            .map_err(|error| OperationError::from(error).uncommitted())?;

        let _cancellation = operation.cancel_on_drop();
        let (reply, response) = oneshot::channel();

        self.queue_bucket_configuration(Command::DeleteBucketCors {
            operation,
            authority,
            request,
            reply,
        })
        .await
        .map_err(OperationError::uncommitted)?;

        response.await.map_err(|_| {
            OperationError::new(OperationFailure::IoError).mutation(MutationOutcome::Unknown)
        })?
    }

    /// Select current CORS rules for response matching without a data-access grant.
    ///
    /// # Errors
    /// Preserves missing buckets, corrupt rules and lifecycle/storage failures.
    pub async fn cors_for_response(
        &self,
        bucket: BucketName,
    ) -> Result<Option<CorsConfiguration>, OperationError> {
        let operation = self.admit()?;
        let (reply, response) = oneshot::channel();

        self.queue_bucket_configuration(Command::CorsForResponse {
            operation,
            bucket,
            reply,
        })
        .await?;

        response
            .await
            .map_err(|_| OperationError::new(OperationFailure::IoError))?
    }

    /// Validate and atomically replace the complete bucket policy.
    /// Uses trusted native authority on the bounded metadata worker.
    ///
    /// # Errors
    /// Rejects invalid or unsupported policy documents.
    /// Preserves missing buckets, owner/access denial, corrupt state and
    /// storage or connection-lifecycle failures. Mutation errors retain their
    /// settlement outcome.
    pub async fn put_bucket_policy(
        &self,
        request: BucketAccessRequest,
        document: String,
    ) -> Result<(), OperationError> {
        self.put_bucket_policy_with(Authority::Native, request, document)
            .await
    }

    /// Validate and atomically replace the complete bucket policy.
    /// Authorizes against current HTTP identity and bucket state.
    ///
    /// # Errors
    /// Rejects invalid or unsupported policy documents.
    /// Preserves missing buckets, owner/access denial, corrupt state and
    /// storage or connection-lifecycle failures. Mutation errors retain their
    /// settlement outcome.
    pub async fn put_bucket_policy_http(
        &self,
        identity: Arc<HttpIdentity>,
        request: BucketAccessRequest,
        document: String,
    ) -> Result<(), OperationError> {
        self.put_bucket_policy_with(Authority::Http(identity), request, document)
            .await
    }

    async fn put_bucket_policy_with(
        &self,
        authority: Authority,
        request: BucketAccessRequest,
        document: String,
    ) -> Result<(), OperationError> {
        crate::access::Policy::validate_envelope(&document).map_err(|error| {
            OperationError::caused(OperationFailure::InvalidArgument, error).uncommitted()
        })?;

        let operation = self
            .admit()
            .map_err(|error| OperationError::from(error).uncommitted())?;

        let _cancellation = operation.cancel_on_drop();
        let (reply, response) = oneshot::channel();

        self.queue_bucket_configuration(Command::PutBucketPolicy {
            operation,
            authority,
            request,
            document,
            reply,
        })
        .await
        .map_err(OperationError::uncommitted)?;

        response.await.map_err(|_| {
            OperationError::new(OperationFailure::IoError).mutation(MutationOutcome::Unknown)
        })?
    }

    /// Read the complete stored bucket policy.
    /// Uses trusted native authority on the bounded metadata worker.
    ///
    /// # Errors
    /// Returns `NoSuchBucketPolicy` when configuration is absent.
    /// Preserves missing buckets, owner/access denial, corrupt state and
    /// storage or connection-lifecycle failures.
    pub async fn get_bucket_policy(
        &self,
        request: BucketAccessRequest,
    ) -> Result<GetBucketPolicyOutput, OperationError> {
        self.get_bucket_policy_with(Authority::Native, request)
            .await
    }

    /// Read the complete stored bucket policy.
    /// Authorizes against current HTTP identity and bucket state.
    ///
    /// # Errors
    /// Returns `NoSuchBucketPolicy` when configuration is absent.
    /// Preserves missing buckets, owner/access denial, corrupt state and
    /// storage or connection-lifecycle failures.
    pub async fn get_bucket_policy_http(
        &self,
        identity: Arc<HttpIdentity>,
        request: BucketAccessRequest,
    ) -> Result<GetBucketPolicyOutput, OperationError> {
        self.get_bucket_policy_with(Authority::Http(identity), request)
            .await
    }

    async fn get_bucket_policy_with(
        &self,
        authority: Authority,
        request: BucketAccessRequest,
    ) -> Result<GetBucketPolicyOutput, OperationError> {
        let operation = self.admit()?;
        let (reply, response) = oneshot::channel();

        self.queue_bucket_configuration(Command::GetBucketPolicy {
            operation,
            authority,
            request,
            reply,
        })
        .await?;

        response
            .await
            .map_err(|_| OperationError::new(OperationFailure::IoError))?
    }

    /// Remove the bucket policy; absence is a successful no-op.
    /// Uses trusted native authority on the bounded metadata worker.
    ///
    /// # Errors
    /// Preserves missing buckets, owner/access denial, corrupt state and
    /// storage or connection-lifecycle failures. Mutation errors retain their
    /// settlement outcome.
    pub async fn delete_bucket_policy(
        &self,
        request: BucketAccessRequest,
    ) -> Result<(), OperationError> {
        self.delete_bucket_policy_with(Authority::Native, request)
            .await
    }

    /// Remove the bucket policy; absence is a successful no-op.
    /// Authorizes against current HTTP identity and bucket state.
    ///
    /// # Errors
    /// Preserves missing buckets, owner/access denial, corrupt state and
    /// storage or connection-lifecycle failures. Mutation errors retain their
    /// settlement outcome.
    pub async fn delete_bucket_policy_http(
        &self,
        identity: Arc<HttpIdentity>,
        request: BucketAccessRequest,
    ) -> Result<(), OperationError> {
        self.delete_bucket_policy_with(Authority::Http(identity), request)
            .await
    }

    async fn delete_bucket_policy_with(
        &self,
        authority: Authority,
        request: BucketAccessRequest,
    ) -> Result<(), OperationError> {
        let operation = self
            .admit()
            .map_err(|error| OperationError::from(error).uncommitted())?;

        let _cancellation = operation.cancel_on_drop();
        let (reply, response) = oneshot::channel();

        self.queue_bucket_configuration(Command::DeleteBucketPolicy {
            operation,
            authority,
            request,
            reply,
        })
        .await
        .map_err(OperationError::uncommitted)?;

        response.await.map_err(|_| {
            OperationError::new(OperationFailure::IoError).mutation(MutationOutcome::Unknown)
        })?
    }

    /// Classify whether the current bucket policy grants public access.
    /// Uses trusted native authority on the bounded metadata worker.
    ///
    /// # Errors
    /// Returns `NoSuchBucketPolicy` when configuration is absent.
    /// Preserves missing buckets, owner/access denial, corrupt state and
    /// storage or connection-lifecycle failures.
    pub async fn get_bucket_policy_status(
        &self,
        request: BucketAccessRequest,
    ) -> Result<bool, OperationError> {
        self.get_bucket_policy_status_with(Authority::Native, request)
            .await
    }

    /// Classify whether the current bucket policy grants public access.
    /// Authorizes against current HTTP identity and bucket state.
    ///
    /// # Errors
    /// Returns `NoSuchBucketPolicy` when configuration is absent.
    /// Preserves missing buckets, owner/access denial, corrupt state and
    /// storage or connection-lifecycle failures.
    pub async fn get_bucket_policy_status_http(
        &self,
        identity: Arc<HttpIdentity>,
        request: BucketAccessRequest,
    ) -> Result<bool, OperationError> {
        self.get_bucket_policy_status_with(Authority::Http(identity), request)
            .await
    }

    async fn get_bucket_policy_status_with(
        &self,
        authority: Authority,
        request: BucketAccessRequest,
    ) -> Result<bool, OperationError> {
        let operation = self.admit()?;
        let (reply, response) = oneshot::channel();

        self.queue_bucket_configuration(Command::GetBucketPolicyStatus {
            operation,
            authority,
            request,
            reply,
        })
        .await?;

        response
            .await
            .map_err(|_| OperationError::new(OperationFailure::IoError))?
    }

    /// Replace all four public-access block flags together.
    /// Uses trusted native authority on the bounded metadata worker.
    ///
    /// # Errors
    /// Preserves missing buckets, owner/access denial, corrupt state and
    /// storage or connection-lifecycle failures. Mutation errors retain their
    /// settlement outcome.
    pub async fn put_public_access_block(
        &self,
        request: BucketAccessRequest,
        configuration: PublicAccessBlock,
    ) -> Result<(), OperationError> {
        self.put_public_access_block_with(Authority::Native, request, configuration)
            .await
    }

    /// Replace all four public-access block flags together.
    /// Authorizes against current HTTP identity and bucket state.
    ///
    /// # Errors
    /// Preserves missing buckets, owner/access denial, corrupt state and
    /// storage or connection-lifecycle failures. Mutation errors retain their
    /// settlement outcome.
    pub async fn put_public_access_block_http(
        &self,
        identity: Arc<HttpIdentity>,
        request: BucketAccessRequest,
        configuration: PublicAccessBlock,
    ) -> Result<(), OperationError> {
        self.put_public_access_block_with(Authority::Http(identity), request, configuration)
            .await
    }

    async fn put_public_access_block_with(
        &self,
        authority: Authority,
        request: BucketAccessRequest,
        configuration: PublicAccessBlock,
    ) -> Result<(), OperationError> {
        let operation = self
            .admit()
            .map_err(|error| OperationError::from(error).uncommitted())?;

        let _cancellation = operation.cancel_on_drop();
        let (reply, response) = oneshot::channel();

        self.queue_bucket_configuration(Command::PutPublicAccessBlock {
            operation,
            authority,
            request,
            configuration,
            reply,
        })
        .await
        .map_err(OperationError::uncommitted)?;

        response.await.map_err(|_| {
            OperationError::new(OperationFailure::IoError).mutation(MutationOutcome::Unknown)
        })?
    }

    /// Read the current public-access block flags.
    /// Uses trusted native authority on the bounded metadata worker.
    ///
    /// # Errors
    /// Returns `NoSuchPublicAccessBlockConfiguration` when configuration is absent.
    /// Preserves missing buckets, owner/access denial, corrupt state and
    /// storage or connection-lifecycle failures.
    pub async fn get_public_access_block(
        &self,
        request: BucketAccessRequest,
    ) -> Result<PublicAccessBlock, OperationError> {
        self.get_public_access_block_with(Authority::Native, request)
            .await
    }

    /// Read the current public-access block flags.
    /// Authorizes against current HTTP identity and bucket state.
    ///
    /// # Errors
    /// Returns `NoSuchPublicAccessBlockConfiguration` when configuration is absent.
    /// Preserves missing buckets, owner/access denial, corrupt state and
    /// storage or connection-lifecycle failures.
    pub async fn get_public_access_block_http(
        &self,
        identity: Arc<HttpIdentity>,
        request: BucketAccessRequest,
    ) -> Result<PublicAccessBlock, OperationError> {
        self.get_public_access_block_with(Authority::Http(identity), request)
            .await
    }

    async fn get_public_access_block_with(
        &self,
        authority: Authority,
        request: BucketAccessRequest,
    ) -> Result<PublicAccessBlock, OperationError> {
        let operation = self.admit()?;
        let (reply, response) = oneshot::channel();

        self.queue_bucket_configuration(Command::GetPublicAccessBlock {
            operation,
            authority,
            request,
            reply,
        })
        .await?;

        response
            .await
            .map_err(|_| OperationError::new(OperationFailure::IoError))?
    }

    /// Remove public-access block configuration; absence is a successful no-op.
    /// Uses trusted native authority on the bounded metadata worker.
    ///
    /// # Errors
    /// Preserves missing buckets, owner/access denial, corrupt state and
    /// storage or connection-lifecycle failures. Mutation errors retain their
    /// settlement outcome.
    pub async fn delete_public_access_block(
        &self,
        request: BucketAccessRequest,
    ) -> Result<(), OperationError> {
        self.delete_public_access_block_with(Authority::Native, request)
            .await
    }

    /// Remove public-access block configuration; absence is a successful no-op.
    /// Authorizes against current HTTP identity and bucket state.
    ///
    /// # Errors
    /// Preserves missing buckets, owner/access denial, corrupt state and
    /// storage or connection-lifecycle failures. Mutation errors retain their
    /// settlement outcome.
    pub async fn delete_public_access_block_http(
        &self,
        identity: Arc<HttpIdentity>,
        request: BucketAccessRequest,
    ) -> Result<(), OperationError> {
        self.delete_public_access_block_with(Authority::Http(identity), request)
            .await
    }

    async fn delete_public_access_block_with(
        &self,
        authority: Authority,
        request: BucketAccessRequest,
    ) -> Result<(), OperationError> {
        let operation = self
            .admit()
            .map_err(|error| OperationError::from(error).uncommitted())?;

        let _cancellation = operation.cancel_on_drop();
        let (reply, response) = oneshot::channel();

        self.queue_bucket_configuration(Command::DeletePublicAccessBlock {
            operation,
            authority,
            request,
            reply,
        })
        .await
        .map_err(OperationError::uncommitted)?;

        response.await.map_err(|_| {
            OperationError::new(OperationFailure::IoError).mutation(MutationOutcome::Unknown)
        })?
    }

    /// Validate the fixed `BucketOwnerEnforced` ownership mode.
    /// Uses trusted native authority on the bounded metadata worker.
    ///
    /// # Errors
    /// Preserves missing buckets, owner/access denial, corrupt state and
    /// storage or connection-lifecycle failures. Mutation errors retain their
    /// settlement outcome.
    pub async fn put_bucket_ownership_controls(
        &self,
        request: BucketAccessRequest,
    ) -> Result<(), OperationError> {
        self.put_bucket_ownership_controls_with(Authority::Native, request)
            .await
    }

    /// Validate the fixed `BucketOwnerEnforced` ownership mode.
    /// Authorizes against current HTTP identity and bucket state.
    ///
    /// # Errors
    /// Preserves missing buckets, owner/access denial, corrupt state and
    /// storage or connection-lifecycle failures. Mutation errors retain their
    /// settlement outcome.
    pub async fn put_bucket_ownership_controls_http(
        &self,
        identity: Arc<HttpIdentity>,
        request: BucketAccessRequest,
    ) -> Result<(), OperationError> {
        self.put_bucket_ownership_controls_with(Authority::Http(identity), request)
            .await
    }

    async fn put_bucket_ownership_controls_with(
        &self,
        authority: Authority,
        request: BucketAccessRequest,
    ) -> Result<(), OperationError> {
        let operation = self
            .admit()
            .map_err(|error| OperationError::from(error).uncommitted())?;

        let _cancellation = operation.cancel_on_drop();
        let (reply, response) = oneshot::channel();

        self.queue_bucket_configuration(Command::PutBucketOwnershipControls {
            operation,
            authority,
            request,
            reply,
        })
        .await
        .map_err(OperationError::uncommitted)?;

        response.await.map_err(|_| {
            OperationError::new(OperationFailure::IoError).mutation(MutationOutcome::Unknown)
        })?
    }

    /// Validate the bucket and its fixed `BucketOwnerEnforced` ownership mode.
    /// Uses trusted native authority on the bounded metadata worker.
    ///
    /// # Errors
    /// Preserves missing buckets, owner/access denial, corrupt state and
    /// storage or connection-lifecycle failures.
    pub async fn get_bucket_ownership_controls(
        &self,
        request: BucketAccessRequest,
    ) -> Result<(), OperationError> {
        self.get_bucket_ownership_controls_with(Authority::Native, request)
            .await
    }

    /// Validate the bucket and its fixed `BucketOwnerEnforced` ownership mode.
    /// Authorizes against current HTTP identity and bucket state.
    ///
    /// # Errors
    /// Preserves missing buckets, owner/access denial, corrupt state and
    /// storage or connection-lifecycle failures.
    pub async fn get_bucket_ownership_controls_http(
        &self,
        identity: Arc<HttpIdentity>,
        request: BucketAccessRequest,
    ) -> Result<(), OperationError> {
        self.get_bucket_ownership_controls_with(Authority::Http(identity), request)
            .await
    }

    async fn get_bucket_ownership_controls_with(
        &self,
        authority: Authority,
        request: BucketAccessRequest,
    ) -> Result<(), OperationError> {
        let operation = self.admit()?;
        let (reply, response) = oneshot::channel();

        self.queue_bucket_configuration(Command::GetBucketOwnershipControls {
            operation,
            authority,
            request,
            reply,
        })
        .await?;

        response
            .await
            .map_err(|_| OperationError::new(OperationFailure::IoError))?
    }

    /// Read the owner of the bucket's fixed private ACL.
    /// Uses trusted native authority on the bounded metadata worker.
    ///
    /// # Errors
    /// Preserves missing buckets, owner/access denial, corrupt state and
    /// storage or connection-lifecycle failures.
    pub async fn get_bucket_acl(
        &self,
        request: BucketAccessRequest,
    ) -> Result<OwnerInfo, OperationError> {
        self.get_bucket_acl_with(Authority::Native, request).await
    }

    /// Read the owner of the bucket's fixed private ACL.
    /// Authorizes against current HTTP identity and bucket state.
    ///
    /// # Errors
    /// Preserves missing buckets, owner/access denial, corrupt state and
    /// storage or connection-lifecycle failures.
    pub async fn get_bucket_acl_http(
        &self,
        identity: Arc<HttpIdentity>,
        request: BucketAccessRequest,
    ) -> Result<OwnerInfo, OperationError> {
        self.get_bucket_acl_with(Authority::Http(identity), request)
            .await
    }

    async fn get_bucket_acl_with(
        &self,
        authority: Authority,
        request: BucketAccessRequest,
    ) -> Result<OwnerInfo, OperationError> {
        let operation = self.admit()?;
        let (reply, response) = oneshot::channel();

        self.queue_bucket_configuration(Command::GetBucketAcl {
            operation,
            authority,
            request,
            reply,
        })
        .await?;

        response
            .await
            .map_err(|_| OperationError::new(OperationFailure::IoError))?
    }

    async fn queue_bucket_configuration(&self, command: Command) -> Result<(), OperationError> {
        let permit =
            tokio::time::timeout(ADMISSION_TIMEOUT, self.owner.state.context.sender.reserve())
                .await
                .map_err(|_| OperationError::new(OperationFailure::StoreBusy))?
                .map_err(|_| OperationError::new(OperationFailure::IoError))?;

        permit.send(WorkerCommand::BucketConfiguration(command));
        Ok(())
    }
}

pub(super) fn run(command: Command, store: Option<&mut OpenedStore>) {
    match command {
        Command::PutBucketCors {
            operation,
            authority,
            request,
            configuration,
            reply,
        } => {
            let result = (|| {
                let _commit = operation
                    .enter_mutation()
                    .map_err(OperationError::uncommitted)?;

                let store = store.ok_or_else(|| OperationError::new(OperationFailure::IoError))?;

                store
                    .database_mut()
                    .put_bucket_cors(&authority, &request, &configuration)
            })();

            let _delivery = reply.send(result.map_err(mutation_error));
        }
        Command::GetBucketCors {
            operation,
            authority,
            request,
            reply,
        } => {
            let result = (|| {
                let _operation = operation;
                let store = store.ok_or_else(|| OperationError::new(OperationFailure::IoError))?;
                store.database_mut().get_bucket_cors(&authority, &request)
            })();

            let _delivery = reply.send(result);
        }
        Command::DeleteBucketCors {
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
                    .delete_bucket_cors(&authority, &request)
            })();

            let _delivery = reply.send(result.map_err(mutation_error));
        }
        Command::CorsForResponse {
            operation,
            bucket,
            reply,
        } => {
            let result = (|| {
                let _operation = operation;
                let store = store.ok_or_else(|| OperationError::new(OperationFailure::IoError))?;
                store.database_mut().cors_for_response(&bucket)
            })();

            let _delivery = reply.send(result);
        }
        Command::PutBucketPolicy {
            operation,
            authority,
            request,
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
                    .put_bucket_policy(&authority, &request, &document)
            })();

            let _delivery = reply.send(result.map_err(mutation_error));
        }
        Command::GetBucketPolicy {
            operation,
            authority,
            request,
            reply,
        } => {
            let result = (|| {
                let _operation = operation;
                let store = store.ok_or_else(|| OperationError::new(OperationFailure::IoError))?;
                store.database_mut().get_bucket_policy(&authority, &request)
            })();

            let _delivery = reply.send(result);
        }
        Command::DeleteBucketPolicy {
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
                    .delete_bucket_policy(&authority, &request)
            })();

            let _delivery = reply.send(result.map_err(mutation_error));
        }
        Command::GetBucketPolicyStatus {
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
                    .get_bucket_policy_status(&authority, &request)
            })();

            let _delivery = reply.send(result);
        }
        Command::PutPublicAccessBlock {
            operation,
            authority,
            request,
            configuration,
            reply,
        } => {
            let result = (|| {
                let _commit = operation
                    .enter_mutation()
                    .map_err(OperationError::uncommitted)?;

                let store = store.ok_or_else(|| OperationError::new(OperationFailure::IoError))?;

                store
                    .database_mut()
                    .put_public_access_block(&authority, &request, configuration)
            })();

            let _delivery = reply.send(result.map_err(mutation_error));
        }
        Command::GetPublicAccessBlock {
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
                    .get_public_access_block(&authority, &request)
            })();

            let _delivery = reply.send(result);
        }
        Command::DeletePublicAccessBlock {
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
                    .delete_public_access_block(&authority, &request)
            })();

            let _delivery = reply.send(result.map_err(mutation_error));
        }
        Command::PutBucketOwnershipControls {
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
                    .put_bucket_ownership_controls(&authority, &request)
            })();

            let _delivery = reply.send(result.map_err(mutation_error));
        }
        Command::GetBucketOwnershipControls {
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
                    .get_bucket_ownership_controls(&authority, &request)
            })();

            let _delivery = reply.send(result);
        }
        Command::GetBucketAcl {
            operation,
            authority,
            request,
            reply,
        } => {
            let result = (|| {
                let _operation = operation;
                let store = store.ok_or_else(|| OperationError::new(OperationFailure::IoError))?;
                store.database_mut().get_bucket_acl(&authority, &request)
            })();

            let _delivery = reply.send(result);
        }
    }
}
