#![allow(
    clippy::unwrap_used,
    clippy::panic,
    reason = "controlled observer failure and test assertions"
)]

use std::sync::mpsc;
use std::thread;

use tracing::field::{Field, Visit};
use tracing::span::{Attributes, Id, Record};
use tracing::{Dispatch, Event, Metadata, Subscriber};

use super::*;

const SECRET: &str = "secret-key signed-url private-path body-canary";

fn diagnostic(identity: u8) -> Diagnostic {
    Diagnostic::io(
        StoreId::from_bytes([identity; 16]),
        Work::Cleanup,
        &io::Error::new(io::ErrorKind::PermissionDenied, SECRET),
    )
}

#[test]
fn native_operation_wrappers_preserve_coarse_classes_without_cause_text() {
    let store_id = StoreId::from_bytes([1; 16]);

    let error = crate::OperationError::from(crate::MetadataError::Io(io::Error::new(
        io::ErrorKind::PermissionDenied,
        SECRET,
    )));

    let record = Diagnostic::operation(store_id, Work::Cleanup, &error);
    assert_eq!(record.failure(), Some("permission"));
    assert!(!format!("{record:?}").contains(SECRET));
    let error = crate::OperationError::from(crate::MetadataError::Corrupt(SECRET));
    let record = Diagnostic::operation(store_id, Work::Cleanup, &error);
    assert_eq!(record.failure(), Some("corruption"));
    assert!(!format!("{record:?}").contains(SECRET));
}

#[test]
fn repeated_failure_coalesces_and_full_queue_drops_without_growing() {
    let queue = Diagnostics::default();

    for _ in 0..1000 {
        queue.record(diagnostic(0));
    }

    for identity in 1..=20 {
        queue.record(diagnostic(identity));
    }

    assert_eq!(queue.stats().queued, 16);
    assert_eq!(queue.stats().coalesced, 999);
    assert_eq!(queue.stats().dropped, 5);
    assert_eq!(queue.0.records.lock().unwrap().capacity(), 16);
    let first = queue.take().unwrap();
    assert_eq!(first.occurrences(), 1000);
    assert_eq!(first.failure(), Some("permission"));
    assert!(!format!("{first:?}").contains(SECRET));
    assert!(first.observation_span() <= first.first_seen.elapsed());
    queue.close();
    assert_eq!(queue.stats().queued, 0);
    assert_eq!(queue.stats().dropped, 20);
    queue.record(diagnostic(0));
    assert!(queue.take().is_none());
    assert_eq!(queue.stats().dropped, 21);
}

#[test]
fn cleanup_samples_coalesce_as_latest_gauges_without_erasing_failures() {
    let queue = Diagnostics::default();
    let store_id = StoreId::from_bytes([0; 16]);

    let first = CleanupObservation {
        sampled_blobs: 16,
        observed_file_bytes: 8192,
        oldest_sample_blob_age_ms: Some(60_000),
        unobserved_blobs: 2,
        more_pending: true,
        blocked_shard_attempts: 7,
    };

    let latest = CleanupObservation {
        sampled_blobs: 1,
        observed_file_bytes: 1024,
        ..CleanupObservation::default()
    };

    queue.record(Diagnostic::cleanup_observation(store_id, first));
    queue.record(diagnostic(0));
    queue.record(Diagnostic::cleanup_observation(store_id, latest));
    assert_eq!(queue.stats().queued, 2);
    let sample = queue.take().unwrap();
    assert_eq!(sample.cleanup(), Some(latest));
    assert_eq!(sample.failure(), None);
    assert_eq!(sample.occurrences(), 2);
    assert_eq!(queue.take().unwrap().failure(), Some("permission"));
}

#[test]
fn concurrent_producers_and_close_account_for_every_observation() {
    let queue = Diagnostics::default();
    let start = Arc::new(std::sync::Barrier::new(9));

    thread::scope(|scope| {
        for identity in 0..8 {
            let queue = queue.clone();
            let start = Arc::clone(&start);

            scope.spawn(move || {
                start.wait();

                for _ in 0..1000 {
                    queue.record(diagnostic(identity));
                }
            });
        }

        start.wait();
        queue.close();
    });

    assert_eq!(queue.stats().dropped, 8000);
    assert_eq!(queue.stats().queued, 0);
    assert!(queue.take().is_none());
}

