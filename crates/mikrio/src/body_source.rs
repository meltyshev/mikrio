use std::fmt;
use std::io::{self, Cursor};
use std::pin::Pin;
use std::task::{Context, Poll};

use tokio::io::{AsyncRead, ReadBuf};

/// An owned upload source, read with the consumer's buffer and backpressure.
/// Construction performs no I/O, starts no task and does not copy byte buffers.
/// Dropping the source drops its reader; it does not commit an upload.
pub struct BodySource {
    reader: Pin<Box<dyn AsyncRead + Send + 'static>>,
}

impl BodySource {
    /// Own a Tokio reader, including a reader that is not `Unpin` or `Sync`.
    /// The source must own everything needed after the call transfers it.
    /// No retry/rewind or length discovery is implicit. Read errors preserve
    /// their original I/O cause; storage operations classify producer failures.
    pub fn from_reader(reader: impl AsyncRead + Send + 'static) -> Self {
        Self {
            reader: Box::pin(reader),
        }
    }
}

impl From<Vec<u8>> for BodySource {
    /// Transfer ownership without copying the payload.
    fn from(bytes: Vec<u8>) -> Self {
        Self::from_reader(Cursor::new(bytes))
    }
}

impl From<Box<[u8]>> for BodySource {
    /// Transfer ownership without copying the payload.
    fn from(bytes: Box<[u8]>) -> Self {
        Self::from_reader(Cursor::new(bytes))
    }
}

impl AsyncRead for BodySource {
    fn poll_read(
        mut self: Pin<&mut Self>,
        context: &mut Context<'_>,
        buffer: &mut ReadBuf<'_>,
    ) -> Poll<io::Result<()>> {
        self.reader.as_mut().poll_read(context, buffer)
    }
}

impl fmt::Debug for BodySource {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("BodySource([REDACTED])")
    }
}
