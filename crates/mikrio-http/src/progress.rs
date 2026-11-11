//! Deadlines apply only while a peer owes progress, never to total transfer time.

use std::future::Future;
use std::io;
use std::pin::Pin;
use std::task::{Context, Poll};
use std::time::Duration;

use futures_util::Stream;
use hyper::body::Bytes;
use s3s::stream::{ByteStream, RemainingLength};
use tokio::io::{AsyncRead, AsyncWrite, ReadBuf};
use tokio::time::Sleep;

#[cfg(test)]
mod tests;

pub(super) const STALL_LIMIT: Duration = Duration::from_secs(30);

pub(super) struct Stall {
    timer: Option<Pin<Box<Sleep>>>,
    limit: Duration,
}

impl Stall {
    pub(super) fn new(limit: Duration) -> Self {
        Self { timer: None, limit }
    }

    pub(super) fn clear(&mut self) {
        self.timer = None;
    }

    pub(super) fn pending(&mut self, context: &mut Context<'_>) -> Poll<io::Result<()>> {
        let timer = self
            .timer
            .get_or_insert_with(|| Box::pin(tokio::time::sleep(self.limit)));

        timer
            .as_mut()
            .poll(context)
            .map(|()| Err(io::ErrorKind::TimedOut.into()))
    }
}

pub(super) struct Socket<T> {
    inner: T,
    writing: Stall,
}

impl<T> Socket<T> {
    pub(super) fn new(inner: T) -> Self {
        Self {
            inner,
            writing: Stall::new(STALL_LIMIT),
        }
    }
}

impl<T: AsyncRead + Unpin> AsyncRead for Socket<T> {
    fn poll_read(
        mut self: Pin<&mut Self>,
        context: &mut Context<'_>,
        buffer: &mut ReadBuf<'_>,
    ) -> Poll<io::Result<()>> {
        Pin::new(&mut self.inner).poll_read(context, buffer)
    }
}

impl<T: AsyncWrite + Unpin> AsyncWrite for Socket<T> {
    fn poll_write(
        mut self: Pin<&mut Self>,
        context: &mut Context<'_>,
        bytes: &[u8],
    ) -> Poll<io::Result<usize>> {
        match Pin::new(&mut self.inner).poll_write(context, bytes) {
            Poll::Ready(Ok(0)) if !bytes.is_empty() => {
                Poll::Ready(Err(io::ErrorKind::WriteZero.into()))
            }
            Poll::Ready(Ok(length)) => {
                if length != 0 {
                    self.writing.clear();
                }

                Poll::Ready(Ok(length))
            }
            Poll::Ready(Err(error)) => Poll::Ready(Err(error)),
            Poll::Pending => self
                .writing
                .pending(context)
                .map(|result| result.map(|()| 0)),
        }
    }

    fn poll_flush(mut self: Pin<&mut Self>, context: &mut Context<'_>) -> Poll<io::Result<()>> {
        match Pin::new(&mut self.inner).poll_flush(context) {
            Poll::Ready(result) => Poll::Ready(result),
            Poll::Pending => self.writing.pending(context),
        }
    }

    fn poll_shutdown(mut self: Pin<&mut Self>, context: &mut Context<'_>) -> Poll<io::Result<()>> {
        match Pin::new(&mut self.inner).poll_shutdown(context) {
            Poll::Ready(result) => {
                self.writing.clear();
                Poll::Ready(result)
            }
            Poll::Pending => self.writing.pending(context),
        }
    }
}

struct Incoming {
    body: s3s::Body,
    reading: Stall,
}

impl Stream for Incoming {
    type Item = Result<Bytes, s3s::StdError>;

    fn poll_next(mut self: Pin<&mut Self>, context: &mut Context<'_>) -> Poll<Option<Self::Item>> {
        for _ in 0..16 {
            if let Poll::Ready(Err(error)) = self.reading.pending(context) {
                return Poll::Ready(Some(Err(Box::new(error))));
            }

            match Pin::new(&mut self.body).poll_next(context) {
                Poll::Pending => return Poll::Pending,
                Poll::Ready(Some(Ok(bytes))) if bytes.is_empty() => {}
                Poll::Ready(result) => {
                    self.reading.clear();
                    return Poll::Ready(result);
                }
            }
        }

        context.waker().wake_by_ref();
        Poll::Pending
    }
}

impl ByteStream for Incoming {
    fn remaining_length(&self) -> RemainingLength {
        self.body.remaining_length()
    }
}

pub(super) fn incoming(body: hyper::body::Incoming) -> s3s::Body {
    let stream: s3s::stream::DynByteStream = Box::pin(Incoming {
        body: body.into(),
        reading: Stall::new(STALL_LIMIT),
    });

    stream.into()
}