#[test]
fn host_receiver_deduplicates_contexts_and_cannot_retain_or_reopen_delivery() {
    let queue = Diagnostics::default();
    let first = queue.receiver();
    let second = queue.receiver();
    assert!(queue.has_host_delivery());
    assert!(first.same_queue(&second));
    assert!(!first.same_queue(&Diagnostics::default().receiver()));
    assert!(!first.is_closed());
    queue.record(diagnostic(1));
    assert_eq!(first.take().unwrap().0.occurrences(), 1);
    second.observer_failed();
    assert_eq!(queue.stats().observer_failures, 1);
    queue.record(diagnostic(1));
    queue.close();
    assert!(first.is_closed());
    assert!(second.take().is_none());
    drop(queue);
    assert!(first.is_closed());
    first.observer_failed();
    assert!(first.take().is_none());

    let abandoned = {
        let queue = Diagnostics::default();
        queue.record(diagnostic(2));
        queue.receiver()
    };

    assert!(abandoned.is_closed());
    assert!(abandoned.take().is_none());
}

struct Observer {
    received: mpsc::Sender<String>,
    reenter: Diagnostics,
    release: Mutex<Option<mpsc::Receiver<()>>>,
    panics: AtomicBool,
}

impl Subscriber for Observer {
    fn enabled(&self, _: &Metadata<'_>) -> bool {
        true
    }

    fn new_span(&self, _: &Attributes<'_>) -> Id {
        Id::from_u64(1)
    }

