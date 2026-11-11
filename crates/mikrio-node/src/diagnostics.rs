//! Queue translation only; JavaScript owns delivery on its own event loop.

use napi_derive::napi;

#[napi]
pub struct NativeDiagnostics(pub(super) mikrio::embedding::Diagnostics);

#[napi]
impl NativeDiagnostics {
    /// Deduplicate independently opened handles sharing native maintenance.
    #[napi(catch_unwind)]
    #[must_use]
    pub fn same_queue(&self, other: &NativeDiagnostics) -> bool {
        self.0.same_queue(&other.0)
    }

    /// Delivery ends when the native context closes or disappears.
    #[napi(catch_unwind)]
    #[must_use]
    pub fn is_closed(&self) -> bool {
        self.0.is_closed()
    }

    /// One detached redacted message; no host callback or native I/O.
    #[napi(catch_unwind)]
    #[must_use]
    pub fn take(&self) -> Option<String> {
        self.0.take()
    }

    /// Count a recoverable host failure without retaining its details.
    #[napi(catch_unwind)]
    pub fn observer_failed(&self) {
        self.0.observer_failed();
    }
}
