//! Internal detached diagnostic records. Human output is not a stable API.
//!
//! Producers never invoke host code. Consumers remove an owned record before
//! calling a sink and must run outside storage workers/critical sections.

use std::collections::VecDeque;
use std::fmt;
use std::io;
use std::sync::atomic::{AtomicBool, AtomicU64, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex, Weak};
use std::time::{Duration, Instant};

use crate::StoreId;

const CAPACITY: usize = 16;

/// Finite operation categories; never a user-supplied operation/path string.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Work {
    /// Metadata worker/storage I/O.
    Metadata,
    /// Deferred physical cleanup.
    Cleanup,
}

impl Work {
    /// Detached human diagnostic category.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Metadata => "metadata",
            Self::Cleanup => "cleanup",
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Failure {
    Corruption,
    Permission,
    Capacity,
    OtherIo,
}

/// One bounded maintenance sample, never an atomic store-wide inventory.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct CleanupObservation {
    /// Pending ledger rows selected, including still-active reservations.
    pub sampled_blobs: u32,
    /// File lengths actually observed under shared protection, not allocated blocks.
    pub observed_file_bytes: u64,
    /// Oldest sampled blob's wall-clock age since creation, not time queued for GC.
    /// Absent for an empty sample; a future creation timestamp has age zero.
    pub oldest_sample_blob_age_ms: Option<u64>,
    /// Selected rows whose files could not be observed (busy, failed or budgeted out).
    pub unobserved_blobs: u32,
    /// More pending rows followed this bounded page at selection time.
    pub more_pending: bool,
    /// Actual refused exclusive shard acquisitions since the preceding sample.
    pub blocked_shard_attempts: u64,
}

impl Failure {
    const fn as_str(self) -> &'static str {
        match self {
            Self::Corruption => "corruption",
            Self::Permission => "permission",
            Self::Capacity => "capacity",
            Self::OtherIo => "io",
        }
    }
}

/// Only allowlisted fixed-size facts enter delivery. Error display/debug/source
/// chains, paths, keys, credentials, URLs, headers and body bytes are omitted.
#[derive(Clone, Debug)]
pub struct Diagnostic {
    store_id: StoreId,
    work: Work,
    failure: Option<Failure>,
    cleanup: Option<CleanupObservation>,
    occurrences: u64,
    first_seen: Instant,
    last_seen: Instant,
}

impl Diagnostic {
    /// Classify an I/O failure without retaining or formatting its details.
    #[must_use]
    pub fn io(store_id: StoreId, work: Work, error: &io::Error) -> Self {
        let failure = match error.kind() {
            io::ErrorKind::PermissionDenied => Failure::Permission,
            io::ErrorKind::StorageFull => Failure::Capacity,
            _ => Failure::OtherIo,
        };

        Self::new(store_id, work, failure)
    }

    pub(crate) fn operation(store_id: StoreId, work: Work, error: &crate::OperationError) -> Self {
        if error.failure() == crate::OperationFailure::StoreCorrupt {
            return Self::new(store_id, work, Failure::Corruption);
        }

        // Native maintenance errors have bounded internal wrappers. Preserve a
        // coarse native class without formatting or retaining any cause data.
        let mut source: Option<&(dyn std::error::Error + 'static)> = Some(error);

        for _ in 0..8 {
            let Some(error) = source else {
                break;
            };

            if let Some(error) = error.downcast_ref::<io::Error>() {
                return Self::io(store_id, work, error);
            }

            source = error.source();
        }

        Self::new(store_id, work, Failure::OtherIo)
    }

    fn new(store_id: StoreId, work: Work, failure: Failure) -> Self {
        let now = Instant::now();

        Self {
            store_id,
            work,
            failure: Some(failure),
            cleanup: None,
            occurrences: 1,
            first_seen: now,
            last_seen: now,
        }
    }

    pub(crate) fn cleanup_observation(store_id: StoreId, observation: CleanupObservation) -> Self {
        let now = Instant::now();

        Self {
            store_id,
            work: Work::Cleanup,
            failure: None,
            cleanup: Some(observation),
            occurrences: 1,
            first_seen: now,
            last_seen: now,
        }
    }

    /// Opaque store correlation identity, not a filesystem path.
    #[must_use]
    pub const fn store_id(&self) -> StoreId {
        self.store_id
    }

    /// Human diagnostic category; no operation input is serialized.
    #[must_use]
    pub const fn work(&self) -> Work {
        self.work
    }

    /// Coarse failure class, independent of the original error's text.
    #[must_use]
    pub fn failure(&self) -> Option<&'static str> {
        self.failure.map(Failure::as_str)
    }

    /// Latest bounded sample when this is an observation rather than a failure.
    #[must_use]
    pub const fn cleanup(&self) -> Option<CleanupObservation> {
        self.cleanup
    }

