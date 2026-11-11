//! Bounded single-cursor reads with terminal cancellation and joinable cleanup.

use std::fmt;
use std::future::poll_fn;
use std::io;
use std::pin::Pin;
use std::task::{Context, Poll};

use tokio::io::ReadBuf;

const CHUNK_BYTES: usize = 64 * 1024;

/// Internal source boundary for a protected local read or a remote response.
/// Actual pending I/O must own its resources even if this source is dropped.
pub trait ReadSource: Send + 'static {
    /// Fill a nonempty buffer or report EOF. Preserve connection/operation
    /// cancellation as a typed failure rather than disguising it as I/O.
    fn poll_read(
        self: Pin<&mut Self>,
        context: &mut Context<'_>,
        buffer: &mut ReadBuf<'_>,
    ) -> Poll<Result<(), BodyError>>;

    /// Request terminal shutdown without waiting or releasing pending I/O's owners.
    /// Repeated requests are harmless, including from a drop fallback.
    fn cancel(self: Pin<&mut Self>);

    /// Join resource release after cancellation or EOF. Dropping this wait must
    /// preserve cleanup; an error retains ownership for a subsequent attempt.
    fn poll_close(self: Pin<&mut Self>, context: &mut Context<'_>) -> Poll<io::Result<()>>;
}

/// Stable read failure classification, independent of source error text.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BodyFailure {
    InvalidArgument,
    Closed,
    Canceled,
    TooLarge,
    Io,
}

/// A typed read failure, retaining an original I/O or cleanup cause.
#[derive(Debug)]
pub struct BodyError {
    failure: BodyFailure,
    cause: Option<io::Error>,
}

impl BodyError {
    /// The source's owning connection or response has closed.
    #[must_use]
    pub const fn closed() -> Self {
        Self::new(BodyFailure::Closed)
    }

    /// The source's owning operation has been canceled.
    #[must_use]
    pub const fn canceled() -> Self {
        Self::new(BodyFailure::Canceled)
    }

    #[must_use]
    pub const fn failure(&self) -> BodyFailure {
        self.failure
    }

    const fn new(failure: BodyFailure) -> Self {
        Self {
            failure,
            cause: None,
        }
    }
}

impl From<io::Error> for BodyError {
    fn from(cause: io::Error) -> Self {
        Self {
            failure: BodyFailure::Io,
            cause: Some(cause),
        }
    }
}

impl fmt::Display for BodyError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(match self.failure {
            BodyFailure::InvalidArgument => "chunk size must be positive",
            BodyFailure::Closed => "object body is closed",
            BodyFailure::Canceled => "object body read was canceled",
            BodyFailure::TooLarge => "object body exceeds the collection limit",
            BodyFailure::Io => "object body I/O failed",
        })
    }
}

impl std::error::Error for BodyError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match &self.cause {
            Some(cause) => Some(cause),
            None => None,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Phase {
    Open,
    Eof,
    Closing,
    Closed,
}

/// One cursor. Each read retains cleanup state when its future is abandoned.
pub struct ReadBody {
    source: Option<Pin<Box<dyn ReadSource>>>,
    phase: Phase,
}

impl ReadBody {
    /// Transfer a source whose native/transport work follows [`ReadSource`]'s
    /// ownership contract. Construction starts no task and performs no read.
    #[must_use]
    pub fn new(source: impl ReadSource) -> Self {
        Self {
            source: Some(Box::pin(source)),
            phase: Phase::Open,
        }
    }

    /// Read a nonempty bounded chunk, or report EOF after resource release.
    ///
    /// # Errors
    /// Rejects zero bounds and closed bodies; I/O failure closes the cursor.
    /// Dropping a pending read also closes it, without claiming I/O has stopped.
    pub async fn read_chunk(&mut self, max_bytes: usize) -> Result<Option<Vec<u8>>, BodyError> {
        if max_bytes == 0 {
            return Err(BodyError::new(BodyFailure::InvalidArgument));
        }

        let mut reading = Reading::new(self)?;
        let result = reading.body.read_owned(max_bytes.min(CHUNK_BYTES)).await;
        reading.completed = result.is_ok();
        result
    }

