use mikrio_core::body::ReadBody;

use crate::StorageError;

/// An owned object response with one cursor and bounded reads.
/// EOF releases read resources and remains distinct from explicit close.
/// Drop requests fallback cleanup; use [`Self::close`] to join and report it.
#[derive(Debug)]
pub struct ObjectBody {
    pub(crate) body: ReadBody,
}

impl ObjectBody {
    /// Read at most `max_bytes`, returning nonempty bytes or `None` at EOF.
    /// Reads use an internal chunk bound even when a larger limit is requested.
    ///
    /// # Errors
    /// Returns `InvalidArgument` for zero, `BodyClosed` after explicit close or
    /// cancellation, `OperationCanceled` when the source reports cancellation,
    /// and `IoError` for a failed read. Dropping a pending read
    /// makes the cursor terminal; it cannot be resumed at an uncertain position.
    pub async fn read_chunk(&mut self, max_bytes: usize) -> Result<Option<Vec<u8>>, StorageError> {
        self.body
            .read_chunk(max_bytes)
            .await
            .map_err(StorageError::from_body)
    }

    /// Collect the remaining bytes up to the explicit limit. Zero is valid for
    /// an empty remainder; output grows only after a bounded read is checked.
    ///
    /// # Errors
    /// Returns `BodyTooLarge` and closes on overflow, `BodyClosed` for a closed
    /// cursor, `OperationCanceled` for source cancellation, or `IoError` for
    /// read/allocation failure. A canceled collection
    /// also closes the cursor, including after partial progress.
    pub async fn read_all(&mut self, max_bytes: usize) -> Result<Vec<u8>, StorageError> {
        self.body
            .read_all(max_bytes)
            .await
            .map_err(StorageError::from_body)
    }

    /// Release early and wait for actual read-resource cleanup. Repeated close
    /// succeeds after cleanup; a dropped close future can be joined again.
    ///
    /// # Errors
    /// Returns `IoError` with the native/transport cleanup cause. Ownership stays
    /// with the body so the same handle can retry a failed close.
    pub async fn close(&mut self) -> Result<(), StorageError> {
        self.body.close().await.map_err(StorageError::from_body)
    }
}

#[cfg(test)]
mod tests {
    use std::io::{self, Cursor};
    use std::pin::Pin;
    use std::task::{Context, Poll};

    use mikrio_core::body::{BodyError, ReadSource};
    use tokio::io::{AsyncRead, ReadBuf};

    use super::*;
    use crate::ErrorCode;

    struct Source(Cursor<Vec<u8>>);

    impl AsyncRead for Source {
        fn poll_read(
            mut self: Pin<&mut Self>,
            context: &mut Context<'_>,
            buffer: &mut ReadBuf<'_>,
        ) -> Poll<io::Result<()>> {
            Pin::new(&mut self.0).poll_read(context, buffer)
        }
    }

    impl ReadSource for Source {
        fn poll_read(
            self: Pin<&mut Self>,
            context: &mut Context<'_>,
            buffer: &mut ReadBuf<'_>,
        ) -> Poll<Result<(), BodyError>> {
            AsyncRead::poll_read(self, context, buffer)
                .map(|result| result.map_err(BodyError::from))
        }

        fn cancel(self: Pin<&mut Self>) {}

        fn poll_close(self: Pin<&mut Self>, _context: &mut Context<'_>) -> Poll<io::Result<()>> {
            Poll::Ready(Ok(()))
        }
    }

    struct CanceledSource;

    impl ReadSource for CanceledSource {
        fn poll_read(
            self: Pin<&mut Self>,
            _context: &mut Context<'_>,
            _buffer: &mut ReadBuf<'_>,
        ) -> Poll<Result<(), BodyError>> {
            Poll::Ready(Err(BodyError::canceled()))
        }

        fn cancel(self: Pin<&mut Self>) {}

        fn poll_close(self: Pin<&mut Self>, _context: &mut Context<'_>) -> Poll<io::Result<()>> {
            Poll::Ready(Ok(()))
        }
    }

    #[test]
    fn public_body_preserves_source_cancellation_then_reports_closed() {
        let runtime = tokio::runtime::Builder::new_current_thread()
            .build()
            .unwrap();

        runtime.block_on(async {
            let mut body = ObjectBody {
                body: ReadBody::new(CanceledSource),
            };

            assert_eq!(
                body.read_chunk(1).await.unwrap_err().code(),
                ErrorCode::OperationCanceled
            );

            assert_eq!(
                body.read_chunk(1).await.unwrap_err().code(),
                ErrorCode::BodyClosed
            );

            body.close().await.unwrap();
        });
    }

    #[test]
    fn public_body_distinguishes_limits_eof_and_explicit_close() {
        let runtime = tokio::runtime::Builder::new_current_thread()
            .build()
            .unwrap();

        runtime.block_on(async {
            let mut body = ObjectBody {
                body: ReadBody::new(Source(Cursor::new(b"payload".to_vec()))),
            };

            assert_eq!(
                body.read_chunk(0).await.unwrap_err().code(),
                ErrorCode::InvalidArgument
            );

            assert_eq!(body.read_chunk(3).await.unwrap(), Some(b"pay".to_vec()));
            assert_eq!(body.read_all(4).await.unwrap(), b"load");
            assert_eq!(body.read_chunk(1).await.unwrap(), None);
            assert_eq!(body.read_all(0).await.unwrap(), Vec::<u8>::new());
            body.close().await.unwrap();

            assert_eq!(
                body.read_chunk(1).await.unwrap_err().code(),
                ErrorCode::BodyClosed
            );

            let mut oversized = ObjectBody {
                body: ReadBody::new(Source(Cursor::new(b"too large".to_vec()))),
            };

            assert_eq!(
                oversized.read_all(2).await.unwrap_err().code(),
                ErrorCode::BodyTooLarge
            );

            assert_eq!(
                oversized.read_all(20).await.unwrap_err().code(),
                ErrorCode::BodyClosed
            );
        });
    }
}
