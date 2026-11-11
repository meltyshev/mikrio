//! Stream orchestration keeps payload I/O outside the metadata worker.

use std::future::{Future, poll_fn};
use std::path::PathBuf;
use std::pin::pin;
use std::sync::Arc;
use std::task::Poll;
use std::time::Duration;

use tokio::io::AsyncRead;
use tokio::sync::{Semaphore, oneshot};

use super::{ADMISSION_TIMEOUT, Command as WorkerCommand, Context, Operation, mutation_error};
use crate::integrity::{ExpectedDigests, PayloadFacts};
use crate::object::{Authority, PutRequest, ReadRequest, StoredObject};
use crate::{
    BlobId, Connection, Guard, GuardMode, GuardTarget, MetadataError, MutationOutcome, OpenedStore,
    OperationError, OperationFailure, StoreId,
};

mod attributes;
pub(super) mod copy;
mod reader;
pub(super) mod source;
pub(super) mod write;

pub(super) const CHUNK_BYTES: usize = 64 * 1024;
static IO_SLOTS: Semaphore = Semaphore::const_new(8);
pub(super) static STREAM_SLOTS: Semaphore = Semaphore::const_new(64);

pub(super) enum Command {
    Copy(copy::Command),
    Attributes {
        _operation: Operation,
        authority: Authority,
        request: crate::object::AttributesRequest,
        reply: oneshot::Sender<Result<crate::object::StoredAttributes, OperationError>>,
    },
    Reserve {
        operation: Arc<Operation>,
        _guard: Arc<Guard>,
        authority: Authority,
        request: Arc<PutRequest>,
        blob_id: BlobId,
        reply: oneshot::Sender<Result<Option<[u8; 16]>, OperationError>>,
    },
    Publish {
        operation: Arc<Operation>,
        _guard: Arc<Guard>,
        authority: Authority,
        request: Arc<PutRequest>,
        bucket_id: [u8; 16],
        blob_id: BlobId,
        facts: PayloadFacts,
        reply: oneshot::Sender<Result<PayloadFacts, OperationError>>,
    },
    Select {
        _operation: Arc<Operation>,
        _guard: Option<Arc<Guard>>,
        authority: Authority,
        request: ReadRequest,
        reply: oneshot::Sender<Result<StoredObject, OperationError>>,
    },
    GetAcl {
        _operation: Operation,
        authority: Authority,
        request: ReadRequest,
        reply: oneshot::Sender<Result<crate::access::OwnerInfo, OperationError>>,
    },
    Delete {
        operation: Operation,
        authority: Authority,
        request: ReadRequest,
        if_match: Option<String>,
        reply: oneshot::Sender<Result<(), OperationError>>,
    },
}