    /// Saturating number of repeated observations coalesced into this record.
    #[must_use]
    pub const fn occurrences(&self) -> u64 {
        self.occurrences
    }

    /// Age of the newest observation, not a wall-clock event timestamp.
    #[must_use]
    pub fn age(&self) -> Duration {
        self.last_seen.elapsed()
    }

    /// Observed monotonic interval covered by this coalesced record.
    #[must_use]
    pub fn observation_span(&self) -> Duration {
        self.last_seen.saturating_duration_since(self.first_seen)
    }

    fn same_kind(&self, other: &Self) -> bool {
        self.store_id == other.store_id && self.work == other.work && self.failure == other.failure
    }
}

struct State {
    records: Mutex<VecDeque<Diagnostic>>,
    closed: AtomicBool,
    queued: AtomicUsize,
    dropped: AtomicU64,
    coalesced: AtomicU64,
    observer_failures: AtomicU64,
    host_delivery: AtomicBool,
}

/// Best-effort fixed-budget delivery. Producers never wait for queue capacity,
/// a contended diagnostic mutex, or a host observer. Clones share this queue.
#[derive(Clone)]
pub struct Diagnostics(Arc<State>);

impl Default for Diagnostics {
    fn default() -> Self {
        Self(Arc::new(State {
            records: Mutex::new(VecDeque::with_capacity(CAPACITY)),
            closed: AtomicBool::new(false),
            queued: AtomicUsize::new(0),
            dropped: AtomicU64::new(0),
            coalesced: AtomicU64::new(0),
            observer_failures: AtomicU64::new(0),
            host_delivery: AtomicBool::new(false),
        }))
    }
}

impl Diagnostics {
    /// Transfer delivery to an internal host adapter. The receiver keeps no
    /// engine resource alive; all independent connections share this queue.
    #[must_use]
    pub fn receiver(&self) -> Receiver {
        self.0.host_delivery.store(true, Ordering::Release);
        Receiver(Arc::downgrade(&self.0))
    }

    pub(crate) fn has_host_delivery(&self) -> bool {
        self.0.host_delivery.load(Ordering::Acquire)
    }

    /// Queue or coalesce a detached observation. Capacity/lock contention and
    /// shutdown discard it with accounting, without changing storage outcomes.
    pub fn record(&self, diagnostic: Diagnostic) {
        let Ok(mut records) = self.0.records.try_lock() else {
            increment(&self.0.dropped, diagnostic.occurrences);
            return;
        };

        if self.0.closed.load(Ordering::Acquire) {
            increment(&self.0.dropped, diagnostic.occurrences);
            return;
        }

        if let Some(existing) = records
            .iter_mut()
            .find(|record| record.same_kind(&diagnostic))
        {
            existing.occurrences = existing.occurrences.saturating_add(diagnostic.occurrences);

            // Samples are gauges: retain the newest one, never sum overlapping
            // sampled bytes or represent a coalesced sample as a total backlog.
            if diagnostic.last_seen >= existing.last_seen {
                existing.cleanup = diagnostic.cleanup;
            }

            existing.first_seen = existing.first_seen.min(diagnostic.first_seen);
            existing.last_seen = existing.last_seen.max(diagnostic.last_seen);
            increment(&self.0.coalesced, diagnostic.occurrences);
            return;
        }

        if records.len() == CAPACITY {
            increment(&self.0.dropped, diagnostic.occurrences);
            return;
        }

        records.push_back(diagnostic);
        self.0.queued.store(records.len(), Ordering::Relaxed);
    }

    /// Detach one owned record; no lock or engine handle travels to the sink.
    /// `None` means unavailable now, including empty, contended or closed queues.
    #[must_use]
    pub fn take(&self) -> Option<Diagnostic> {
        let Ok(mut records) = self.0.records.try_lock() else {
            return None;
        };

        if self.0.closed.load(Ordering::Acquire) {
            return None;
        }

        let record = records.pop_front();
        self.0.queued.store(records.len(), Ordering::Relaxed);
        record
    }

    /// Revoke queued delivery without waiting for an observer that already
    /// received an owned record. Such a callback has no storage authority.
    pub fn close(&self) {
        self.0.closed.store(true, Ordering::Release);

        // Wait only for the bounded internal queue critical section. No sink
        // can hold this mutex. Terminal cleanup may drain poisoned state but
        // never clears poison or resumes delivery after an invariant failure.
        let mut records = self
            .0
            .records
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);

        for record in records.drain(..) {
            increment(&self.0.dropped, record.occurrences);
        }

