use super::*;

mod admin;
mod authorization_publication;
mod batch_deletion;
mod bucket_configuration;
mod buckets;
mod copy;
mod corrupt_recovery;
mod descriptors;
mod expiration;
mod faults;
mod gc_faults;
mod gc_progress;
mod guard_admission;
mod guard_errors;
mod histories;
mod listener;
mod listener_publication;
mod maintenance;
mod measurements;
mod multipart;
mod objects;
mod part_reads;
mod payload_interruption;
mod processes;
mod publication_orders;
mod put_collision;
mod read_authorization;
mod read_selection;
mod reservation_interruption;
mod restore;
mod storage_replay;
mod tagging;
mod worker_unwind;
use crate::test_directory::TestDirectory;
use crate::{GuardMode, GuardTarget};
use std::future::Future;
use std::io::Write;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::task::Poll;

type TestResult = Result<(), Box<dyn std::error::Error>>;

fn runtime() -> io::Result<tokio::runtime::Runtime> {
    tokio::runtime::Builder::new_current_thread()
        .enable_time()
        .max_blocking_threads(1)
        .build()
}

fn ensure_open(state: &State) -> Result<(), ConnectionError> {
    match state
        .activity
        .lock()
        .map_err(|_| shutdown("poisoned test connection"))?
        .lifecycle
    {
        Lifecycle::Open => Ok(()),
        Lifecycle::Closing(_) => Err(ConnectionError::StoreClosing),
        Lifecycle::Closed => Err(ConnectionError::StoreClosed),
    }
}

impl Connection {
    // Queue-accounting tests isolate their exact foreground commands. Object
    // lifetime tests exercise the ordinary scheduler without this test hook.
    pub(crate) fn pause_maintenance(&self) {
        self.owner
            .state
            .context
            .test_hooks
            .pause_maintenance
            .store(true, Ordering::SeqCst);
    }

    async fn probe(
        &self,
        work: impl FnOnce(&OpenedStore) -> io::Result<()> + Send + 'static,
    ) -> Result<(), ConnectionError> {
        ensure_open(&self.owner.state)?;

        let permit = self
            .owner
            .state
            .context
            .sender
            .reserve()
            .await
            .map_err(|_| shutdown("probe worker unavailable"))?;

        let (reply, response) = oneshot::channel();

        {
            let lifecycle = self
                .owner
                .state
                .activity
                .lock()
                .map_err(|_| shutdown("poisoned test admission"))?;

            match lifecycle.lifecycle {
                Lifecycle::Closing(_) => return Err(ConnectionError::StoreClosing),
                Lifecycle::Closed => return Err(ConnectionError::StoreClosed),
                Lifecycle::Open => {}
            }

            permit.send(Command::Probe {
                _state: Arc::clone(&self.owner.state),
                work: Box::new(move |store| work(store)),
                reply,
            });
        }

        response
            .await
            .map_err(|_| shutdown("probe result unavailable"))?
            .map_err(|error| ConnectionError::from(OpenError::from(error)))
    }
}

#[test]
fn cloned_connection_closes_together_while_independent_open_remains_usable() -> TestResult {
    let directory = TestDirectory::new()?;
    let runtime = runtime()?;

    runtime.block_on(async {
        let first =
            Connection::open(directory.path().join("store"), Some("eu-west-1".into())).await?;

        let alias = first.clone();
        let second = Connection::open(directory.path().join("store"), None).await?;

        assert!(Arc::ptr_eq(
            &first.owner.state.context,
            &second.owner.state.context
        ));

        assert!(!Arc::ptr_eq(&first.owner.state, &second.owner.state));
        assert_eq!(first.info(), second.info());
        let snapshot = first.info().clone();
        alias.close().await?;

        assert!(matches!(
            first.probe(|_| Ok(())).await,
            Err(ConnectionError::StoreClosed)
        ));

        second
            .probe(|store| {
                assert_eq!(store.record().region, "eu-west-1");
                Ok(())
            })
            .await?;

        first.close().await?;
        second.close().await?;
        second.close().await?;
        assert_eq!(first.info(), &snapshot);
        assert_eq!(tokio::spawn(async { 42 }).await?, 42);
        Ok::<_, Box<dyn std::error::Error>>(())
    })?;

    directory.finish()?;
    Ok(())
}

