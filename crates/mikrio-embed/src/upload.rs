//! Demand-driven transfer from a foreign producer into the facade's reader.
//! No interpreter object enters the reader and no task is created per chunk.

use std::future::Future;
use std::io;
use std::pin::Pin;
use std::task::{Context, Poll};

use bytes::{Buf, Bytes};
use mikrio::BodySource;
use tokio::io::{AsyncRead, ReadBuf};
use tokio::sync::{mpsc, oneshot};

use crate::chunks::{CHUNK_BYTES, ChunkBudget, ChunkError, ChunkReservation};

type Reply = oneshot::Sender<io::Result<Option<Bytes>>>;

/// Host-side demand queue. A source callback runs only after `next` returns.
pub struct Upload {
    requests: mpsc::Receiver<Reply>,
    budget: ChunkBudget,
}

impl Upload {
    /// Create a single-consumer transfer without pulling or allocating payloads.
    #[must_use]
    pub fn channel() -> (Self, BodySource) {
        let (requests, receiver) = mpsc::channel(1);
        let budget = ChunkBudget::default();

        let reader = Reader {
            requests,
            budget: budget.clone(),
            response: None,
            bytes: Bytes::new(),
            has_ended: false,
        };

        (
            Self {
                requests: receiver,
                budget,
            },
            BodySource::from_reader(reader),
        )
    }

    /// Wait for native demand and reserve capacity before copying host bytes.
    /// `None` means native consumption ended, including early validation failure.
    /// Abandoning this wait after accepting demand fails that request. Retain the
    /// future across timed waits, or drop the transfer to cancel it.
    pub async fn next(&mut self) -> Option<Request> {
        let reply = self.requests.recv().await?;
        let chunk = self.budget.reserve().await.ok()?;
        Some(Request { reply, chunk })
    }
}

/// One requested bounded chunk. Dropping it fails the source rather than EOF.
pub struct Request {
    reply: Reply,
    chunk: ChunkReservation,
}

impl Request {
    /// Native consumption has ended; do not invoke another foreign callback.
    #[must_use]
    pub fn is_canceled(&self) -> bool {
        self.reply.is_closed()
    }

    /// Copy one slice on the host thread. Empty slices are not EOF.
    ///
    /// # Errors
    /// Rejects slices above 64 KiB and callbacks returning after cancellation.
    pub fn write(self, bytes: &[u8]) -> Result<(), ChunkError> {
        if self.reply.is_closed() {
            return Err(ChunkError::Closed);
        }

        let bytes = self.chunk.copy(bytes)?;

        self.reply
            .send(Ok(Some(bytes)))
            .map_err(|_| ChunkError::Closed)
    }

    /// End a successfully exhausted source; only this message represents EOF.
    pub fn finish(self) {
        let _delivery = self.reply.send(Ok(None));
    }

    /// Fail a producer without transferring a foreign exception to native work.
    /// The adapter retains its original host exception for result conversion.
    pub fn fail(self) {
        let _delivery = self
            .reply
            .send(Err(io::Error::other("host body producer failed")));
    }
}

struct Reader {
    requests: mpsc::Sender<Reply>,
    budget: ChunkBudget,
    response: Option<oneshot::Receiver<io::Result<Option<Bytes>>>>,
    bytes: Bytes,
    has_ended: bool,
}

impl AsyncRead for Reader {
    fn poll_read(
        mut self: Pin<&mut Self>,
        context: &mut Context<'_>,
        output: &mut ReadBuf<'_>,
    ) -> Poll<io::Result<()>> {
        if output.remaining() == 0 || self.has_ended {
            return Poll::Ready(Ok(()));
        }

        if !self.bytes.is_empty() {
            let byte_count = output.remaining().min(self.bytes.len()).min(CHUNK_BYTES);
            output.put_slice(&self.bytes[..byte_count]);
            self.bytes.advance(byte_count);
            return Poll::Ready(Ok(()));
        }

        if self.response.is_none() {
            let (reply, response) = oneshot::channel();

            self.requests
                .try_send(reply)
                .map_err(|_| io::Error::other("host body producer unavailable"))?;

            self.response = Some(response);
        }

        let Some(response) = self.response.as_mut() else {
            return Poll::Ready(Err(io::Error::other("host body response missing")));
        };

        let result = match Pin::new(response).poll(context) {
            Poll::Pending => return Poll::Pending,
            Poll::Ready(result) => result,
        };

        self.response = None;

        match result.map_err(|_| io::Error::other("host body request abandoned"))?? {
            Some(bytes) => self.bytes = bytes,
            None => self.has_ended = true,
        }

        // Even an endless empty-chunk producer yields between demands.
        context.waker().wake_by_ref();
        Poll::Pending
    }
}

impl Drop for Reader {
    fn drop(&mut self) {
        self.budget.close();
    }
}
