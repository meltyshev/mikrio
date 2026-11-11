//! Bounded decoded uploads and one owned guarded download cursor.

use std::io;
use std::pin::Pin;
use std::sync::Mutex;
use std::task::{Context, Poll};

use futures_util::{Stream, stream};
use hyper::body::Bytes;
use mikrio_core::body::ReadBody;
use s3s::dto::StreamingBlob;
use tokio::io::{AsyncRead, ReadBuf};

use super::error::upload;
use crate::progress::{STALL_LIMIT, Stall};

#[cfg(test)]
mod tests;

const CHUNK_BYTES: usize = 64 * 1024;

pub(super) struct Upload {
    source: StreamingBlob,
    chunk: Bytes,
    stalled: Stall,
    is_post: bool,
}

impl Upload {
    pub(super) fn new(source: StreamingBlob) -> Self {
        Self {
            source,
            chunk: Bytes::new(),
            stalled: Stall::new(STALL_LIMIT),
            is_post: false,
        }
    }

    pub(super) fn post(source: StreamingBlob) -> Self {
        Self {
            is_post: true,
            ..Self::new(source)
        }
    }
}

impl AsyncRead for Upload {
    fn poll_read(
        self: Pin<&mut Self>,
        context: &mut Context<'_>,
        buffer: &mut ReadBuf<'_>,
    ) -> Poll<io::Result<()>> {
        let this = self.get_mut();

        if buffer.remaining() == 0 {
            return Poll::Ready(Ok(()));
        }

        if let Poll::Ready(Err(_)) = this.stalled.pending(context) {
            return Poll::Ready(Err(upload(&s3s::s3_error!(RequestTimeout))));
        }

        // Empty decoded chunks are neither EOF nor meaningful progress. Bound
        // work per poll so an empty-chunk producer cannot monopolize a worker.
        for _ in 0..16 {
            if !this.chunk.is_empty() {
                let length = buffer.remaining().min(this.chunk.len()).min(CHUNK_BYTES);
                buffer.put_slice(&this.chunk.split_to(length));
                this.stalled.clear();
                return Poll::Ready(Ok(()));
            }

            match Pin::new(&mut this.source).poll_next(context) {
                Poll::Pending => return Poll::Pending,
                Poll::Ready(None) => {
                    this.stalled.clear();
                    return Poll::Ready(Ok(()));
                }
                Poll::Ready(Some(Err(error))) => {
                    let error = stream_error(error.as_ref()).unwrap_or_else(|| {
                        if this.is_post {
                            s3s::s3_error!(MalformedPOSTRequest)
                        } else {
                            s3s::s3_error!(BadDigest)
                        }
                    });

                    return Poll::Ready(Err(upload(&error)));
                }
                Poll::Ready(Some(Ok(bytes))) => this.chunk = bytes,
            }
        }

        context.waker().wake_by_ref();
        Poll::Pending
    }
}

type DownloadStream = Pin<Box<dyn Stream<Item = io::Result<Bytes>> + Send>>;

struct Download(Mutex<DownloadStream>);

impl Stream for Download {
    type Item = io::Result<Bytes>;

    fn poll_next(mut self: Pin<&mut Self>, context: &mut Context<'_>) -> Poll<Option<Self::Item>> {
        // s3s requires Sync, though poll_next owns exclusive access. The mutex
        // only supplies that boundary; it never spans a native read or await.
        match self.0.get_mut() {
            Ok(stream) => stream.as_mut().poll_next(context),
            Err(_) => Poll::Ready(Some(Err(io::Error::other("HTTP body owner unavailable")))),
        }
    }
}

pub(super) fn download(body: ReadBody) -> StreamingBlob {
    let stream = stream::try_unfold(body, |mut body| async move {
        let bytes = body
            .read_chunk(CHUNK_BYTES)
            .await
            .map_err(io::Error::other)?;

        Ok(bytes.map(|bytes| (Bytes::from(bytes), body)))
    });

    StreamingBlob::wrap(Download(Mutex::new(Box::pin(stream))))
}

fn stream_error(error: &(dyn std::error::Error + 'static)) -> Option<s3s::S3Error> {
    use s3s::stream::aws_chunked_stream::AwsChunkedStreamError;
    use s3s::stream::upload_stream::UploadStreamError;
    use s3s::{FileStreamError, s3_error};

    let mut source = Some(error);

    while let Some(cause) = source {
        if let Some(error) = cause.downcast_ref::<UploadStreamError>()
            && !matches!(error, UploadStreamError::Underlying(_))
        {
            return Some(s3s::S3Error::new(error.to_s3_error_code()));
        }

        if let Some(error) = cause.downcast_ref::<AwsChunkedStreamError>()
            && !matches!(error, AwsChunkedStreamError::Underlying(_))
        {
            return Some(s3s::S3Error::new(error.to_s3_error_code()));
        }

        if let Some(error) = cause.downcast_ref::<FileStreamError>() {
            match error {
                FileStreamError::TooLarge => return Some(s3_error!(EntityTooLarge)),
                FileStreamError::TooSmall => return Some(s3_error!(EntityTooSmall)),
                FileStreamError::Incomplete | FileStreamError::LengthMismatch { .. } => {
                    return Some(s3_error!(IncompleteBody));
                }
                FileStreamError::InvalidTrailer => {
                    return Some(s3_error!(MalformedPOSTRequest));
                }
                FileStreamError::Underlying(_) => {}
            }
        }

        if let Some(error) = cause.downcast_ref::<io::Error>() {
            if error.kind() == io::ErrorKind::TimedOut {
                return Some(s3_error!(RequestTimeout));
            }

            if error.kind() == io::ErrorKind::UnexpectedEof {
                return Some(s3_error!(IncompleteBody));
            }
        }

        source = cause.source();
    }

    None
}