#[test]
fn independently_owned_runtimes_do_not_depend_on_the_first_opener() -> TestResult {
    let directory = TestDirectory::new()?;
    let first_runtime = runtime()?;
    let second_runtime = runtime()?;
    let first = first_runtime.block_on(Connection::open(directory.path().join("store"), None))?;
    let second = second_runtime.block_on(Connection::open(directory.path().join("store"), None))?;

    assert!(!Arc::ptr_eq(
        &first.owner.state.context,
        &second.owner.state.context
    ));

    assert_eq!(first.info(), second.info());
    first_runtime.block_on(first.close())?;
    drop(first);
    drop(first_runtime);

    second_runtime.block_on(async {
        second.probe(|_| Ok(())).await?;
        second.close().await
    })?;

    directory.finish()?;
    Ok(())
}

#[test]
fn failed_close_retains_guard_and_closing_fence_until_retry_succeeds() -> TestResult {
    let directory = TestDirectory::new()?;

    runtime()?.block_on(async {
        let connection = Connection::open(directory.path().join("store"), None).await?;
        let manager = GuardManager::open(&connection.info().directory)?;

        connection
            .owner
            .state
            .context
            .test_hooks
            .fail_close
            .store(true, Ordering::SeqCst);

        assert!(matches!(
            connection.close().await,
            Err(ConnectionError::ShutdownFailed(_))
        ));

        assert!(matches!(
            connection.probe(|_| Ok(())).await,
            Err(ConnectionError::StoreClosing)
        ));

        assert!(
            manager
                .try_acquire(GuardTarget::Store, GuardMode::Exclusive)?
                .is_none()
        );

        connection.close().await?;

        assert!(
            manager
                .try_acquire(GuardTarget::Store, GuardMode::Exclusive)?
                .is_some()
        );

        Ok::<_, Box<dyn std::error::Error>>(())
    })?;

    directory.finish()?;
    Ok(())
}

#[test]
fn canceled_native_caller_and_dropped_close_wait_do_not_release_live_work() -> TestResult {
    let directory = TestDirectory::new()?;

    runtime()?.block_on(async {
        let connection = Connection::open(directory.path().join("store"), None).await?;
        let manager = GuardManager::open(&connection.info().directory)?;
        let file_path = directory.path().join("probe-output");
        let mut file = std::fs::File::create_new(&file_path)?;
        let (started, observed) = oneshot::channel();
        let (release, released) = std::sync::mpsc::sync_channel(1);
        let caller_connection = connection.clone();

        let caller = tokio::spawn(async move {
            caller_connection
                .probe(move |_| {
                    file.write_all(b"before\n")?;
                    crate::sync_file(&file)?;
                    let _observation = started.send(());

                    released
                        .recv_timeout(Duration::from_secs(10))
                        .map_err(io::Error::other)?;

                    file.write_all(b"after\n")?;
                    crate::sync_file(&file)
                })
                .await
        });

        tokio::time::timeout(Duration::from_secs(10), observed).await??;
        caller.abort();
        assert!(caller.await.unwrap_err().is_cancelled());

        let mut closing = Box::pin(connection.close());

        std::future::poll_fn(|context| {
            assert!(closing.as_mut().poll(context).is_pending());
            Poll::Ready(())
        })
        .await;

        drop(closing);

        assert!(matches!(
            ensure_open(&connection.owner.state),
            Err(ConnectionError::StoreClosing)
        ));

        assert!(
            manager
                .try_acquire(GuardTarget::Store, GuardMode::Exclusive)?
                .is_none()
        );

        release.send(())?;
        tokio::time::timeout(Duration::from_secs(10), connection.close()).await??;
        assert_eq!(std::fs::read(file_path)?, b"before\nafter\n");

        assert!(
            manager
                .try_acquire(GuardTarget::Store, GuardMode::Exclusive)?
                .is_some()
        );

        Ok::<_, Box<dyn std::error::Error>>(())
    })?;

    directory.finish()?;
    Ok(())
}