impl Connection {
    /// Stream a trusted-native PUT through reservation, durable bytes and publication.
    ///
    /// # Errors
    /// Reports source, integrity, current condition/identity, lifecycle and storage
    /// failures with a mutation outcome. Cancellation never replays the source.
    pub async fn put_object<R: AsyncRead + Send + 'static>(
        &self,
        request: PutRequest,
        source: R,
    ) -> Result<PayloadFacts, OperationError> {
        self.put(request, source, Authority::Native, || {
            Ok(ExpectedDigests::default())
        })
        .await
    }

    /// Stream an authenticated PUT, rechecking current access at publication.
    /// The terminal decoder runs after decoded EOF and before durable publication;
    /// its returned expectations are checked against the same payload hashes.
    ///
    /// # Errors
    /// Returns the same storage/outcome errors as native PUT, plus current denial.
    pub async fn put_object_http<R: AsyncRead + Send + 'static>(
        &self,
        identity: Arc<crate::access::HttpIdentity>,
        request: PutRequest,
        source: R,
        terminal: impl FnOnce() -> std::io::Result<ExpectedDigests> + Send + 'static,
    ) -> Result<PayloadFacts, OperationError> {
        self.put(request, source, Authority::Http(identity), terminal)
            .await
    }

    async fn put<R: AsyncRead + Send + 'static>(
        &self,
        request: PutRequest,
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

        // The operation ticket owns this driver through settlement. Close cancels
        // source waits and joins the ticket; every native call retains its own Arc.
        drop(tokio::spawn(async move {
            let result = contain(run_put(
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

    /// Select detached metadata in one current trusted-native snapshot.
    ///
    /// # Errors
    /// Preserves missing/condition/owner/corrupt state and native lifecycle errors.
    pub async fn head_object(&self, request: ReadRequest) -> Result<StoredObject, OperationError> {
        let operation = Arc::new(self.admit()?);
        select(operation, Authority::Native, request, None).await
    }

    /// Select HTTP metadata and permission-dependent disclosure in one snapshot.
    ///
    /// # Errors
    /// Authorizes before missing-key or conditional disclosure; fails closed.
    pub async fn head_object_http(
        &self,
        identity: Arc<crate::access::HttpIdentity>,
        request: ReadRequest,
    ) -> Result<StoredObject, OperationError> {
        let operation = Arc::new(self.admit()?);
        select(operation, Authority::Http(identity), request, None).await
    }

    /// Read the fixed ACL owner from the current object's metadata snapshot.
    ///
    /// # Errors
    /// Preserves missing object/bucket, owner, corruption and lifecycle failures.
    pub async fn get_object_acl(
        &self,
        request: ReadRequest,
    ) -> Result<crate::access::OwnerInfo, OperationError> {
        self.object_acl(request, Authority::Native).await
    }

    /// Authorize the distinct ACL permission against the current object and tags.
    ///
    /// # Errors
    /// Current denial precedes missing-key disclosure; no payload guard is needed.
    pub async fn get_object_acl_http(
        &self,
        identity: Arc<crate::access::HttpIdentity>,
        request: ReadRequest,
    ) -> Result<crate::access::OwnerInfo, OperationError> {
        self.object_acl(request, Authority::Http(identity)).await
    }

    async fn object_acl(
        &self,
        request: ReadRequest,
        authority: Authority,
    ) -> Result<crate::access::OwnerInfo, OperationError> {
        let operation = self.admit()?;
        let (reply, response) = oneshot::channel();

        queue(
            &self.owner.state.context,
            Command::GetAcl {
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

    /// Logically delete one current object without unlinking its payload.
    ///
    /// # Errors
    /// Conditional missing targets fail; unconditional absence in a live bucket
    /// succeeds. Mutation failures preserve their authoritative outcome.
    pub async fn delete_object(
        &self,
        request: ReadRequest,
        if_match: Option<String>,
    ) -> Result<(), OperationError> {
        self.delete(request, if_match, Authority::Native).await
    }

    /// Authorize and delete an HTTP object in the same writer transaction.
    ///
    /// # Errors
    /// Current denial precedes condition disclosure; errors retain mutation outcome.
    pub async fn delete_object_http(
        &self,
        identity: Arc<crate::access::HttpIdentity>,
        request: ReadRequest,
        if_match: Option<String>,
    ) -> Result<(), OperationError> {
        self.delete(request, if_match, Authority::Http(identity))
            .await
    }

    async fn delete(
        &self,
        request: ReadRequest,
        if_match: Option<String>,
        authority: Authority,
    ) -> Result<(), OperationError> {
        let operation = self
            .admit()
            .map_err(|error| OperationError::from(error).uncommitted())?;

        let _cancellation = operation.cancel_on_drop();
        let (reply, response) = oneshot::channel();

        queue(
            &self.owner.state.context,
            Command::Delete {
                operation,
                authority,
                request,
                if_match,
                reply,
            },
        )
        .await
        .map_err(OperationError::uncommitted)?;

        response.await.map_err(|_| unknown())?
    }
}

impl Operation {
    pub(super) async fn interrupted<T>(
        &self,
        future: impl Future<Output = T>,
    ) -> Result<T, OperationError> {
        let mut cancellation = self.cancellation.subscribe();
        let mut closing = self.state.closing.subscribe();

        let mut listener = self
            .listener
            .as_ref()
            .map(|scope| scope.closing.subscribe());

        if *closing.borrow()
            || *cancellation.borrow()
            || listener.as_ref().is_some_and(|closing| *closing.borrow())
        {
            return Err(OperationError::new(OperationFailure::OperationCanceled));
        }

        let mut canceled = pin!(cancellation.changed());
        let mut closed = pin!(closing.changed());

        let mut listener_closed = pin!(async {
            match &mut listener {
                Some(closing) => {
                    let _changed = closing.changed().await;
                }
                None => std::future::pending().await,
            }
        });

        let mut future = pin!(future);

        poll_fn(|context| {
            if canceled.as_mut().poll(context).is_ready()
                || closed.as_mut().poll(context).is_ready()
                || listener_closed.as_mut().poll(context).is_ready()
            {
                return Poll::Ready(Err(OperationError::new(
                    OperationFailure::OperationCanceled,
                )));
            }

            future.as_mut().poll(context).map(Ok)
        })
        .await
    }
}

pub(super) async fn contain<T>(
    future: impl Future<Output = Result<T, OperationError>>,
) -> Result<T, OperationError> {
    let mut future = pin!(future);

    poll_fn(|context| {
        std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            future.as_mut().poll(context)
        }))
        .unwrap_or_else(|_| Poll::Ready(Err(unknown())))
    })
    .await
}

async fn run_put<R: AsyncRead + Send + 'static>(
    operation: Arc<Operation>,
    path: PathBuf,
    authority: Authority,
    request: Arc<PutRequest>,
    source: R,
    terminal: impl FnOnce() -> std::io::Result<ExpectedDigests> + Send + 'static,
    _stream_slot: tokio::sync::SemaphorePermit<'static>,
) -> Result<PayloadFacts, OperationError> {
    let verifier = request
        .integrity
        .verifier(request.content_length.map(crate::object::ByteLength::get))?;

    let mut reservation = None;

    for _ in 0..4 {
        let blob_id = BlobId::from_bytes(
            *StoreId::generate()
                .map_err(MetadataError::Entropy)?
                .as_bytes(),
        );

        #[cfg(test)]
        let blob_id = operation
            .state
            .context
            .test_hooks
            .put_outputs
            .lock()
            .unwrap()
            .pop_front()
            .unwrap_or(blob_id);

        let guard = acquire(Arc::clone(&operation), path.clone(), blob_id).await?;

        let output = write::Output {
            operation: Arc::clone(&operation),
            path: path.clone(),
            blob_id,
            guard,
        };

        let available = output.available().await?;

        if !available {
            continue;
        }

        let (reply, response) = oneshot::channel();

        queue(
            &operation.state.context,
            Command::Reserve {
                operation: Arc::clone(&operation),
                _guard: Arc::clone(&output.guard),
                authority: authority.clone(),
                request: Arc::clone(&request),
                blob_id,
                reply,
            },
        )
        .await?;

        // Reservation uncertainty cannot publish this logical PUT; no file is
        // created unless its actual reservation acknowledgment is established.
        if let Some(bucket_id) = response
            .await
            .map_err(|_| OperationError::new(OperationFailure::IoError))??
        {
            reservation = Some((output, bucket_id));
            break;
        }
    }

    let (output, bucket_id) =
        reservation.ok_or_else(|| OperationError::new(OperationFailure::IoError))?;

    let blob_id = output.blob_id;

    let (guard, facts) = output.publish(source, verifier, terminal).await?;

    let (reply, response) = oneshot::channel();

    queue(
        &operation.state.context,
        Command::Publish {
            operation: Arc::clone(&operation),
            _guard: guard,
            authority,
            request,
            bucket_id,
            blob_id,
            facts,
            reply,
        },
    )
    .await?;

    response.await.map_err(|_| unknown())?
}

pub(super) async fn acquire(
    operation: Arc<Operation>,
    path: PathBuf,
    blob_id: BlobId,
) -> Result<Arc<Guard>, OperationError> {
    #[cfg(test)]
    let mut observation = crate::test_metrics::GuardWait::start();

    let deadline = tokio::time::Instant::now() + ADMISSION_TIMEOUT;

    loop {
        let path = path.clone();
        let owner = Arc::clone(&operation);

        let guard = native(move || {
            Ok(owner
                .guards(&path)?
                .try_acquire(GuardTarget::Shard(blob_id.shard()), GuardMode::Shared)?)
        })
        .await?;

        if let Some(guard) = guard {
            #[cfg(test)]
            observation.acquired();

            return Ok(Arc::new(guard));
        }

        #[cfg(test)]
        observation.refused();

        if tokio::time::Instant::now() >= deadline {
            return Err(OperationError::new(OperationFailure::StoreBusy));
        }

        #[cfg(test)]
        let backoff = std::time::Instant::now();

        operation
            .interrupted(tokio::time::sleep(Duration::from_millis(10)))
            .await?;

        #[cfg(test)]
        crate::test_metrics::GuardWait::backoff(backoff.elapsed());
    }
}

pub(super) async fn native<T: Send + 'static>(
    work: impl FnOnce() -> Result<T, OperationError> + Send + 'static,
) -> Result<T, OperationError> {
    let permit = tokio::time::timeout(ADMISSION_TIMEOUT, IO_SLOTS.acquire())
        .await
        .map_err(|_| OperationError::new(OperationFailure::StoreBusy))?
        .map_err(|_| OperationError::new(OperationFailure::IoError))?;

    tokio::task::spawn_blocking(move || {
        let _permit = permit;
        work()
    })
    .await
    .map_err(|_| unknown())?
}

pub(super) async fn select(
    operation: Arc<Operation>,
    authority: Authority,
    request: ReadRequest,
    guard: Option<Arc<Guard>>,
) -> Result<StoredObject, OperationError> {
    let (reply, response) = oneshot::channel();

    queue(
        &operation.state.context,
        Command::Select {
            _operation: Arc::clone(&operation),
            _guard: guard,
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

async fn queue(context: &Context, command: Command) -> Result<(), OperationError> {
    let permit = tokio::time::timeout(ADMISSION_TIMEOUT, context.sender.reserve())
        .await
        .map_err(|_| OperationError::new(OperationFailure::StoreBusy))?
        .map_err(|_| OperationError::new(OperationFailure::IoError))?;

    permit.send(WorkerCommand::Object(command));
    Ok(())
}

pub(super) fn unknown() -> OperationError {
    OperationError::new(OperationFailure::IoError).mutation(MutationOutcome::Unknown)
}

pub(super) fn run(command: Command, store: Option<&mut OpenedStore>) {
    match command {
        Command::Copy(command) => copy::run(command, store),
        Command::Reserve {
            operation,
            _guard,
            authority,
            request,
            blob_id,
            reply,
        } => {
            let result = (|| {
                // Internal reservation does not enter the logical publication fence.
                let is_canceled = *operation.cancellation.borrow()
                    || *operation.state.closing.borrow()
                    || operation
                        .listener
                        .as_ref()
                        .is_some_and(|scope| *scope.closing.borrow());

                if is_canceled {
                    return Err(OperationError::new(OperationFailure::OperationCanceled));
                }

                let store = store.ok_or_else(|| OperationError::new(OperationFailure::IoError))?;

                store
                    .database_mut()
                    .reserve_put(&authority, &request, blob_id)
            })();

            let _delivery = reply.send(result.map_err(OperationError::uncommitted));
        }
        Command::Publish {
            operation,
            _guard,
            authority,
            request,
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
                crate::protocol_fault::hit("commit_fence", "entered")?;

                let store = store.ok_or_else(|| OperationError::new(OperationFailure::IoError))?;

                store
                    .database_mut()
                    .publish_put(&authority, &request, bucket_id, blob_id, &facts)?;

                Ok(facts)
            })();

            let _delivery = reply.send(result.map_err(mutation_error));
        }
        Command::Select {
            _operation,
            _guard,
            authority,
            request,
            reply,
        } => {
            let result = store
                .ok_or_else(|| OperationError::new(OperationFailure::IoError))
                .and_then(|store| store.database_mut().select_object(&authority, &request));

            let _delivery = reply.send(result);
        }
        Command::GetAcl {
            _operation,
            authority,
            request,
            reply,
        } => {
            let result = store
                .ok_or_else(|| OperationError::new(OperationFailure::IoError))
                .and_then(|store| store.database_mut().get_object_acl(&authority, &request));

            let _delivery = reply.send(result);
        }
        Command::Attributes {
            _operation,
            authority,
            request,
            reply,
        } => {
            let result = store
                .ok_or_else(|| OperationError::new(OperationFailure::IoError))
                .and_then(|store| store.database_mut().select_attributes(&authority, &request));

            let _delivery = reply.send(result);
        }
        Command::Delete {
            operation,
            authority,
            request,
            if_match,
            reply,
        } => {
            let result = (|| {
                let _commit = operation
                    .enter_mutation()
                    .map_err(OperationError::uncommitted)?;

                let store = store.ok_or_else(|| OperationError::new(OperationFailure::IoError))?;

                store
                    .database_mut()
                    .delete_object(&authority, &request, if_match.as_deref())
            })();

            let _delivery = reply.send(result.map_err(mutation_error));
        }
    }
}
