//! Fixed counters at actual payload writes and guard acquisition; never shipped.

use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{Duration, Instant};

static PAYLOAD_BYTES: AtomicU64 = AtomicU64::new(0);
static PAYLOAD_CALLS: AtomicU64 = AtomicU64::new(0);
static GUARD_ACQUISITIONS: AtomicU64 = AtomicU64::new(0);
static GUARD_SUCCEEDED: AtomicU64 = AtomicU64::new(0);
static GUARD_CONTENDED: AtomicU64 = AtomicU64::new(0);
static GUARD_REFUSALS: AtomicU64 = AtomicU64::new(0);
static GUARD_ELAPSED_NS: AtomicU64 = AtomicU64::new(0);
static GUARD_MAX_ELAPSED_NS: AtomicU64 = AtomicU64::new(0);
static GUARD_BACKOFF_NS: AtomicU64 = AtomicU64::new(0);

pub(crate) fn payload_written(bytes: usize) {
    PAYLOAD_BYTES.fetch_add(
        u64::try_from(bytes).expect("supported usize fits u64"),
        Ordering::Relaxed,
    );

    PAYLOAD_CALLS.fetch_add(1, Ordering::Relaxed);
}

pub(crate) fn payload() -> (u64, u64) {
    (
        PAYLOAD_BYTES.load(Ordering::Relaxed),
        PAYLOAD_CALLS.load(Ordering::Relaxed),
    )
}

pub(crate) struct GuardWait {
    started: Instant,
    contended: bool,
    acquired: bool,
}

impl GuardWait {
    pub(crate) fn start() -> Self {
        Self {
            started: Instant::now(),
            contended: false,
            acquired: false,
        }
    }

    pub(crate) fn refused(&mut self) {
        if !self.contended {
            self.contended = true;
            GUARD_CONTENDED.fetch_add(1, Ordering::Relaxed);
        }

        GUARD_REFUSALS.fetch_add(1, Ordering::Relaxed);
    }

    pub(crate) fn acquired(&mut self) {
        self.acquired = true;
    }

    pub(crate) fn backoff(elapsed: Duration) {
        GUARD_BACKOFF_NS.fetch_add(nanos(elapsed), Ordering::Relaxed);
    }
}

impl Drop for GuardWait {
    fn drop(&mut self) {
        let elapsed = nanos(self.started.elapsed());
        GUARD_ACQUISITIONS.fetch_add(1, Ordering::Relaxed);
        GUARD_SUCCEEDED.fetch_add(u64::from(self.acquired), Ordering::Relaxed);
        GUARD_ELAPSED_NS.fetch_add(elapsed, Ordering::Relaxed);
        GUARD_MAX_ELAPSED_NS.fetch_max(elapsed, Ordering::Relaxed);
    }
}

fn nanos(elapsed: Duration) -> u64 {
    u64::try_from(elapsed.as_nanos()).expect("bounded measurement duration fits u64")
}

#[derive(serde::Serialize)]
pub(crate) struct GuardSnapshot {
    pub(crate) acquisitions: u64,
    pub(crate) succeeded: u64,
    pub(crate) contended: u64,
    pub(crate) refusals: u64,
    pub(crate) total_acquisition_ns: u64,
    pub(crate) max_acquisition_ns: u64,
    pub(crate) retry_backoff_ns: u64,
}

pub(crate) fn guards() -> GuardSnapshot {
    GuardSnapshot {
        acquisitions: GUARD_ACQUISITIONS.load(Ordering::Relaxed),
        succeeded: GUARD_SUCCEEDED.load(Ordering::Relaxed),
        contended: GUARD_CONTENDED.load(Ordering::Relaxed),
        refusals: GUARD_REFUSALS.load(Ordering::Relaxed),
        total_acquisition_ns: GUARD_ELAPSED_NS.load(Ordering::Relaxed),
        max_acquisition_ns: GUARD_MAX_ELAPSED_NS.load(Ordering::Relaxed),
        retry_backoff_ns: GUARD_BACKOFF_NS.load(Ordering::Relaxed),
    }
}