#[test]
fn full_queue_and_canceled_close_admission_allow_native_holder_progress() -> TestResult {
    let directory = TestDirectory::new()?;

    runtime()?.block_on(async {
        let connection = Connection::open(directory.path().join("store"), None).await?;
        connection.pause_maintenance();
        let (started, observed) = oneshot::channel();
        let (release, released) = std::sync::mpsc::sync_channel(1);
        let held = connection.clone();

        let holder = tokio::spawn(async move {
            held.probe(move |_| {
                let _observation = started.send(());

                released
                    .recv_timeout(Duration::from_secs(10))
                    .map_err(io::Error::other)
            })
            .await
        });

        tokio::time::timeout(Duration::from_secs(10), observed).await??;
        let completed = Arc::new(AtomicUsize::new(0));

        for _ in 0..QUEUE_CAPACITY {
            let completed = Arc::clone(&completed);
            let (reply, _response) = oneshot::channel();

            connection
                .owner
                .state
                .context
                .sender
                .try_send(Command::Probe {
                    _state: Arc::clone(&connection.owner.state),
                    reply,
                    work: Box::new(move |_| {
                        completed.fetch_add(1, Ordering::SeqCst);
                        Ok(())
                    }),
                })
                .map_err(|_| io::Error::other("queue filled before declared capacity"))?;
        }

        assert_eq!(connection.owner.state.context.sender.capacity(), 0);

        let mut closing = Box::pin(connection.close());

        std::future::poll_fn(|context| {
            assert!(closing.as_mut().poll(context).is_pending());
            Poll::Ready(())
        })
        .await;

        let mut concurrent = Box::pin(connection.close());

        std::future::poll_fn(|context| {
            assert!(concurrent.as_mut().poll(context).is_pending());
            Poll::Ready(())
        })
        .await;

        drop(closing);

        // The concurrent caller must take over the canceled queue admission.
        std::future::poll_fn(|context| {
            assert!(concurrent.as_mut().poll(context).is_pending());
            Poll::Ready(())
        })
        .await;

        assert!(matches!(
            ensure_open(&connection.owner.state),
            Err(ConnectionError::StoreClosing)
        ));

        release.send(())?;
        tokio::time::timeout(Duration::from_secs(10), holder).await???;
        tokio::time::timeout(Duration::from_secs(10), concurrent).await??;
        assert_eq!(completed.load(Ordering::SeqCst), QUEUE_CAPACITY);
        Ok::<_, Box<dyn std::error::Error>>(())
    })?;

    directory.finish()?;
    Ok(())
}

#[test]
fn dropping_last_alias_settles_worker_on_its_native_thread() -> TestResult {
    let directory = TestDirectory::new()?;

    runtime()?.block_on(async {
        let connection = Connection::open(directory.path().join("store"), None).await?;
        let manager = GuardManager::open(&connection.info().directory)?;
        let mut terminal = connection.owner.state.context.terminal.clone();

        let worker = connection
            .owner
            .state
            .context
            .worker
            .lock()
            .unwrap()
            .take()
            .unwrap();

        let alias = connection.clone();
        drop(connection);
        alias.probe(|_| Ok(())).await?;
        drop(alias);

        tokio::time::timeout(Duration::from_secs(10), async {
            while terminal.borrow().is_none() {
                terminal.changed().await?;
            }

            while !worker.is_finished() {
                tokio::task::yield_now().await;
            }

            Ok::<_, watch::error::RecvError>(())
        })
        .await??;

        assert!(terminal.borrow().as_ref().unwrap().is_ok());
        worker.join().unwrap();

        assert!(
            manager
                .try_acquire(GuardTarget::Store, GuardMode::Exclusive)?
                .is_some()
        );

        Ok::<_, Box<dyn std::error::Error>>(())
    })?;

    directory.finish()?;
    Ok(())
}

#[test]
fn last_alias_drop_fences_unentered_work_and_preserves_independent_connection() -> TestResult {
    let directory = TestDirectory::new()?;

    runtime()?.block_on(async {
        let path = directory.path().join("store");
        let first = Connection::open(path.clone(), None).await?;
        let alias = first.clone();
        let second = Connection::open(path, None).await?;
        let manager = GuardManager::open(&first.info().directory)?;
        let mut terminal = first.owner.state.context.terminal.clone();
        let operation = first.admit()?;

        drop(first);
        let entered = alias.admit()?.enter_commit()?;
        drop(alias);

        assert!(matches!(
            operation.enter_commit(),
            Err(ConnectionError::StoreClosing)
        ));

        second.probe(|_| Ok(())).await?;
        second.close().await?;

        assert!(
            manager
                .try_acquire(GuardTarget::Store, GuardMode::Exclusive)?
                .is_none()
        );

        // A closed wrapper remains alive, but must not retain the abandoned
        // connection's guard after its already-entered work actually settles.
        drop(entered);

        tokio::time::timeout(Duration::from_secs(10), async {
            while terminal.borrow().is_none() {
                terminal.changed().await?;
            }

            Ok::<_, watch::error::RecvError>(())
        })
        .await??;

        assert!(terminal.borrow().as_ref().unwrap().is_ok());

        assert!(
            manager
                .try_acquire(GuardTarget::Store, GuardMode::Exclusive)?
                .is_some()
        );

        assert_eq!(second.info().record.region, "us-east-1");
        Ok::<_, Box<dyn std::error::Error>>(())
    })?;

    directory.finish()?;
    Ok(())
}

