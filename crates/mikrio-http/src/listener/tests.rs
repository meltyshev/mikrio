#![allow(
    clippy::unwrap_used,
    reason = "controlled shutdown ownership test assertions"
)]

use std::cell::RefCell;
use std::pin::Pin;
use std::time::Duration;

use mikrio_core::object::BucketName;
use tokio::sync::oneshot;

use super::*;
use crate::tests::TestDirectory;
use crate::tests::cleanup::Scope;
use crate::tests::report_case;

type TestResult = Result<(), Box<dyn std::error::Error>>;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum Startup {
    Reserved,
    Bound,
    Publishing,
    Ready,
}

struct StartupPause {
    stage: Startup,
    entered: oneshot::Sender<Option<SocketAddr>>,
    released: oneshot::Receiver<()>,
}

thread_local! {
    // Each test owns a current-thread runtime. No switch exists in ordinary builds.
    static STARTUP: RefCell<Option<StartupPause>> = const { RefCell::new(None) };
}

pub(super) async fn startup_checkpoint(stage: Startup, address: Option<SocketAddr>) {
    let pause = STARTUP.with_borrow_mut(|slot| {
        if slot.as_ref().is_some_and(|pause| pause.stage == stage) {
            slot.take()
        } else {
            None
        }
    });

    let Some(pause) = pause else {
        return;
    };

    pause.entered.send(address).unwrap();
    pause.released.await.unwrap();
}

fn pause_startup(stage: Startup) -> (oneshot::Receiver<Option<SocketAddr>>, oneshot::Sender<()>) {
    let (entered, observed) = oneshot::channel();
    let (release, released) = oneshot::channel();

    STARTUP.with_borrow_mut(|slot| {
        assert!(
            slot.replace(StartupPause {
                stage,
                entered,
                released
            })
            .is_none()
        );
    });

    (observed, release)
}

#[test]
fn canceled_startup_releases_socket_and_slot_at_every_boundary() -> TestResult {
    Scope::run(TestDirectory::new()?, |cleanup| {
        let runtime = &cleanup.runtime;
        let directory = &cleanup.directory;

        runtime.block_on(async {
            tokio::time::timeout(Duration::from_secs(30), async {
                let connection =
                    cleanup.track(Connection::open(directory.path().join("store"), None).await?);

                let bucket = BucketName::parse("canceled-start")?;
                connection.create_bucket(bucket.clone()).await?;

                for stage in [
                    Startup::Reserved,
                    Startup::Bound,
                    Startup::Publishing,
                    Startup::Ready,
                ] {
                    report_case(format_args!("stage={stage:?}"));

                    let (mut observed, _release) = pause_startup(stage);
                    let bootstrap = Arc::new(Bootstrap::generate()?);

                    let identity = Arc::new(
                        bootstrap
                            .resolve(bootstrap.access_key_id())
                            .unwrap()
                            .verified(),
                    );

                    connection
                        .head_bucket_http(Arc::clone(&identity), bucket.clone(), None)
                        .await?;

                    let mut starting =
                        Box::pin(Listener::bind(&connection, "127.0.0.1", 0, None, bootstrap));

                    let address = poll_fn(|cx| {
                        assert!(starting.as_mut().poll(cx).is_pending());
                        Pin::new(&mut observed).poll(cx)
                    })
                    .await?;

                    assert!(matches!(
                        connection.reserve_listener(),
                        Err(ConnectionError::ListenerAlreadyRunning)
                    ));

                    if let Some(address) = address {
                        assert_eq!(
                            std::net::TcpListener::bind(address).unwrap_err().kind(),
                            io::ErrorKind::AddrInUse,
                            "{stage:?}"
                        );
                    }

                    drop(starting);

                    let denied = connection
                        .head_bucket_http(identity, bucket.clone(), None)
                        .await
                        .unwrap_err();

                    assert_eq!(
                        denied.failure(),
                        mikrio_core::OperationFailure::AccessDenied
                    );

                    let replacement = Listener::bind(
                        &connection,
                        "127.0.0.1",
                        address.map_or(0, |address| address.port()),
                        None,
                        Arc::new(Bootstrap::generate()?),
                    )
                    .await?;

                    replacement.close().await?;
                }

                connection
                    .create_bucket(BucketName::parse("direct-after-cancellation")?)
                    .await?;

                connection.close().await?;
                Ok::<_, Box<dyn std::error::Error>>(())
            })
            .await?
        })?;

        Ok(())
    })
}

