//! Internal foreign-host delivery bridge; not a supported application API.

use crate::{OpenOptions, StorageError, Store};

/// Private backend response construction, independent of any transport SDK.
pub mod response;

/// Cancel further batch dispatch while waiting for authoritative item settlement.
///
/// # Errors
/// Preserves indexed progress and the ordinary batch error contract.
pub async fn delete_objects_until(
    store: &Store,
    input: crate::DeleteObjectsInput,
    canceled: impl std::future::Future<Output = ()> + Send,
) -> Result<crate::DeleteObjectsOutput, StorageError> {
    store.delete_objects_until(input, canceled).await
}

/// Detached listener termination future for foreign environment references.
/// It holds no server/store wrapper and does not initiate shutdown.
#[cfg(feature = "http")]
pub fn server_closed(
    server: &crate::Server,
) -> impl std::future::Future<Output = Result<(), StorageError>> + Send + use<> {
    let completion = server.listener.completion();
    async move { completion.wait().await.map_err(StorageError::from_core) }
}

/// Split detached GET facts from its unique body without copying metadata.
/// Foreign adapters own the body separately from their output allocation.
#[must_use]
pub fn split_get(output: crate::GetObjectOutput) -> (crate::HeadObjectOutput, crate::ObjectBody) {
    (
        crate::HeadObjectOutput {
            details: output.details,
        },
        output.body,
    )
}

/// Open the ordinary facade and detach its bounded host observation capability.
///
/// # Errors
/// Returns the same failures as [`Store::open`].
pub async fn open(options: OpenOptions) -> Result<(Store, Diagnostics), StorageError> {
    let store = Store::open(options).await?;
    let diagnostics = Diagnostics(store.connection.diagnostic_receiver());
    Ok((store, diagnostics))
}

/// Weak observation ownership: no connection, guard, transaction or body.
#[derive(Clone)]
pub struct Diagnostics(mikrio_core::diagnostics::Receiver);

impl Diagnostics {
    /// Whether two handles refer to the same context's diagnostic queue.
    #[must_use]
    pub fn same_queue(&self, other: &Self) -> bool {
        self.0.same_queue(&other.0)
    }

    /// Whether native shutdown revoked delivery or the context was destroyed.
    #[must_use]
    pub fn is_closed(&self) -> bool {
        self.0.is_closed()
    }

    /// Detach a bounded redacted human message. Text is not a parsing interface.
    #[must_use]
    pub fn take(&self) -> Option<String> {
        let (record, stats) = self.0.take()?;

        if let Some(observation) = record.cleanup() {
            return Some(format!(
                "mikrio: bounded cleanup sample store_id={} work=cleanup sampled_blobs={} observed_file_bytes={} oldest_sample_blob_age_ms={:?} unobserved_blobs={} more_pending={} blocked_shard_attempts={} age_ms={} dropped_observations={} observer_failures={}",
                record.store_id(),
                observation.sampled_blobs,
                observation.observed_file_bytes,
                observation.oldest_sample_blob_age_ms,
                observation.unobserved_blobs,
                observation.more_pending,
                observation.blocked_shard_attempts,
                record.age().as_millis(),
                stats.dropped,
                stats.observer_failures,
            ));
        }

        Some(format!(
            "mikrio: storage work needs attention store_id={} work={} failure={} occurrences={} age_ms={} dropped_observations={} observer_failures={}",
            record.store_id(),
            record.work().as_str(),
            record.failure().unwrap_or("unknown"),
            record.occurrences(),
            record.age().as_millis(),
            stats.dropped,
            stats.observer_failures,
        ))
    }

    /// Count a recoverable host observer failure without recursively logging.
    pub fn observer_failed(&self) {
        self.0.observer_failed();
    }
}