#[test]
fn last_native_work_drop_releases_guard_even_with_full_notification_queue() -> TestResult {
    let directory = TestDirectory::new()?;

    runtime()?.block_on(async {
        let path = directory.path().join("store");
        let connection = Connection::open(path.clone(), None).await?;
        connection.pause_maintenance();
        let closed = Connection::open(path, None).await?;
        closed.close().await?;
        let manager = GuardManager::open(&connection.info().directory)?;
        let mut terminal = connection.owner.state.context.terminal.clone();
        let (started, observed) = oneshot::channel();
        let (release, released) = std::sync::mpsc::sync_channel(1);
        let (reply, _response) = oneshot::channel();

        connection
            .owner
            .state
            .context
            .sender
            .try_send(Command::Probe {
                _state: Arc::clone(&connection.owner.state),
                reply,
                work: Box::new(move |_| {
                    let _observation = started.send(());

                    released
                        .recv_timeout(Duration::from_secs(10))
                        .map_err(io::Error::other)
                }),
            })
            .map_err(|_| io::Error::other("probe admission failed"))?;

        tokio::time::timeout(Duration::from_secs(10), observed).await??;

        for _ in 0..QUEUE_CAPACITY {
            connection
                .owner
                .state
                .context
                .sender
                .try_send(Command::Reap)
                .map_err(|_| io::Error::other("queue filled before declared capacity"))?;
        }

        drop(connection);

        assert!(
            manager
                .try_acquire(GuardTarget::Store, GuardMode::Exclusive)?
                .is_none()
        );

        release.send(())?;

        tokio::time::timeout(Duration::from_secs(10), async {
            while terminal.borrow().is_none() {
                terminal.changed().await?;
            }

            Ok::<_, watch::error::RecvError>(())
        })
        .await??;

        assert!(terminal.borrow().as_ref().unwrap().is_ok());

        assert!(
            manager
                .try_acquire(GuardTarget::Store, GuardMode::Exclusive)?
                .is_some()
        );

        assert_eq!(closed.info().record.region, "us-east-1");
        Ok::<_, Box<dyn std::error::Error>>(())
    })?;

    directory.finish()?;
    Ok(())
}

#[test]
fn runtime_shutdown_keeps_guard_until_abandoned_native_work_settles() -> TestResult {
    let directory = TestDirectory::new()?;
    let caller_runtime = runtime()?;
    let file_path = directory.path().join("native-output");
    let mut file = std::fs::File::create_new(&file_path)?;
    let (release, released) = std::sync::mpsc::sync_channel(1);

    let (manager, mut terminal, worker) = caller_runtime.block_on(async {
        let connection = Connection::open(directory.path().join("store"), None).await?;
        let manager = GuardManager::open(&connection.info().directory)?;
        let terminal = connection.owner.state.context.terminal.clone();

        let worker = connection
            .owner
            .state
            .context
            .worker
            .lock()
            .unwrap()
            .take()
            .unwrap();

        let (started, observed) = oneshot::channel();

        // The task is the last public connection owner. Dropping its runtime
        // abandons that caller while the actual metadata thread is still busy.
        let caller = tokio::spawn(async move {
            connection
                .probe(move |_| {
                    file.write_all(b"before\n")?;
                    crate::sync_file(&file)?;
                    let _observation = started.send(());

                    released
                        .recv_timeout(Duration::from_secs(10))
                        .map_err(io::Error::other)?;

                    file.write_all(b"after\n")?;
                    crate::sync_file(&file)
                })
                .await
        });

        tokio::time::timeout(Duration::from_secs(10), observed).await??;
        drop(caller);
        Ok::<_, Box<dyn std::error::Error>>((manager, terminal, worker))
    })?;

    drop(caller_runtime);

    assert!(
        manager
            .try_acquire(GuardTarget::Store, GuardMode::Exclusive)?
            .is_none()
    );

    assert!(terminal.borrow().is_none());
    release.send(())?;

    runtime()?.block_on(async {
        tokio::time::timeout(Duration::from_secs(10), async {
            while terminal.borrow().is_none() {
                terminal.changed().await?;
            }

            while !worker.is_finished() {
                tokio::task::yield_now().await;
            }

            Ok::<_, watch::error::RecvError>(())
        })
        .await
    })??;

    assert!(terminal.borrow().as_ref().unwrap().is_ok());
    worker.join().unwrap();
    assert_eq!(std::fs::read(file_path)?, b"before\nafter\n");

    assert!(
        manager
            .try_acquire(GuardTarget::Store, GuardMode::Exclusive)?
            .is_some()
    );

    directory.finish()?;
    Ok(())
}

