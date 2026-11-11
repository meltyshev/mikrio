//! Observations of completed worker steps, independent of transport keepalives.

use tokio::sync::watch;
use tokio::time::Instant;

/// Private adapter observation channel. It grants no admission or cancellation authority.
#[derive(Clone, Debug)]
pub struct WorkProgress(watch::Sender<Instant>);

impl Default for WorkProgress {
    fn default() -> Self {
        Self(watch::channel(Instant::now()).0)
    }
}

impl WorkProgress {
    /// Observe the most recent completed worker step using monotonic time.
    #[must_use]
    pub fn subscribe(&self) -> watch::Receiver<Instant> {
        self.0.subscribe()
    }

    pub(super) fn advance(&self) {
        self.0.send_replace(Instant::now());
    }
}