#[test]
#[allow(
    clippy::print_stderr,
    reason = "retain fixture and listener settlement timing separately"
)]
fn store_close_at_startup_waits_for_socket_and_readiness_settlement() -> TestResult {
    Scope::run(TestDirectory::new()?.retain_on_failure(), |cleanup| {
        let runtime = &cleanup.runtime;
        let directory = &cleanup.directory;

        runtime.block_on(async {
            let preparing = std::time::Instant::now();

            // Durable store initialization is fixture work, not listener settlement.
            // Keep all four close races under the same unchanged 30-second deadline.
            for stage in [
                Startup::Reserved,
                Startup::Bound,
                Startup::Publishing,
                Startup::Ready,
            ] {
                let path = directory.path().join(format!("store-{stage:?}"));

                cleanup
                    .track(Connection::open(path, None).await?)
                    .close()
                    .await?;
            }

            eprintln!("startup fixture preparation: {:?}", preparing.elapsed());
            let settling = std::time::Instant::now();

            let result = tokio::time::timeout(Duration::from_secs(30), async {
                for stage in [
                    Startup::Reserved,
                    Startup::Bound,
                    Startup::Publishing,
                    Startup::Ready,
                ] {
                    report_case(format_args!("stage={stage:?}"));

                    let path = directory.path().join(format!("store-{stage:?}"));
                    let connection = cleanup.track(Connection::open(path.clone(), None).await?);
                    let peer = cleanup.track(Connection::open(path, None).await?);
                    let (mut observed, release) = pause_startup(stage);

                    let mut starting = Box::pin(Listener::bind(
                        &connection,
                        "127.0.0.1",
                        0,
                        None,
                        Arc::new(Bootstrap::generate()?),
                    ));

                    let address = poll_fn(|cx| {
                        assert!(starting.as_mut().poll(cx).is_pending());
                        Pin::new(&mut observed).poll(cx)
                    })
                    .await?;

                    let mut closing = Box::pin(connection.close());

                    poll_fn(|cx| {
                        assert!(closing.as_mut().poll(cx).is_pending());
                        Poll::Ready(())
                    })
                    .await;

                    release.send(()).unwrap();

                    match starting.await {
                        Ok(listener) => {
                            assert_eq!(stage, Startup::Ready);
                            listener.close().await?;
                        }
                        Err(error) => {
                            assert_ne!(stage, Startup::Ready);
                            assert!(matches!(error, ConnectionError::StoreClosing));
                        }
                    }

                    closing.await?;

                    if let Some(address) = address {
                        drop(std::net::TcpListener::bind(address)?);
                    }

                    peer.create_bucket(BucketName::parse("independent-peer")?)
                        .await?;

                    peer.close().await?;
                }

                Ok::<_, Box<dyn std::error::Error>>(())
            })
            .await?;

            eprintln!("four startup close races: {:?}", settling.elapsed());
            result
        })?;

        Ok(())
    })
}

fn fixture(connection: &Connection) -> Result<(Listener, Driver), Box<dyn std::error::Error>> {
    let lease = connection.reserve_listener()?;
    let (attempt, response) = watch::channel(None);
    let attempts = Arc::new(Mutex::new(response));
    let (finished, response) = watch::channel(None);
    let (retry, retries) = mpsc::channel(1);

    let listener = Listener {
        owner: Arc::new(Owner {
            control: lease.control(),
            attempt: Arc::clone(&attempts),
            retry,
            finished: response,
        }),
        endpoint: "http://127.0.0.1:1".into(),
    };

    let driver = Driver {
        lease,
        bootstrap: Arc::new(Bootstrap::generate()?),
        attempt,
        attempts,
        retries,
        finished,
    };

    Ok((listener, driver))
}

#[test]
fn failed_listener_close_retains_slot_and_retry_preserves_original_error() -> TestResult {
    Scope::run(TestDirectory::new()?, |cleanup| {
        let runtime = &cleanup.runtime;
        let directory = &cleanup.directory;

        runtime.block_on(async {
            let connection =
                cleanup.track(Connection::open(directory.path().join("store"), None).await?);

            let (listener, driver) = fixture(&connection)?;
            assert!(listener.closed().now_or_never().is_none());
            let original = listener.owner.attempt.lock().unwrap().clone();
            let owner = tokio::spawn(driver.settle(Some(shutdown("controlled transport failure"))));

            assert!(matches!(
                Completion(original.clone()).wait().await,
                Err(ConnectionError::ShutdownFailed(_))
            ));

            assert!(matches!(
                listener.closed().await,
                Err(ConnectionError::ShutdownFailed(_))
            ));

            assert!(listener.completion().wait().now_or_never().is_none());

            assert!(matches!(
                connection.reserve_listener(),
                Err(ConnectionError::ListenerAlreadyRunning)
            ));

            connection
                .create_bucket(BucketName::parse("direct-after-failure")?)
                .await?;

            listener.close().await?;
            listener.close().await?;
            listener.closed().await?;
            listener.completion().wait().await?;

            assert!(matches!(
                Completion(original).wait().await,
                Err(ConnectionError::ShutdownFailed(_))
            ));

            owner.await?;
            let replacement = connection.reserve_listener()?;
            replacement.finish().await?;
            drop(replacement);
            connection.close().await?;
            Ok::<_, Box<dyn std::error::Error>>(())
        })?;

        Ok(())
    })
}

#[test]
fn parent_close_retries_failed_listener_cleanup() -> TestResult {
    Scope::run(TestDirectory::new()?, |cleanup| {
        let runtime = &cleanup.runtime;
        let directory = &cleanup.directory;

        runtime.block_on(async {
            let connection =
                cleanup.track(Connection::open(directory.path().join("store"), None).await?);

            let (listener, driver) = fixture(&connection)?;
            let original = listener.owner.attempt.lock().unwrap().clone();
            let owner = tokio::spawn(driver.settle(Some(shutdown("controlled transport failure"))));

            assert!(matches!(
                Completion(original).wait().await,
                Err(ConnectionError::ShutdownFailed(_))
            ));

            tokio::time::timeout(std::time::Duration::from_secs(10), connection.close()).await??;
            listener.close().await?;
            owner.await?;
            Ok::<_, Box<dyn std::error::Error>>(())
        })?;

        Ok(())
    })
}
