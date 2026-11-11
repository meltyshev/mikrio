//! Explicit private diagnostic observation addon, never packed for users.

use std::io;

use mikrio_core::StoreId;
use mikrio_core::diagnostics::{Diagnostic, Diagnostics, Work};
use napi::bindgen_prelude::BigInt;
use napi::{Error, Result, Status};
use napi_derive::napi;

#[napi]
pub struct DiagnosticProbe(Diagnostics);

impl Drop for DiagnosticProbe {
    fn drop(&mut self) {
        self.0.close();
    }
}

#[napi]
impl DiagnosticProbe {
    #[napi(constructor, catch_unwind)]
    #[must_use]
    pub fn new() -> Self {
        Self(Diagnostics::default())
    }

    /// Observe a controlled error with sensitive detail; retain classification only.
    #[napi(catch_unwind)]
    pub fn emit(&self, detail: String) {
        let error = io::Error::new(io::ErrorKind::PermissionDenied, detail);

        self.0.record(Diagnostic::io(
            StoreId::from_bytes([7; 16]),
            Work::Cleanup,
            &error,
        ));
    }

    /// Remove a detached event before any host code runs.
    #[napi(catch_unwind)]
    #[must_use]
    pub fn take(&self) -> Option<Event> {
        self.0.take().map(|record| Event {
            store_id: record.store_id().to_string(),
            work: record.work().as_str().into(),
            failure: record.failure().map(str::to_owned),
            occurrences: record.occurrences().into(),
            age_ms: u64::try_from(record.age().as_millis())
                .unwrap_or(u64::MAX)
                .into(),
        })
    }

    #[napi(catch_unwind)]
    pub fn observer_failed(&self) {
        self.0.observer_failed();
    }

    #[napi(catch_unwind)]
    pub fn close(&self) {
        self.0.close();
    }

    /// Inspect the bounded queue without invoking an observer.
    ///
    /// # Errors
    /// Rejects a queue count inconsistent with the fixed internal capacity.
    #[napi(getter, catch_unwind)]
    pub fn stats(&self) -> Result<Stats> {
        let stats = self.0.stats();

        Ok(Stats {
            queued: u32::try_from(stats.queued)
                .map_err(|_| Error::new(Status::GenericFailure, "invalid diagnostic count"))?,
            dropped: stats.dropped.into(),
            coalesced: stats.coalesced.into(),
            observer_failures: stats.observer_failures.into(),
        })
    }
}

impl Default for DiagnosticProbe {
    fn default() -> Self {
        Self::new()
    }
}

#[napi(object, object_from_js = false)]
pub struct Event {
    pub store_id: String,
    pub work: String,
    pub failure: Option<String>,
    pub occurrences: BigInt,
    pub age_ms: BigInt,
}

#[napi(object, object_from_js = false)]
pub struct Stats {
    pub queued: u32,
    pub dropped: BigInt,
    pub coalesced: BigInt,
    pub observer_failures: BigInt,
}