    /// Collect the remaining bytes, checking the bound before extending output.
    ///
    /// # Errors
    /// Zero is valid for an empty remainder. Overflow closes the cursor and
    /// reports `TooLarge`, with a cleanup cause if resource release also fails.
    pub async fn read_all(&mut self, max_bytes: usize) -> Result<Vec<u8>, BodyError> {
        let mut reading = Reading::new(self)?;
        let mut output = Vec::new();

        loop {
            let remaining = max_bytes - output.len();
            let limit = remaining.saturating_add(1).min(CHUNK_BYTES);

            let Some(chunk) = reading.body.read_owned(limit).await? else {
                reading.completed = true;
                return Ok(output);
            };

            if chunk.len() > remaining {
                let cause = reading
                    .body
                    .close()
                    .await
                    .err()
                    .and_then(|error| error.cause);

                reading.completed = true;

                return Err(BodyError {
                    failure: BodyFailure::TooLarge,
                    cause,
                });
            }

            // The length check above bounds this sum. Grow geometrically to
            // avoid reallocating the accumulated body for every chunk, while
            // keeping reserved output capacity within the caller's limit.
            let required = output.len() + chunk.len();

            if required > output.capacity() {
                let capacity = output
                    .capacity()
                    .saturating_mul(2)
                    .max(required)
                    .min(max_bytes);

                output
                    .try_reserve_exact(capacity - output.len())
                    .map_err(io::Error::other)?;
            }

            output.extend_from_slice(&chunk);
            tokio::task::yield_now().await;
        }
    }

    /// Stop the cursor and join native/transport resource release. This is
    /// idempotent; a canceled or failed close may be joined again.
    ///
    /// # Errors
    /// Retains the original cleanup I/O cause and the source on failure.
    pub async fn close(&mut self) -> Result<(), BodyError> {
        self.start_close();

        if let Some(source) = &mut self.source {
            poll_fn(|context| source.as_mut().poll_close(context)).await?;
        }

        self.source = None;
        self.phase = Phase::Closed;
        Ok(())
    }

    async fn read_owned(&mut self, max_bytes: usize) -> Result<Option<Vec<u8>>, BodyError> {
        if self.phase == Phase::Eof {
            return Ok(None);
        }

        let source = self
            .source
            .as_mut()
            .ok_or_else(|| BodyError::new(BodyFailure::Closed))?;

        let mut bytes = vec![0; max_bytes];
        let mut buffer = ReadBuf::new(&mut bytes);
        poll_fn(|context| source.as_mut().poll_read(context, &mut buffer)).await?;
        let length = buffer.filled().len();

        if length == 0 {
            self.close().await?;
            self.phase = Phase::Eof;
            return Ok(None);
        }

        bytes.truncate(length);
        Ok(Some(bytes))
    }

    fn start_close(&mut self) {
        if matches!(self.phase, Phase::Closing | Phase::Closed) {
            return;
        }

        self.phase = Phase::Closing;

        if let Some(source) = &mut self.source {
            source.as_mut().cancel();
        }
    }
}

impl Drop for ReadBody {
    fn drop(&mut self) {
        self.start_close();
    }
}

impl fmt::Debug for ReadBody {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("ReadBody")
            .field("phase", &self.phase)
            .finish_non_exhaustive()
    }
}

struct Reading<'body> {
    body: &'body mut ReadBody,
    completed: bool,
}

impl<'body> Reading<'body> {
    fn new(body: &'body mut ReadBody) -> Result<Self, BodyError> {
        if matches!(body.phase, Phase::Closing | Phase::Closed) {
            return Err(BodyError::new(BodyFailure::Closed));
        }

        Ok(Self {
            body,
            completed: false,
        })
    }
}

impl Drop for Reading<'_> {
    fn drop(&mut self) {
        if !self.completed {
            self.body.start_close();
        }
    }
}

#[cfg(test)]
mod tests;