#[test]
#[allow(
    clippy::print_stderr,
    reason = "retain native initialization timing for watchdog diagnosis"
)]
fn canceled_open_after_real_initialization_publishes_no_handle_and_reopens_safely() -> TestResult {
    let directory = TestDirectory::new()?;
    let runtime = runtime()?;
    // Register this context before polling open so the test can pause the
    // actual initializer's result publication without replacing its work.
    let path = OpenedStore::prepare(&directory.path().join("store"), None)?;
    let manager = GuardManager::open(&path)?;
    let context = context(runtime.handle().id(), manager.identity(), &path)?;
    let (initialized, observed) = oneshot::channel();
    let (release, released) = std::sync::mpsc::sync_channel(1);

    *context.test_hooks.before_open_reply.lock().unwrap() = Some(Box::new(move |info| {
        let _observation = initialized.send(info.clone());
        released.recv_timeout(Duration::from_secs(10)).unwrap();
    }));

    let mut terminal = context.terminal.clone();
    let worker = context.worker.lock().unwrap().take().unwrap();
    let started = std::time::Instant::now();
    let caller = runtime.spawn(Connection::open(path.clone(), Some("eu-west-1".into())));

    // This bounds test setup, not production admission or native I/O. Fresh
    // initialization includes 256 guard-file flushes and can exceed ten seconds
    // when other tests and a Docker build compete for the same host storage.
    let initialized = runtime.block_on(async {
        tokio::time::timeout(Duration::from_secs(60), observed)
            .await
            .map_err(|error| {
                io::Error::other(format!(
                    "initializer result watchdog: {error}; open attempts: {}; caller finished: {}",
                    context.test_hooks.open_attempts.load(Ordering::SeqCst),
                    caller.is_finished(),
                ))
            })
    });

    eprintln!(
        "native initialization observation after {:?}",
        started.elapsed()
    );

    caller.abort();
    let canceled = runtime.block_on(caller);

    let protected = manager
        .try_acquire(GuardTarget::Store, GuardMode::Exclusive)
        .map(|guard| guard.is_none());

    drop(context);
    let released = release.send(());
    drop(release);

    let settled = runtime.block_on(async {
        tokio::time::timeout(Duration::from_secs(10), async {
            while terminal.borrow().is_none() {
                terminal.changed().await?;
            }

            while !worker.is_finished() {
                tokio::task::yield_now().await;
            }

            Ok::<_, watch::error::RecvError>(())
        })
        .await
        .map_err(|error| io::Error::other(format!("canceled-open worker shutdown: {error}")))
    });

    // Settle the actual owner before inspecting fallible observations or removing
    // the directory, including when setup timed out. The harness additionally
    // owns a process watchdog for uninterruptible native I/O.
    let joined = worker.join();
    let initialized = initialized??;
    assert!(canceled.is_err_and(|error| error.is_cancelled()));
    assert!(protected?);
    released?;
    settled??;
    joined.unwrap();
    assert!(terminal.borrow().as_ref().unwrap().is_ok());

    assert!(
        manager
            .try_acquire(GuardTarget::Store, GuardMode::Exclusive)?
            .is_some()
    );

    runtime.block_on(async {
        let reopened = Connection::open(path, None).await?;
        assert_eq!(reopened.info(), &initialized);
        reopened.close().await?;
        Ok::<_, Box<dyn std::error::Error>>(())
    })?;

    directory.finish()?;
    Ok(())
}

