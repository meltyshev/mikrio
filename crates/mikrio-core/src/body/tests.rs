use std::error::Error as _;
use std::future::Future;
use std::io::Cursor;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};

use tokio::io::AsyncRead;
use tokio::sync::oneshot;

use super::*;

#[derive(Default)]
struct Observations {
    canceled: AtomicBool,
    dropped: AtomicBool,
    read_capacity: AtomicUsize,
    bytes_read: AtomicUsize,
}

struct Source {
    bytes: Cursor<Vec<u8>>,
    observations: Arc<Observations>,
    block_read: bool,
    fail_read: bool,
    fail_close: bool,
    terminal_failure: Option<BodyError>,
    release: Option<oneshot::Receiver<()>>,
}

impl Source {
    fn new(bytes: Vec<u8>) -> Self {
        Self {
            bytes: Cursor::new(bytes),
            observations: Arc::default(),
            block_read: false,
            fail_read: false,
            fail_close: false,
            terminal_failure: None,
            release: None,
        }
    }
}

impl AsyncRead for Source {
    fn poll_read(
        mut self: Pin<&mut Self>,
        context: &mut Context<'_>,
        buffer: &mut ReadBuf<'_>,
    ) -> Poll<io::Result<()>> {
        self.observations
            .read_capacity
            .fetch_max(buffer.remaining(), Ordering::SeqCst);

        if self.fail_read {
            return Poll::Ready(Err(io::Error::from_raw_os_error(12345)));
        }

        if self.block_read {
            return Poll::Pending;
        }

        let before = buffer.filled().len();
        let result = Pin::new(&mut self.bytes).poll_read(context, buffer);

        self.observations
            .bytes_read
            .fetch_add(buffer.filled().len() - before, Ordering::SeqCst);

        result
    }
}

impl ReadSource for Source {
    fn poll_read(
        mut self: Pin<&mut Self>,
        context: &mut Context<'_>,
        buffer: &mut ReadBuf<'_>,
    ) -> Poll<Result<(), BodyError>> {
        if let Some(failure) = self.terminal_failure.take() {
            return Poll::Ready(Err(failure));
        }

        AsyncRead::poll_read(self, context, buffer).map(|result| result.map_err(BodyError::from))
    }

    fn cancel(self: Pin<&mut Self>) {
        self.observations.canceled.store(true, Ordering::SeqCst);
    }

    fn poll_close(mut self: Pin<&mut Self>, context: &mut Context<'_>) -> Poll<io::Result<()>> {
        if std::mem::take(&mut self.fail_close) {
            return Poll::Ready(Err(io::Error::from_raw_os_error(12346)));
        }

        if let Some(release) = &mut self.release {
            std::task::ready!(Pin::new(release).poll(context)).map_err(io::Error::other)?;
            self.release = None;
        }

        Poll::Ready(Ok(()))
    }
}

impl Drop for Source {
    fn drop(&mut self) {
        self.observations.dropped.store(true, Ordering::SeqCst);
    }
}

fn runtime() -> tokio::runtime::Runtime {
    tokio::runtime::Builder::new_current_thread()
        .enable_time()
        .build()
        .unwrap()
}

#[test]
fn bounded_reads_release_at_eof_and_keep_eof_distinct_from_close() {
    runtime().block_on(async {
        let source = Source::new(vec![0x5a; CHUNK_BYTES + 3]);
        let observations = Arc::clone(&source.observations);
        let mut body = ReadBody::new(source);

        assert_eq!(
            body.read_chunk(0).await.unwrap_err().failure(),
            BodyFailure::InvalidArgument
        );

        assert!(!observations.canceled.load(Ordering::SeqCst));

        assert_eq!(
            body.read_chunk(usize::MAX).await.unwrap().unwrap(),
            vec![0x5a; CHUNK_BYTES]
        );

        assert_eq!(body.read_all(3).await.unwrap(), vec![0x5a; 3]);
        assert!(observations.dropped.load(Ordering::SeqCst));
        assert!(observations.read_capacity.load(Ordering::SeqCst) <= CHUNK_BYTES);
        assert_eq!(body.read_chunk(1).await.unwrap(), None);
        assert_eq!(body.read_all(0).await.unwrap(), Vec::<u8>::new());

        body.close().await.unwrap();
        body.close().await.unwrap();

        assert_eq!(
            body.read_chunk(1).await.unwrap_err().failure(),
            BodyFailure::Closed
        );

        assert_eq!(
            body.read_all(0).await.unwrap_err().failure(),
            BodyFailure::Closed
        );
    });
}