        self.0.queued.store(0, Ordering::Relaxed);
    }

    /// Account for a recoverable observer failure, without recursively logging
    /// its potentially sensitive exception/panic or changing operation results.
    pub fn observer_failed(&self) {
        increment(&self.0.observer_failures, 1);
    }

    /// Approximate cumulative counters, read independently without waiting.
    /// Saturation caps each u64; these are delivery counts, not a storage backlog.
    #[must_use]
    pub fn stats(&self) -> Stats {
        Stats {
            queued: self.0.queued.load(Ordering::Relaxed),
            dropped: self.0.dropped.load(Ordering::Relaxed),
            coalesced: self.0.coalesced.load(Ordering::Relaxed),
            observer_failures: self.0.observer_failures.load(Ordering::Relaxed),
        }
    }
}

/// Internal host observation capability, with no connection or shutdown rights.
#[derive(Clone)]
pub struct Receiver(Weak<State>);

impl Receiver {
    /// Deduplicate registration of independently opened connections.
    #[must_use]
    pub fn same_queue(&self, other: &Self) -> bool {
        Weak::ptr_eq(&self.0, &other.0)
    }

    /// A closed or destroyed context cannot deliver another queued record.
    #[must_use]
    pub fn is_closed(&self) -> bool {
        self.0
            .upgrade()
            .is_none_or(|state| state.closed.load(Ordering::Acquire))
    }

    /// Detach one bounded observation and its approximate delivery counters.
    #[must_use]
    pub fn take(&self) -> Option<(Diagnostic, Stats)> {
        let queue = Diagnostics(self.0.upgrade()?);
        queue.take().map(|record| (record, queue.stats()))
    }

    /// Count a recoverable host failure without retaining its details.
    pub fn observer_failed(&self) {
        if let Some(state) = self.0.upgrade() {
            Diagnostics(state).observer_failed();
        }
    }
}

/// Detached counters; no independent loads imply an atomic queue snapshot.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Stats {
    /// Records pending delivery, at most the internal fixed capacity.
    pub queued: usize,
    /// Observations discarded by contention, capacity or close.
    pub dropped: u64,
    /// Additional repeated observations combined into existing records.
    pub coalesced: u64,
    /// Observer failures reported by the host adapter.
    pub observer_failures: u64,
}

/// The host dispatcher captured at registration, never a library global default.
/// Delivery must run on a detached host delivery worker, not an engine worker.
#[derive(Clone)]
pub struct TracingSink(tracing::Dispatch);

impl TracingSink {
    /// Capture the caller's scoped/global dispatcher without configuring it.
    #[must_use]
    pub fn capture() -> Self {
        Self(tracing::dispatcher::get_default(Clone::clone))
    }

    /// Invoke the captured host subscriber with allowlisted detached fields.
    ///
    /// # Errors
    /// Contains an unwinding subscriber panic. The host's panic hook remains
    /// its own; this cannot suppress that hook, host abort or blocked host code.
    pub fn emit(&self, diagnostic: &Diagnostic, stats: Stats) -> Result<(), ObserverPanicked> {
        std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            tracing::dispatcher::with_default(&self.0, || {
                if let Some(observation) = diagnostic.cleanup {
                    tracing::info!(
                        target: "mikrio",
                        store_id = %diagnostic.store_id,
                        work = diagnostic.work.as_str(),
                        sampled_blobs = observation.sampled_blobs,
                        observed_file_bytes = observation.observed_file_bytes,
                        oldest_sample_blob_age_ms = observation.oldest_sample_blob_age_ms,
                        unobserved_blobs = observation.unobserved_blobs,
                        more_pending = observation.more_pending,
                        blocked_shard_attempts = observation.blocked_shard_attempts,
                        age_ms = diagnostic.age().as_millis(),
                        dropped_observations = stats.dropped,
                        observer_failures = stats.observer_failures,
                        "bounded cleanup sample"
                    );

                    return;
                }

                tracing::warn!(
                    target: "mikrio",
                    store_id = %diagnostic.store_id,
                    work = diagnostic.work.as_str(),
                    failure = diagnostic.failure(),
                    occurrences = diagnostic.occurrences,
                    age_ms = diagnostic.age().as_millis(),
                    observation_span_ms = diagnostic.observation_span().as_millis(),
                    dropped_observations = stats.dropped,
                    observer_failures = stats.observer_failures,
                    "storage work needs attention"
                );
            });
        }))
        .map_err(|_| ObserverPanicked)
    }
}

/// A contained observer failure; never retains its arbitrary panic payload.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ObserverPanicked;

impl fmt::Display for ObserverPanicked {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("diagnostic observer panicked")
    }
}

impl std::error::Error for ObserverPanicked {}

fn increment(counter: &AtomicU64, count: u64) {
    let _previous = counter.try_update(Ordering::Relaxed, Ordering::Relaxed, |value| {
        Some(value.saturating_add(count))
    });
}

#[cfg(test)]
mod tests;