#[test]
fn busy_open_reuses_one_worker_and_retries_without_blocking_runtime() -> TestResult {
    let directory = TestDirectory::new()?;
    let path = directory.path().join("store");

    OpenedStore::open(&path, None)?
        .close()
        .map_err(|(_, error)| error)?;

    let manager = GuardManager::open(&path)?;

    let guard = manager
        .try_acquire(GuardTarget::Store, GuardMode::Exclusive)?
        .unwrap();

    runtime()?.block_on(async {
        let key = (Handle::current().id(), manager.identity());
        let caller = tokio::spawn(Connection::open(path, None));

        let (original, hooks) = tokio::time::timeout(Duration::from_secs(10), async {
            loop {
                let owner = CONTEXTS.get().and_then(|registry| {
                    registry
                        .lock()
                        .ok()
                        .and_then(|registry| registry.get(&key).and_then(Weak::upgrade))
                });

                if let Some(owner) = owner {
                    return (Arc::downgrade(&owner), Arc::clone(&owner.test_hooks));
                }

                tokio::task::yield_now().await;
            }
        })
        .await?;

        tokio::time::timeout(Duration::from_secs(10), async {
            while hooks.open_attempts.load(Ordering::SeqCst) < 3 {
                tokio::task::yield_now().await;
            }
        })
        .await?;

        assert_eq!(tokio::spawn(async { 42 }).await?, 42);
        drop(guard);
        let connection = tokio::time::timeout(Duration::from_secs(10), caller).await???;

        assert!(Arc::ptr_eq(
            &original.upgrade().unwrap(),
            &connection.owner.state.context
        ));

        connection.close().await?;
        Ok::<_, Box<dyn std::error::Error>>(())
    })?;

    directory.finish()?;
    Ok(())
}

#[test]
fn concurrent_initializers_on_independent_runtimes_converge_on_one_identity() -> TestResult {
    #[allow(
        clippy::print_stderr,
        reason = "retain exact contention timing in test evidence"
    )]
    fn open_after_contention(
        path: PathBuf,
        barrier: &std::sync::Barrier,
    ) -> Result<(ConnectionInfo, bool), Box<dyn std::error::Error + Send + Sync>> {
        let runtime = runtime()?;
        barrier.wait();
        let started = std::time::Instant::now();
        let initial = runtime.block_on(Connection::open(path.clone(), None));

        eprintln!(
            "initial concurrent open: {:?}; elapsed {:?}",
            initial.as_ref().map(Connection::info),
            started.elapsed()
        );

        let initialized = initial.is_ok();
        // A contender may exhaust its unchanged admission budget while the
        // winner still initializes. Wait for both actual attempts to settle
        // before retrying StoreBusy, instead of guessing a retry count. Each
        // thread retains its own runtime and any successfully opened handle.
        barrier.wait();

        runtime.block_on(async {
            let connection = match initial {
                Err(ConnectionError::Open(error)) if error.failure() == OpenFailure::StoreBusy => {
                    Connection::open(path, None).await?
                }
                result => result?,
            };

            let info = connection.info().clone();
            connection.close().await?;
            Ok((info, initialized))
        })
    }

    let directory = TestDirectory::new()?;
    let path = directory.path().join("store");
    let barrier = std::sync::Barrier::new(2);

    let (first, second) = std::thread::scope(|scope| {
        let first = scope.spawn(|| open_after_contention(path.clone(), &barrier));
        let second = scope.spawn(|| open_after_contention(path.clone(), &barrier));

        (
            first.join().unwrap().unwrap(),
            second.join().unwrap().unwrap(),
        )
    });

    assert!(
        first.1 || second.1,
        "neither concurrent attempt initialized"
    );

    assert_eq!(first.0, second.0);
    directory.finish()?;
    Ok(())
}