#[test]
fn collection_growth_keeps_bytes_exact_and_capacity_within_the_limit() {
    runtime().block_on(async {
        let length = 5 * CHUNK_BYTES + 17;
        let expected: Vec<_> = (0_u8..=255).cycle().take(length).collect();

        for limit in [length, length + 23, 16 * CHUNK_BYTES, usize::MAX] {
            let source = Source::new(expected.clone());
            let observations = Arc::clone(&source.observations);
            let mut body = ReadBody::new(source);
            let bytes = body.read_all(limit).await.unwrap();

            assert_eq!(bytes, expected, "limit={limit}");
            assert!(bytes.capacity() <= limit, "limit={limit}");
            assert!(observations.dropped.load(Ordering::SeqCst), "limit={limit}");
            assert_eq!(body.read_chunk(1).await.unwrap(), None, "limit={limit}");
        }
    });
}

#[test]
fn collection_overflow_reads_only_one_byte_past_the_limit_and_closes() {
    runtime().block_on(async {
        for limit in [0, 3, CHUNK_BYTES, CHUNK_BYTES + 1] {
            let source = Source::new(vec![0xa5; 4 * CHUNK_BYTES]);
            let observations = Arc::clone(&source.observations);
            let mut body = ReadBody::new(source);

            assert_eq!(
                body.read_all(limit).await.unwrap_err().failure(),
                BodyFailure::TooLarge,
                "limit={limit}"
            );

            assert!(
                observations.canceled.load(Ordering::SeqCst),
                "limit={limit}"
            );

            assert!(observations.dropped.load(Ordering::SeqCst), "limit={limit}");
            assert_eq!(observations.bytes_read.load(Ordering::SeqCst), limit + 1);

            assert!(
                observations.read_capacity.load(Ordering::SeqCst) <= CHUNK_BYTES.min(limit + 1),
                "limit={limit}"
            );

            assert_eq!(
                body.read_chunk(1).await.unwrap_err().failure(),
                BodyFailure::Closed,
                "limit={limit}"
            );
        }

        let mut empty = ReadBody::new(Source::new(Vec::new()));
        assert_eq!(empty.read_all(0).await.unwrap(), Vec::<u8>::new());
    });
}

#[test]
fn abandoning_a_read_cancels_the_cursor_and_abandoning_close_keeps_cleanup_joinable() {
    runtime().block_on(async {
        let (release, released) = oneshot::channel();
        let mut source = Source::new(vec![42]);
        source.block_read = true;
        source.release = Some(released);
        let observations = Arc::clone(&source.observations);
        let mut body = ReadBody::new(source);
        let mut reading = Box::pin(body.read_chunk(1));

        assert!(
            poll_fn(|context| Poll::Ready(reading.as_mut().poll(context)))
                .await
                .is_pending()
        );

        drop(reading);
        assert!(observations.canceled.load(Ordering::SeqCst));

        assert_eq!(
            body.read_chunk(1).await.unwrap_err().failure(),
            BodyFailure::Closed
        );

        let mut closing = Box::pin(body.close());

        assert!(
            poll_fn(|context| Poll::Ready(closing.as_mut().poll(context)))
                .await
                .is_pending()
        );

        drop(closing);
        assert!(!observations.dropped.load(Ordering::SeqCst));

        release.send(()).unwrap();
        body.close().await.unwrap();
        assert!(observations.dropped.load(Ordering::SeqCst));
    });
}

#[test]
fn read_failure_preserves_its_cause_and_cleanup_failure_can_be_retried() {
    runtime().block_on(async {
        let mut source = Source::new(vec![1]);
        source.fail_read = true;
        source.fail_close = true;
        let observations = Arc::clone(&source.observations);
        let mut body = ReadBody::new(source);
        let failure = body.read_chunk(1).await.unwrap_err();

        assert_eq!(failure.failure(), BodyFailure::Io);

        assert_eq!(
            failure
                .source()
                .unwrap()
                .downcast_ref::<io::Error>()
                .unwrap()
                .raw_os_error(),
            Some(12345)
        );

        assert!(observations.canceled.load(Ordering::SeqCst));

        assert_eq!(
            body.read_all(1).await.unwrap_err().failure(),
            BodyFailure::Closed
        );

        let failure = body.close().await.unwrap_err();

        assert_eq!(
            failure
                .source()
                .unwrap()
                .downcast_ref::<io::Error>()
                .unwrap()
                .raw_os_error(),
            Some(12346)
        );

        assert!(!observations.dropped.load(Ordering::SeqCst));
        body.close().await.unwrap();
        assert!(observations.dropped.load(Ordering::SeqCst));
    });
}

