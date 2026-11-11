//! Bounded read-only observations, separate from deletion authority and delivery.

use std::io;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::{Duration, Instant};

use tokio::sync::oneshot;

use super::{Command, Operation, native, queue, unavailable};
use crate::diagnostics::{CleanupObservation, Diagnostic};
use crate::{BlobId, GuardMode, GuardTarget, OperationError};

#[derive(Default)]
pub(super) struct Sampler {
    sampled_at: Option<Instant>,
    after: Option<(u8, i64)>,
    blocked: u64,
    had_pending: bool,
}

impl Sampler {
    pub(super) fn blocked(&mut self) {
        self.blocked = self.blocked.saturating_add(1);
    }

    pub(super) async fn observe(
        &mut self,
        operation: Arc<Operation>,
        path: PathBuf,
    ) -> Result<(), OperationError> {
        if self
            .sampled_at
            .is_some_and(|last| last.elapsed() < Duration::from_secs(10))
        {
            return Ok(());
        }

        if *operation.state.closing.borrow() {
            return Ok(());
        }

        // Errors and full foreground queues are paced too. Observation never
        // retries in a tight loop or bypasses the normal native-work limits.
        self.sampled_at = Some(Instant::now());
        let (reply, response) = oneshot::channel();

        queue(
            &operation,
            Command::Sample {
                after: self.after,
                reply,
                _operation: Arc::clone(&operation),
            },
        )?;

        let page = response.await.map_err(|_| unavailable())??;
        self.after = page.after;
        let blocked = std::mem::take(&mut self.blocked);
        let owner = Arc::clone(&operation);

        let observation = native(move || {
            let started = Instant::now();

            let now = crate::EpochMillis::from_system_time(std::time::SystemTime::now())
                .map_err(io::Error::other)?;

            let mut observation = CleanupObservation {
                sampled_blobs: u32::try_from(page.blobs.len()).map_err(io::Error::other)?,
                oldest_sample_blob_age_ms: page
                    .oldest_created_at_ms
                    .map(|created| u64::try_from(now.get().saturating_sub(created).max(0)))
                    .transpose()
                    .map_err(io::Error::other)?,
                more_pending: page.more_pending,
                blocked_shard_attempts: blocked,
                ..CleanupObservation::default()
            };

            for blob_id in page.blobs {
                if *owner.state.closing.borrow() || started.elapsed() >= Duration::from_millis(50) {
                    observation.unobserved_blobs += 1;
                    continue;
                }

                match file_length(&owner, &path, blob_id) {
                    Ok(Some(length)) => {
                        observation.observed_file_bytes =
                            observation.observed_file_bytes.saturating_add(length);
                    }
                    Ok(None) | Err(_) => observation.unobserved_blobs += 1,
                }
            }

            Ok(observation)
        })
        .await?;

        let has_pending = observation.sampled_blobs != 0 || observation.more_pending;

        if (has_pending || self.had_pending || observation.blocked_shard_attempts != 0)
            && let Some(store_id) = operation.state.context.store_id.get()
        {
            operation
                .state
                .context
                .diagnostics
                .record(Diagnostic::cleanup_observation(*store_id, observation));
        }

        self.had_pending = has_pending;
        Ok(())
    }
}

fn file_length(
    operation: &Operation,
    path: &Path,
    blob_id: BlobId,
) -> Result<Option<u64>, OperationError> {
    // The preliminary ledger page is only a hint. Shared exclusion protects
    // this observation from GC; it neither blocks writers nor authorizes unlink.
    let Some(_guard) = operation
        .guards(path)?
        .try_acquire(GuardTarget::Shard(blob_id.shard()), GuardMode::Shared)?
    else {
        return Ok(None);
    };

    let root = operation.directory(path)?;
    let shard = format!("{:02x}", blob_id.shard());

    for parent in ["staging", "objects"] {
        let directory = root.open_child(parent)?.open_child(&shard)?;

        match directory.open_file(&blob_id.to_string()) {
            // Return at the first path: a concurrent publication can rename the
            // same inode between probes, which must not count its bytes twice.
            Ok(file) => return Ok(Some(file.metadata()?.len())),
            Err(error) if error.kind() == io::ErrorKind::NotFound => {}
            Err(error) => return Err(error.into()),
        }
    }

    // A reservation need not have created a file; completed GC can also win
    // before our shared acquisition. Absence is an observation, never corruption.
    Ok(Some(0))
}
