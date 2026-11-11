//! Exact guarded completion; assembly never holds a metadata transaction.

use std::collections::BTreeMap;
use std::path::PathBuf;
use std::sync::Arc;

use tokio::sync::oneshot;

use crate::connection::objects::{STREAM_SLOTS, acquire, contain, source, unknown, write};
use crate::connection::{ADMISSION_TIMEOUT, Operation, mutation_error};
use crate::integrity::{ExpectedDigests, PayloadFacts, PayloadVerifier};
use crate::multipart::{CompleteRequest, CompletedUpload, CompletionClaim, CompletionSelection};
use crate::object::Authority;
use crate::{
    BlobId, Connection, Guard, MetadataError, OpenedStore, OperationError, OperationFailure,
    StoreId,
};

pub(in crate::connection) enum Command {
    Select {
        operation: Arc<Operation>,
        authority: Authority,
        request: Arc<CompleteRequest>,
        reply: oneshot::Sender<Result<CompletionSelection, OperationError>>,
    },
    Claim {
        operation: Arc<Operation>,
        _guards: Arc<[Arc<Guard>]>,
        authority: Authority,
        request: Arc<CompleteRequest>,
        selected: Arc<CompletionSelection>,
        claim: CompletionClaim,
        reply: oneshot::Sender<Result<bool, OperationError>>,
    },
    Publish {
        operation: Arc<Operation>,
        _guards: Arc<[Arc<Guard>]>,
        authority: Authority,
        request: Arc<CompleteRequest>,
        selected: Arc<CompletionSelection>,
        claim: CompletionClaim,
        facts: PayloadFacts,
        reply: oneshot::Sender<Result<CompletedUpload, OperationError>>,
    },
}

impl Connection {
    /// Assemble exactly the selected trusted-native parts and atomically publish their object.
    /// Cancellation retains native work and guards through settlement; a failed claim recovers
    /// to OPEN without discarding acknowledged parts unless abort or namespace retirement wins.
    ///
    /// # Errors
    /// Reports invalid selections, current conditions, integrity and storage failures with
    /// mutation outcomes. Repeating a completed upload ID is not an idempotent replay.
    pub async fn complete_multipart_upload(
        &self,
        request: CompleteRequest,
    ) -> Result<CompletedUpload, OperationError> {
        self.complete_multipart_upload_with(Authority::Native, request, None)
            .await
    }

    /// Complete an authenticated upload with current destination authority rechecked at commit.
    ///
    /// # Errors
    /// Revoked authority cannot publish; input, condition and storage failures preserve outcomes.
    pub async fn complete_multipart_upload_http(
        &self,
        identity: Arc<crate::access::HttpIdentity>,
        request: CompleteRequest,
    ) -> Result<CompletedUpload, OperationError> {
        self.complete_multipart_upload_with(Authority::Http(identity), request, None)
            .await
    }

    /// Complete while reporting completed worker steps to the HTTP adapter.
    ///
    /// # Errors
    /// Preserves the ordinary authenticated completion outcomes.
    pub async fn complete_multipart_upload_http_with_progress(
        &self,
        identity: Arc<crate::access::HttpIdentity>,
        request: CompleteRequest,
        progress: crate::WorkProgress,
    ) -> Result<CompletedUpload, OperationError> {
        self.complete_multipart_upload_with(Authority::Http(identity), request, Some(progress))
            .await
    }