#[test]
fn collection_yields_and_cancellation_after_progress_makes_the_cursor_terminal() {
    runtime().block_on(async {
        let source = Source::new(vec![7; 4 * CHUNK_BYTES]);
        let observations = Arc::clone(&source.observations);
        let mut body = ReadBody::new(source);
        let mut reading = Box::pin(body.read_all(usize::MAX));

        assert!(
            poll_fn(|context| Poll::Ready(reading.as_mut().poll(context)))
                .await
                .is_pending()
        );

        assert_eq!(
            observations.read_capacity.load(Ordering::SeqCst),
            CHUNK_BYTES
        );

        drop(reading);
        assert!(observations.canceled.load(Ordering::SeqCst));

        assert_eq!(
            body.read_chunk(1).await.unwrap_err().failure(),
            BodyFailure::Closed
        );

        body.close().await.unwrap();
        assert!(observations.dropped.load(Ordering::SeqCst));
    });
}

#[test]
fn eof_waits_for_cleanup_and_cancellation_does_not_publish_an_eof_state() {
    runtime().block_on(async {
        let (release, released) = oneshot::channel();
        let mut source = Source::new(Vec::new());
        source.release = Some(released);
        let observations = Arc::clone(&source.observations);
        let mut body = ReadBody::new(source);
        let mut reading = Box::pin(body.read_chunk(1));

        assert!(
            poll_fn(|context| Poll::Ready(reading.as_mut().poll(context)))
                .await
                .is_pending()
        );

        assert!(!observations.dropped.load(Ordering::SeqCst));
        drop(reading);

        assert_eq!(
            body.read_chunk(1).await.unwrap_err().failure(),
            BodyFailure::Closed
        );

        release.send(()).unwrap();
        body.close().await.unwrap();
        assert!(observations.dropped.load(Ordering::SeqCst));

        assert_eq!(
            body.read_chunk(1).await.unwrap_err().failure(),
            BodyFailure::Closed
        );
    });
}

#[test]
fn collection_limit_failure_retains_a_cleanup_error_and_its_resource_owner() {
    runtime().block_on(async {
        let mut source = Source::new(vec![42]);
        source.fail_close = true;
        let observations = Arc::clone(&source.observations);
        let mut body = ReadBody::new(source);
        let failure = body.read_all(0).await.unwrap_err();

        assert_eq!(failure.failure(), BodyFailure::TooLarge);

        assert_eq!(
            failure
                .source()
                .unwrap()
                .downcast_ref::<io::Error>()
                .unwrap()
                .raw_os_error(),
            Some(12346)
        );

        assert!(!observations.dropped.load(Ordering::SeqCst));

        assert_eq!(
            body.read_chunk(1).await.unwrap_err().failure(),
            BodyFailure::Closed
        );

        body.close().await.unwrap();
        assert!(observations.dropped.load(Ordering::SeqCst));
    });
}

#[cfg(unix)]
struct NativeSource {
    canceled: Arc<AtomicBool>,
    finished: oneshot::Receiver<()>,
}

#[cfg(unix)]
impl AsyncRead for NativeSource {
    fn poll_read(
        self: Pin<&mut Self>,
        _context: &mut Context<'_>,
        _buffer: &mut ReadBuf<'_>,
    ) -> Poll<io::Result<()>> {
        Poll::Pending
    }
}

#[cfg(unix)]
impl ReadSource for NativeSource {
    fn poll_read(
        self: Pin<&mut Self>,
        context: &mut Context<'_>,
        buffer: &mut ReadBuf<'_>,
    ) -> Poll<Result<(), BodyError>> {
        AsyncRead::poll_read(self, context, buffer).map(|result| result.map_err(BodyError::from))
    }