#[test]
fn exhausted_open_budget_reports_busy_and_preserves_the_held_store() -> TestResult {
    let directory = TestDirectory::new()?;
    let path = directory.path().join("store");
    let native = OpenedStore::open(&path, None)?;
    let expected = native.record().clone();
    native.close().map_err(|(_, error)| error)?;
    let manager = GuardManager::open(&path)?;

    let guard = manager
        .try_acquire(GuardTarget::Store, GuardMode::Exclusive)?
        .unwrap();

    runtime()?.block_on(async {
        let result = Connection::open(path.clone(), None).await;

        assert!(matches!(result, Err(ConnectionError::Open(error))
            if error.failure() == OpenFailure::StoreBusy));

        assert!(
            manager
                .try_acquire(GuardTarget::Store, GuardMode::Shared)?
                .is_none()
        );

        drop(guard);

        let reopened = Connection::open(path, None).await?;
        assert_eq!(reopened.info().record, expected);
        reopened.close().await?;
        Ok::<_, Box<dyn std::error::Error>>(())
    })?;

    directory.finish()?;
    Ok(())
}

#[test]
fn close_wins_commit_fence_and_refuses_unstarted_publication() -> TestResult {
    let directory = TestDirectory::new()?;

    runtime()?.block_on(async {
        let connection = Connection::open(directory.path().join("store"), None).await?;
        let operation = connection.admit()?;
        let mut closing = Box::pin(connection.close());

        std::future::poll_fn(|context| {
            assert!(closing.as_mut().poll(context).is_pending());
            Poll::Ready(())
        })
        .await;

        assert!(matches!(
            connection.admit(),
            Err(ConnectionError::StoreClosing)
        ));

        assert!(matches!(
            operation.enter_commit(),
            Err(ConnectionError::StoreClosing)
        ));

        tokio::time::timeout(Duration::from_secs(10), closing).await??;

        assert!(matches!(
            connection.admit(),
            Err(ConnectionError::StoreClosed)
        ));

        Ok::<_, Box<dyn std::error::Error>>(())
    })?;

    directory.finish()?;
    Ok(())
}

#[test]
fn entered_commit_owner_keeps_guard_until_actual_native_work_settles() -> TestResult {
    let directory = TestDirectory::new()?;

    runtime()?.block_on(async {
        let connection = Connection::open(directory.path().join("store"), None).await?;
        let manager = GuardManager::open(&connection.info().directory)?;
        let commit = connection.admit()?.enter_commit()?;
        let path = directory.path().join("private-commit-fixture");
        let mut file = std::fs::File::create_new(&path)?;
        let (started, observed) = oneshot::channel();
        let (release, released) = std::sync::mpsc::sync_channel(1);

        let native = tokio::task::spawn_blocking(move || -> io::Result<()> {
            let _commit = commit;
            file.write_all(b"before close\n")?;
            crate::sync_file(&file)?;
            let _observation = started.send(());

            released
                .recv_timeout(Duration::from_secs(10))
                .map_err(io::Error::other)?;

            file.write_all(b"settled after close fence\n")?;
            crate::sync_file(&file)
        });

        tokio::time::timeout(Duration::from_secs(10), observed).await??;
        let mut closing = Box::pin(connection.close());

        std::future::poll_fn(|context| {
            assert!(closing.as_mut().poll(context).is_pending());
            Poll::Ready(())
        })
        .await;

        assert!(
            manager
                .try_acquire(GuardTarget::Store, GuardMode::Exclusive)?
                .is_none()
        );

        release.send(())?;
        tokio::time::timeout(Duration::from_secs(10), native).await???;
        tokio::time::timeout(Duration::from_secs(10), closing).await??;

        assert_eq!(
            std::fs::read(path)?,
            b"before close\nsettled after close fence\n"
        );

        assert!(
            manager
                .try_acquire(GuardTarget::Store, GuardMode::Exclusive)?
                .is_some()
        );

        Ok::<_, Box<dyn std::error::Error>>(())
    })?;

    directory.finish()?;
    Ok(())
}

fn access_request() -> crate::OwnedAccessRequest {
    crate::AccessRequest {
        bucket: "photos",
        expected_bucket_id: None,
        key: Some("album/photo.jpg"),
        action: crate::access::Action::parse("s3:GetObject").unwrap(),
        context: &crate::access::Context::default(),
        expected_owner: None,
    }
    .into_owned()
    .unwrap()
}

