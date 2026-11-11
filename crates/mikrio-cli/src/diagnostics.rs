//! A bounded host subscriber. Native delivery never waits for terminal output.

use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};

use tokio::sync::mpsc;
use tracing::field::{Field, Visit};
use tracing::span::{Attributes, Id, Record};
use tracing::{Dispatch, Event, Level, Metadata, Subscriber};

const CAPACITY: usize = 16;

pub(crate) struct Diagnostics {
    receiver: mpsc::Receiver<Diagnostic>,
    dropped: Arc<AtomicU64>,
}

#[derive(Default)]
pub(crate) struct Diagnostic {
    level: &'static str,
    work: Option<&'static str>,
    failure: Option<&'static str>,
    occurrences: u64,
    dropped: u64,
}

struct Observer {
    sender: mpsc::Sender<Diagnostic>,
    dropped: Arc<AtomicU64>,
}

impl Diagnostics {
    pub(crate) fn new() -> (Self, Dispatch) {
        let (sender, receiver) = mpsc::channel(CAPACITY);
        let dropped = Arc::new(AtomicU64::new(0));

        let dispatch = Dispatch::new(Observer {
            sender,
            dropped: Arc::clone(&dropped),
        });

        (Self { receiver, dropped }, dispatch)
    }

    pub(crate) async fn receive(&mut self) -> Option<Diagnostic> {
        let record = self.receiver.recv().await?;
        Some(self.with_dropped(record))
    }

    pub(crate) fn pending(&mut self) -> Option<Diagnostic> {
        let record = self.receiver.try_recv().ok()?;
        Some(self.with_dropped(record))
    }

    pub(crate) fn close(&mut self) {
        self.receiver.close();
    }

    fn with_dropped(&self, mut record: Diagnostic) -> Diagnostic {
        record.dropped = self.dropped.swap(0, Ordering::Relaxed);
        record
    }
}

impl Diagnostic {
    pub(crate) const fn level(&self) -> &'static str {
        self.level
    }

    pub(crate) fn message(&self) -> String {
        format!(
            "storage work needs attention: work={} failure={} occurrences={} dropped={}",
            self.work.unwrap_or("unknown"),
            self.failure.unwrap_or("unknown"),
            self.occurrences,
            self.dropped,
        )
    }
}

impl Visit for Diagnostic {
    fn record_str(&mut self, field: &Field, value: &str) {
        match (field.name(), value) {
            ("work", "metadata") => self.work = Some("metadata"),
            ("work", "cleanup") => self.work = Some("cleanup"),
            ("failure", "corruption") => self.failure = Some("corruption"),
            ("failure", "permission") => self.failure = Some("permission"),
            ("failure", "capacity") => self.failure = Some("capacity"),
            ("failure", "io") => self.failure = Some("io"),
            _ => {}
        }
    }

    fn record_u64(&mut self, field: &Field, value: u64) {
        if field.name() == "occurrences" {
            self.occurrences = value;
        }
    }

    fn record_debug(&mut self, _: &Field, _: &dyn std::fmt::Debug) {
        // Never format arbitrary fields or event messages: they may be private.
    }
}

impl Subscriber for Observer {
    fn enabled(&self, metadata: &Metadata<'_>) -> bool {
        metadata.is_event() && metadata.target() == "mikrio" && *metadata.level() <= Level::WARN
    }

    fn event(&self, event: &Event<'_>) {
        if !self.enabled(event.metadata()) {
            return;
        }

        let mut record = Diagnostic {
            level: if *event.metadata().level() == Level::ERROR {
                "error"
            } else {
                "warn"
            },
            ..Diagnostic::default()
        };

        event.record(&mut record);

        if let Err(mpsc::error::TrySendError::Full(_)) = self.sender.try_send(record) {
            let _previous =
                self.dropped
                    .try_update(Ordering::Relaxed, Ordering::Relaxed, |value| {
                        Some(value.saturating_add(1))
                    });
        }
    }

    fn new_span(&self, _: &Attributes<'_>) -> Id {
        Id::from_u64(1)
    }

    fn record(&self, _: &Id, _: &Record<'_>) {}
    fn record_follows_from(&self, _: &Id, _: &Id) {}
    fn enter(&self, _: &Id) {}
    fn exit(&self, _: &Id) {}
}

#[cfg(test)]
mod tests {
    use super::*;

    struct Private;

    impl std::fmt::Debug for Private {
        #[expect(
            clippy::panic,
            reason = "a private formatter is a negative test tripwire"
        )]
        fn fmt(&self, _: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
            panic!("private debug value must never be formatted")
        }
    }

    #[test]
    fn host_diagnostics_are_bounded_redacted_and_detach_on_close() {
        let (mut diagnostics, dispatch) = Diagnostics::new();

        tracing::dispatcher::with_default(&dispatch, || {
            tracing::warn!(target: "upstream", "private canary");
            tracing::info!(target: "mikrio", "cleanup sample");

            for _ in 0..CAPACITY + 3 {
                tracing::warn!(target: "mikrio", work = "cleanup", failure = Some("permission"), occurrences = 4_u64, private = ?Private, "private canary");
            }
        });

        let first = diagnostics.pending().expect("first warning");
        assert_eq!(first.level(), "warn");

        assert_eq!(
            first.message(),
            "storage work needs attention: work=cleanup failure=permission occurrences=4 dropped=3"
        );

        for _ in 1..CAPACITY {
            assert_eq!(diagnostics.pending().expect("bounded warning").dropped, 0);
        }

        assert!(diagnostics.pending().is_none());

        tracing::dispatcher::with_default(&dispatch, || {
            tracing::error!(target: "mikrio", work = "private canary", failure = "private canary", secret = ?Private, "private canary");
        });

        let unknown = diagnostics.pending().expect("unknown warning");
        assert_eq!(unknown.level(), "error");
        assert!(!unknown.message().contains("canary"));
        diagnostics.close();

        tracing::dispatcher::with_default(&dispatch, || {
            tracing::warn!(target: "mikrio", "closed observer");
        });

        assert!(diagnostics.pending().is_none());
    }
}