    async fn complete_multipart_upload_with(
        &self,
        authority: Authority,
        request: CompleteRequest,
        progress: Option<crate::WorkProgress>,
    ) -> Result<CompletedUpload, OperationError> {
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

            let result = contain(complete(
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

async fn complete(
    operation: Arc<Operation>,
    path: PathBuf,
    authority: Authority,
    request: Arc<CompleteRequest>,
) -> Result<CompletedUpload, OperationError> {
    let (reply, response) = oneshot::channel();

    queue(
        &operation,
        Command::Select {
            operation: Arc::clone(&operation),
            authority: authority.clone(),
            request: Arc::clone(&request),
            reply,
        },
    )
    .await?;

    let selected = Arc::new(
        response
            .await
            .map_err(|_| OperationError::new(OperationFailure::IoError))??,
    );

    #[cfg(test)]
    crate::protocol_fault::hit("multipart.complete", "selected")?;

    let mut reserved = None;

    for _ in 0..4 {
        let claim = CompletionClaim {
            claim_id: *StoreId::generate()
                .map_err(MetadataError::Entropy)?
                .as_bytes(),
            blob_id: BlobId::from_bytes(
                *StoreId::generate()
                    .map_err(MetadataError::Entropy)?
                    .as_bytes(),
            ),
        };

        let shards = selected
            .parts
            .iter()
            .map(|part| part.blob_id)
            .chain([claim.blob_id])
            .map(|blob| (blob.shard(), blob))
            .collect::<BTreeMap<_, _>>();

        let mut guards = Vec::with_capacity(shards.len());
        let mut output_guard = None;

        for (shard, blob) in shards {
            let guard = acquire(Arc::clone(&operation), path.clone(), blob).await?;

            if shard == claim.blob_id.shard() {
                output_guard = Some(Arc::clone(&guard));
            }

            guards.push(guard);
        }

        let guards: Arc<[Arc<Guard>]> = guards.into();

        let output = write::Output {
            operation: Arc::clone(&operation),
            path: path.clone(),
            blob_id: claim.blob_id,
            guard: output_guard.ok_or_else(|| OperationError::new(OperationFailure::IoError))?,
        };

        if !output.available().await? {
            continue;
        }

        let (reply, response) = oneshot::channel();

        queue(
            &operation,
            Command::Claim {
                operation: Arc::clone(&operation),
                _guards: Arc::clone(&guards),
                authority: authority.clone(),
                request: Arc::clone(&request),
                selected: Arc::clone(&selected),
                claim,
                reply,
            },
        )
        .await?;

        if response
            .await
            .map_err(|_| OperationError::new(OperationFailure::IoError))??
        {
            reserved = Some((claim, output, guards));
            break;
        }
    }

    let (claim, output, guards) =
        reserved.ok_or_else(|| OperationError::new(OperationFailure::IoError))?;

    operation.progressed();

    #[cfg(test)]
    crate::protocol_fault::hit("multipart.complete", "claimed")?;

    let length = selected.layout.content_length();

    let verifier = PayloadVerifier::new(
        Some(length),
        length,
        selected.checksum.algorithm(),
        ExpectedDigests::default(),
    )?;

    let inputs = Arc::clone(&selected);
    let part_count = inputs.parts.len();

    let source = source::Source::new(
        Arc::clone(&operation),
        path,
        (0..part_count).map(move |index| {
            let part = &inputs.parts[index];

            source::Input {
                blob_id: part.blob_id,
                length: part.length.get(),
                md5: part.md5,
                checksum: Some(part.checksum),
            }
        }),
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
            selected,
            claim,
            facts,
            reply,
        },
    )
    .await?;

    response.await.map_err(|_| unknown())?
}

async fn queue(operation: &Operation, command: Command) -> Result<(), OperationError> {
    super::queue(&operation.state.context, super::Command::Complete(command)).await
}

pub(super) fn run(command: Command, store: Option<&mut OpenedStore>) {
    match command {
        Command::Select {
            operation,
            authority,
            request,
            reply,
        } => {
            let result = (|| {
                let _operation = operation;
                let store = store.ok_or_else(|| OperationError::new(OperationFailure::IoError))?;

                store.database_mut().select_completion(&authority, &request)
            })();

            let _delivery = reply.send(result);
        }

        Command::Claim {
            operation,
            _guards,
            authority,
            request,
            selected,
            claim,
            reply,
        } => {
            let result = (|| {
                operation.check_commit()?;
                let store = store.ok_or_else(|| OperationError::new(OperationFailure::IoError))?;

                store
                    .database_mut()
                    .claim_completion(&authority, &request, &selected, claim)
            })();

            // A provisional claim cannot publish an object; recovery preserves all parts.
            let _delivery = reply.send(result.map_err(OperationError::uncommitted));
        }

        Command::Publish {
            operation,
            _guards,
            authority,
            request,
            selected,
            claim,
            facts,
            reply,
        } => {
            let result = (|| {
                operation
                    .check_commit()
                    .map_err(OperationError::uncommitted)?;

                #[cfg(test)]
                crate::protocol_fault::hit("multipart.complete", "commit_fence")?;

                let store = store.ok_or_else(|| OperationError::new(OperationFailure::IoError))?;

                store
                    .database_mut()
                    .publish_completion(&authority, &request, &selected, claim, &facts)
            })();

            let _delivery = reply.send(result.map_err(mutation_error));
        }
    }
}