#[test]
fn access_commands_use_current_sql_and_never_default_to_native_authority() -> TestResult {
    const KEY: &str = "mikrio_a00112233445566778899AABBCCDDEEFF";
    let directory = TestDirectory::new()?;
    let runtime = runtime()?;
    let connection = runtime.block_on(Connection::open(directory.path().join("store"), None))?;

    let database =
        rusqlite::Connection::open(connection.info().directory.join("metadata.sqlite3"))?;

    database.execute_batch(r#"
        PRAGMA foreign_keys=ON;
        INSERT INTO principals(principal_id,name,is_enabled,created_at_ms,policy_generation,policy_json)
        VALUES(zeroblob(16),'app',1,0,randomblob(16),'{"Version":"2012-10-17","Statement":{"Effect":"Allow","Action":"s3:GetObject","Resource":"*"}}');
        INSERT INTO access_keys(access_key_id,principal_id,secret_access_key,created_at_ms) VALUES('mikrio_a00112233445566778899AABBCCDDEEFF',zeroblob(16),'test-secret',0);
        INSERT INTO buckets(bucket_id,name,created_at_ms) VALUES(randomblob(16),'photos',0);
    "#)?;

    let identity = Arc::new(
        runtime
            .block_on(connection.resolve_application_key(KEY.into()))?
            .verified(),
    );

    runtime.block_on(connection.authorize_http(Arc::clone(&identity), vec![access_request()]))?;

    assert!(matches!(
        runtime.block_on(connection.authorize_http(
            Arc::new(crate::access::HttpIdentity::anonymous()),
            vec![access_request()]
        )),
        Err(crate::AccessError::Denied)
    ));

    database.execute("DELETE FROM access_keys", [])?;

    assert!(matches!(
        runtime.block_on(connection.authorize_http(identity, vec![access_request()])),
        Err(crate::AccessError::Denied)
    ));

    assert!(matches!(
        runtime.block_on(connection.resolve_application_key(KEY.into())),
        Err(crate::AccessError::Denied)
    ));

    runtime.block_on(connection.close())?;

    assert!(matches!(
        runtime.block_on(connection.resolve_application_key(KEY.into())),
        Err(crate::AccessError::Connection(ConnectionError::StoreClosed))
    ));

    database.close().map_err(|(_, error)| error)?;
    directory.finish()?;
    Ok(())
}

#[test]
fn abandoned_queued_access_retains_its_operation_until_native_settlement() -> TestResult {
    let directory = TestDirectory::new()?;

    runtime()?.block_on(async {
        let connection = Connection::open(directory.path().join("store"), None).await?;
        connection.pause_maintenance();
        let (entered, entered_reply) = oneshot::channel();
        let (release, released) = std::sync::mpsc::sync_channel(1);
        let worker_owner = connection.clone();

        let blocker = tokio::spawn(async move {
            worker_owner
                .probe(move |_| {
                    entered
                        .send(())
                        .map_err(|()| io::Error::other("test waiter disappeared"))?;

                    released
                        .recv_timeout(Duration::from_secs(10))
                        .map_err(io::Error::other)
                })
                .await
        });

        entered_reply.await?;

        let mut access = Box::pin(connection.authorize_http(
            Arc::new(crate::access::HttpIdentity::anonymous()),
            vec![access_request()],
        ));

        std::future::poll_fn(|cx| {
            assert!(access.as_mut().poll(cx).is_pending());
            Poll::Ready(())
        })
        .await;

        assert_eq!(
            connection.owner.state.context.sender.capacity(),
            QUEUE_CAPACITY - 1
        );

        drop(access);

        let mut close = Box::pin(connection.close());

        std::future::poll_fn(|cx| {
            assert!(close.as_mut().poll(cx).is_pending());
            Poll::Ready(())
        })
        .await;

        assert_eq!(
            connection.owner.state.activity.lock().unwrap().operations,
            1
        );

        assert!(matches!(
            connection.admit(),
            Err(ConnectionError::StoreClosing)
        ));

        let manager = GuardManager::open(&connection.info().directory)?;

        assert!(
            manager
                .try_acquire(GuardTarget::Store, GuardMode::Exclusive)?
                .is_none()
        );

        release.send(())?;
        blocker.await??;
        close.await?;

        let guard = manager
            .try_acquire(GuardTarget::Store, GuardMode::Exclusive)?
            .ok_or_else(|| io::Error::other("settled access retained store use"))?;

        drop(guard);
        Ok::<_, Box<dyn std::error::Error>>(())
    })?;

    directory.finish()?;
    Ok(())
}