    fn cancel(self: Pin<&mut Self>) {
        self.canceled.store(true, Ordering::SeqCst);
    }

    fn poll_close(mut self: Pin<&mut Self>, context: &mut Context<'_>) -> Poll<io::Result<()>> {
        Pin::new(&mut self.finished)
            .poll(context)
            .map(|result| result.map_err(io::Error::other))
    }
}

#[test]
fn source_closure_and_cancellation_keep_their_classification_and_close_the_cursor() {
    runtime().block_on(async {
        for failure in [BodyError::closed(), BodyError::canceled()] {
            let expected = failure.failure();
            let mut source = Source::new(vec![42]);
            source.terminal_failure = Some(failure);
            let observations = Arc::clone(&source.observations);
            let mut body = ReadBody::new(source);

            assert_eq!(body.read_chunk(1).await.unwrap_err().failure(), expected);
            assert!(observations.canceled.load(Ordering::SeqCst));

            assert_eq!(
                body.read_chunk(1).await.unwrap_err().failure(),
                BodyFailure::Closed
            );

            body.close().await.unwrap();
            assert!(observations.dropped.load(Ordering::SeqCst));
        }
    });
}

#[test]
#[cfg(unix)]
fn dropping_body_or_close_wait_does_not_release_a_running_native_reads_guard()
-> Result<(), Box<dyn std::error::Error>> {
    use std::io::Read as _;
    use std::sync::mpsc;
    use std::time::Duration;

    use crate::test_directory::TestDirectory;
    use crate::{GuardManager, GuardMode, GuardTarget};

    let directory = TestDirectory::new()?;
    let manager = GuardManager::open(directory.path())?;
    let path = directory.path().join("read-payload");
    std::fs::write(&path, b"owned native read")?;

    for drop_body in [false, true] {
        let guard = manager
            .try_acquire(GuardTarget::Store, GuardMode::Shared)?
            .unwrap();

        let mut file = std::fs::File::open(&path)?;
        let canceled = Arc::new(AtomicBool::new(false));
        let (started, observed_start) = oneshot::channel();
        let (finished, observed_finish) = oneshot::channel();
        let (release, released) = mpsc::sync_channel(1);

        std::thread::scope(|scope| -> Result<(), Box<dyn std::error::Error>> {
            let native = scope.spawn(move || -> io::Result<[u8; 17]> {
                let read_guard = guard;

                started
                    .send(())
                    .map_err(|()| io::Error::other("read start receiver lost"))?;

                released
                    .recv_timeout(Duration::from_secs(10))
                    .map_err(io::Error::other)?;

                let mut bytes = [0; 17];
                file.read_exact(&mut bytes)?;
                drop(file);
                drop(read_guard);
                let _delivery = finished.send(());
                Ok(bytes)
            });

            runtime().block_on(async {
                tokio::time::timeout(Duration::from_secs(10), observed_start).await??;

                let mut body = ReadBody::new(NativeSource {
                    canceled: Arc::clone(&canceled),
                    finished: observed_finish,
                });

                let mut reading = Box::pin(body.read_chunk(1));

                assert!(
                    poll_fn(|context| Poll::Ready(reading.as_mut().poll(context)))
                        .await
                        .is_pending()
                );

                drop(reading);
                assert!(canceled.load(Ordering::SeqCst));

                let mut closing = Box::pin(body.close());

                assert!(
                    poll_fn(|context| Poll::Ready(closing.as_mut().poll(context)))
                        .await
                        .is_pending()
                );

                drop(closing);

                assert!(
                    manager
                        .try_acquire(GuardTarget::Store, GuardMode::Exclusive)?
                        .is_none()
                );

                if drop_body {
                    drop(body);

                    assert!(
                        manager
                            .try_acquire(GuardTarget::Store, GuardMode::Exclusive)?
                            .is_none()
                    );

                    release.send(())?;
                } else {
                    release.send(())?;
                    tokio::time::timeout(Duration::from_secs(10), body.close()).await??;
                }

                Ok::<_, Box<dyn std::error::Error>>(())
            })?;

            assert_eq!(&native.join().unwrap()?, b"owned native read");
            Ok(())
        })?;

        assert!(
            manager
                .try_acquire(GuardTarget::Store, GuardMode::Exclusive)?
                .is_some()
        );
    }

    directory.finish()?;
    Ok(())
}
