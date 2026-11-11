use std::cell::Cell;
use std::io;
use std::marker::PhantomPinned;
use std::pin::Pin;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::task::{Context, Poll};

use mikrio::BodySource;
use tokio::io::{AsyncRead, AsyncReadExt as _, ReadBuf};

struct PinnedSource {
    position: Cell<usize>,
    poll_count: Arc<AtomicUsize>,
    is_dropped: Arc<AtomicBool>,
    _pinned: PhantomPinned,
}

impl AsyncRead for PinnedSource {
    fn poll_read(
        self: Pin<&mut Self>,
        _context: &mut Context<'_>,
        buffer: &mut ReadBuf<'_>,
    ) -> Poll<io::Result<()>> {
        self.poll_count.fetch_add(1, Ordering::Relaxed);
        let remaining = &b"owned bytes"[self.position.get()..];
        let count = buffer.remaining().min(remaining.len());
        buffer.put_slice(&remaining[..count]);
        self.position.set(self.position.get() + count);
        Poll::Ready(Ok(()))
    }
}

impl Drop for PinnedSource {
    fn drop(&mut self) {
        self.is_dropped.store(true, Ordering::Relaxed);
    }
}

#[test]
fn a_pinned_non_sync_reader_is_lazy_bounded_and_owned_until_drop() -> io::Result<()> {
    let poll_count = Arc::new(AtomicUsize::new(0));
    let is_dropped = Arc::new(AtomicBool::new(false));

    let mut source = BodySource::from_reader(PinnedSource {
        position: Cell::new(0),
        poll_count: Arc::clone(&poll_count),
        is_dropped: Arc::clone(&is_dropped),
        _pinned: PhantomPinned,
    });

    assert_eq!(poll_count.load(Ordering::Relaxed), 0);
    let runtime = tokio::runtime::Builder::new_current_thread().build()?;

    runtime.block_on(async {
        let mut buffer = [0; 3];
        assert_eq!(source.read(&mut buffer).await?, 3);
        assert_eq!(&buffer, b"own");
        assert_eq!(poll_count.load(Ordering::Relaxed), 1);
        assert_eq!(source.read(&mut buffer).await?, 3);
        assert_eq!(&buffer, b"ed ");
        assert_eq!(poll_count.load(Ordering::Relaxed), 2);
        io::Result::Ok(())
    })?;

    assert!(!is_dropped.load(Ordering::Relaxed));
    drop(source);
    assert!(is_dropped.load(Ordering::Relaxed));
    Ok(())
}

#[test]
fn owned_binary_buffers_preserve_empty_and_non_utf8_bytes() -> io::Result<()> {
    let runtime = tokio::runtime::Builder::new_current_thread().build()?;

    runtime.block_on(async {
        let mut source = BodySource::from(vec![0, 255, 128]);
        assert_eq!(format!("{source:?}"), "BodySource([REDACTED])");
        let mut output = Vec::new();
        source.read_to_end(&mut output).await?;
        assert_eq!(output, [0, 255, 128]);

        let mut empty = BodySource::from(Vec::new().into_boxed_slice());
        let mut buffer = [0; 1];
        assert_eq!(empty.read(&mut buffer).await?, 0);
        io::Result::Ok(())
    })
}

struct FailedSource(Arc<AtomicUsize>);

impl AsyncRead for FailedSource {
    fn poll_read(
        self: Pin<&mut Self>,
        _context: &mut Context<'_>,
        _buffer: &mut ReadBuf<'_>,
    ) -> Poll<io::Result<()>> {
        self.0.fetch_add(1, Ordering::Relaxed);
        Poll::Ready(Err(io::Error::from_raw_os_error(12345)))
    }
}

#[test]
fn source_io_failure_is_not_replaced_by_eof_or_a_retry() -> io::Result<()> {
    let runtime = tokio::runtime::Builder::new_current_thread().build()?;

    runtime.block_on(async {
        let poll_count = Arc::new(AtomicUsize::new(0));
        let mut source = BodySource::from_reader(FailedSource(Arc::clone(&poll_count)));
        let mut buffer = [0; 1];
        let error = source.read(&mut buffer).await.unwrap_err();
        assert_eq!(error.raw_os_error(), Some(12345));
        assert_eq!(poll_count.load(Ordering::Relaxed), 1);
    });

    Ok(())
}