    fn record(&self, _: &Id, _: &Record<'_>) {}
    fn record_follows_from(&self, _: &Id, _: &Id) {}
    fn enter(&self, _: &Id) {}
    fn exit(&self, _: &Id) {}

    fn event(&self, event: &Event<'_>) {
        assert!(
            !self.panics.swap(false, Ordering::AcqRel),
            "controlled observer panic"
        );

        // Reenter from host code. A dispatch-under-lock bug would discard this
        // observation, which the queued-record assertion below detects.
        self.reenter.record(diagnostic(2));
        let mut fields = Fields(String::new());
        event.record(&mut fields);
        assert_eq!(event.metadata().target(), "mikrio");
        self.received.send(fields.0).unwrap();
        let release = self.release.lock().unwrap().take();

        if let Some(release) = release {
            release.recv_timeout(Duration::from_secs(20)).unwrap();
        }
    }
}

struct Fields(String);

impl Visit for Fields {
    fn record_debug(&mut self, field: &Field, value: &dyn fmt::Debug) {
        use std::fmt::Write;
        write!(self.0, "{}={value:?};", field.name()).unwrap();
    }
}

#[test]
fn captured_host_dispatcher_observes_idle_failure_without_blocking_close() {
    let directory = crate::test_directory::TestDirectory::new().unwrap();

    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_time()
        .build()
        .unwrap();

    let connection = runtime
        .block_on(crate::Connection::open(
            directory.path().join("store"),
            None,
        ))
        .unwrap();

    let queue = Diagnostics::default();
    let (received, events) = mpsc::channel();
    let (release, blocked) = mpsc::channel();

    let dispatch = Dispatch::new(Observer {
        received,
        reenter: queue.clone(),
        release: Mutex::new(Some(blocked)),
        panics: AtomicBool::new(false),
    });

    let sink = tracing::dispatcher::with_default(&dispatch, TracingSink::capture);
    let (trigger, idle) = mpsc::channel();
    let consumer = queue.clone();

    let reporter = thread::spawn(move || {
        // The scoped caller has returned. Only this detached worker invokes its
        // captured subscriber; there is no global subscriber installation.
        idle.recv_timeout(Duration::from_secs(5)).unwrap();
        let record = consumer.take().unwrap();
        sink.emit(&record, consumer.stats()).unwrap();
    });

    queue.record(diagnostic(1));
    trigger.send(()).unwrap();
    let event = events.recv_timeout(Duration::from_secs(5)).unwrap();
    assert!(event.contains("work=\"cleanup\""));
    assert!(event.contains("failure=\"permission\""));
    assert!(!event.contains(SECRET));
    assert!(!event.contains("signed-url"));
    assert_eq!(queue.stats().queued, 1); // The observer reentered successfully.
    queue.close(); // Observer is still waiting for `release` on its own thread.
    assert_eq!(queue.stats().queued, 0);

    runtime.block_on(async {
        tokio::time::timeout(Duration::from_secs(2), connection.close())
            .await
            .unwrap()
            .unwrap();
    });

    // The real connection's native close fence settles while host code remains
    // blocked. The fixture's detached event owns neither it nor its guards.
    release.send(()).unwrap();
    reporter.join().unwrap();
    drop(connection);
    drop(runtime);
    directory.finish().unwrap();
}

#[test]
fn real_idle_cleanup_retries_while_captured_tracing_observer_panics_and_blocks() {
    use crate::integrity::{PutIntegrity, PutIntegrityInput};
    use crate::object::{
        BucketName, ObjectKey, PutRequest, ReadConditions, ReadConditionsInput, ReadRequest,
        Representation, RepresentationInput, UserMetadata, WriteConditions,
    };

    let directory = crate::test_directory::TestDirectory::new().unwrap();
    let path = directory.path().join("secret-path-canary");

    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_time()
        .max_blocking_threads(1)
        .build()
        .unwrap();

    let (received, events) = mpsc::channel();
    let (release, blocked) = mpsc::channel();

    let dispatch = Dispatch::new(Observer {
        received,
        reenter: Diagnostics::default(),
        release: Mutex::new(Some(blocked)),
        panics: AtomicBool::new(true),
    });

    let connection = tracing::dispatcher::with_default(&dispatch, || {
        runtime
            .block_on(crate::Connection::open(path.clone(), None))
            .unwrap()
    });

    assert!(!tracing::dispatcher::get_default(Dispatch::is::<Observer>));

    let put = |key: &str| PutRequest {
        bucket: BucketName::parse("attachments").unwrap(),
        key: ObjectKey::parse(key).unwrap(),
        expected_owner: None,
        content_length: None,
        representation: Representation::new(RepresentationInput::default()).unwrap(),
        metadata: UserMetadata::default(),
        tags: None,
        integrity: PutIntegrity::new(PutIntegrityInput::default()).unwrap(),
        conditions: WriteConditions::new(None, None).unwrap(),
    };

    runtime.block_on(async {
        connection
            .create_bucket(BucketName::parse("attachments").unwrap())
            .await
            .unwrap();

        connection
            .put_object(put("secret-key-canary"), SECRET.as_bytes())
            .await
            .unwrap();

        let request = ReadRequest {
            bucket: BucketName::parse("attachments").unwrap(),
            key: ObjectKey::parse("secret-key-canary").unwrap(),
            expected_owner: None,
            conditions: ReadConditions::new(ReadConditionsInput::default()).unwrap(),
            range: None,
            part_number: None,
            response: crate::object::Representation::default(),
        };

        let (selected, mut body) = connection.get_object(request.clone()).await.unwrap();

        let payload_path = path
            .join("objects")
            .join(format!("{:02x}", selected.blob_id.shard()))
            .join(selected.blob_id.to_string());

        let retained = payload_path.with_file_name("unknown-secret-canary");
        connection.delete_object(request, None).await.unwrap();
        std::fs::rename(&payload_path, &retained).unwrap();
        std::fs::create_dir(&payload_path).unwrap();
        body.close().await.unwrap();

        // The foreground is idle. Production maintenance discovers the real
        // unexpected path and the captured host dispatch survives caller scope.
        let event = tokio::time::timeout(Duration::from_secs(10), async {
            loop {
                match events.try_recv() {
                    Ok(event) => break event,
                    Err(mpsc::TryRecvError::Empty) => {
                        tokio::time::sleep(Duration::from_millis(10)).await;
                    }
                    Err(error) => panic!("observer disconnected: {error}"),
                }
            }
        })
        .await
        .unwrap();

        assert!(event.contains("work=\"cleanup\""));
        assert!(event.contains("observer_failures=1"));
        assert!(!event.contains("canary"));
        assert!(!event.contains(SECRET));
        assert_eq!(std::fs::read(&retained).unwrap(), SECRET.as_bytes());
        std::fs::remove_dir(&payload_path).unwrap();
        std::fs::rename(&retained, &payload_path).unwrap();

        // Two observed maintenance failures have already scheduled retry
        // backoff. Include that delay before testing progress and shutdown.
        tokio::time::timeout(Duration::from_secs(10), async {
            while payload_path.exists() {
                tokio::time::sleep(Duration::from_millis(10)).await;
            }

            connection
                .put_object(put("unrelated"), b"progress".as_slice())
                .await
                .unwrap();

            connection.close().await.unwrap();
        })
        .await
        .unwrap();

        // The subscriber is still blocked on its detached delivery thread.
        release.send(()).unwrap();
    });

    drop(connection);
    drop(runtime);
    directory.finish().unwrap();
}

#[test]
fn observer_panic_is_contained_and_scoped_dispatch_is_restored() {
    let queue = Diagnostics::default();
    let (received, events) = mpsc::channel();

    let dispatch = Dispatch::new(Observer {
        received,
        reenter: queue.clone(),
        release: Mutex::new(None),
        panics: AtomicBool::new(true),
    });

    let sink = tracing::dispatcher::with_default(&dispatch, TracingSink::capture);
    assert!(!tracing::dispatcher::get_default(Dispatch::is::<Observer>));
    let record = diagnostic(1);
    assert_eq!(sink.emit(&record, queue.stats()), Err(ObserverPanicked));
    queue.observer_failed();
    assert!(!tracing::dispatcher::get_default(Dispatch::is::<Observer>));
    assert_eq!(queue.stats().observer_failures, 1);
    sink.emit(&record, queue.stats()).unwrap();
    let event = events.recv_timeout(Duration::from_secs(5)).unwrap();
    assert!(event.contains("observer_failures=1"));
    assert!(!event.contains(SECRET));
}
