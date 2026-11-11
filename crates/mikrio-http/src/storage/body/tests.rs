#![allow(clippy::unwrap_used, reason = "owned bounded stream fixtures")]

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

use futures_util::StreamExt;
use mikrio_core::body::{BodyError, ReadSource};
use tokio::io::AsyncReadExt;

use super::*;
use crate::tests::report_case;

#[test]
fn underlying_upload_timeouts_retain_their_code_through_each_decoder() {
    use s3s::FileStreamError;
    use s3s::stream::aws_chunked_stream::AwsChunkedStreamError;
    use s3s::stream::upload_stream::UploadStreamError;

    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap();

    runtime.block_on(async {
        let timeout = || io::Error::new(io::ErrorKind::TimedOut, "private transport details");

        let sources = [
            Upload::new(StreamingBlob::wrap(stream::iter([Err::<Bytes, _>(
                timeout(),
            )]))),
            Upload::new(StreamingBlob::wrap(stream::iter([Err::<Bytes, _>(
                UploadStreamError::Underlying(Box::new(timeout())),
            )]))),
            Upload::new(StreamingBlob::wrap(stream::iter([Err::<Bytes, _>(
                AwsChunkedStreamError::Underlying(Box::new(timeout())),
            )]))),
            Upload::post(StreamingBlob::wrap(stream::iter([Err::<Bytes, _>(
                FileStreamError::Underlying(Box::new(timeout())),
            )]))),
        ];

        for (index, mut upload) in sources.into_iter().enumerate() {
            report_case(format_args!("source_index={index}"));

            let error = upload.read_u8().await.unwrap_err();

            let failure = error
                .get_ref()
                .unwrap()
                .downcast_ref::<super::super::error::UploadFailure>()
                .unwrap();

            assert_eq!(
                failure.0,
                s3s::S3ErrorCode::RequestTimeout,
                "source {index}"
            );

            assert!(!error.to_string().contains("private transport details"));
        }
    });
}

#[test]
fn stalled_and_empty_decoded_streams_cannot_reset_the_upload_deadline() {
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap();

    runtime.block_on(async {
        for (case, source) in [
            (
                "pending",
                StreamingBlob::wrap(stream::pending::<io::Result<Bytes>>()),
            ),
            (
                "empty",
                StreamingBlob::wrap(stream::repeat_with(|| Ok::<_, io::Error>(Bytes::new()))),
            ),
        ] {
            report_case(format_args!("source={case}"));

            let mut upload = Upload::new(source);
            upload.stalled = Stall::new(Duration::from_millis(40));
            let error = upload.read_u8().await.unwrap_err();

            let failure = error
                .get_ref()
                .unwrap()
                .downcast_ref::<super::super::error::UploadFailure>()
                .unwrap();

            assert_eq!(failure.0, s3s::S3ErrorCode::RequestTimeout);
        }
    });
}

#[test]
fn decoded_progress_and_engine_backpressure_have_separate_clocks() {
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap();

    runtime.block_on(async {
        let source = stream::unfold(0, |count| async move {
            if count == 6 {
                return None;
            }

            tokio::time::sleep(Duration::from_millis(30)).await;
            Some((Ok::<_, io::Error>(Bytes::from_static(b"x")), count + 1))
        });

        let mut upload = Upload::new(StreamingBlob::wrap(source));
        upload.stalled = Stall::new(Duration::from_millis(100));
        let start = tokio::time::Instant::now();
        assert_eq!(upload.read_u8().await.unwrap(), b'x');
        // The engine does not ask for bytes while writing/syncing its last chunk.
        tokio::time::sleep(Duration::from_millis(200)).await;
        let mut rest = Vec::new();
        upload.read_to_end(&mut rest).await.unwrap();
        assert_eq!(rest, b"xxxxx");
        assert!(start.elapsed() > Duration::from_millis(300));
    });
}

struct FailingSource {
    sent: bool,
    closed: Arc<AtomicBool>,
}

impl ReadSource for FailingSource {
    fn poll_read(
        mut self: Pin<&mut Self>,
        _: &mut Context<'_>,
        buffer: &mut ReadBuf<'_>,
    ) -> Poll<Result<(), BodyError>> {
        if self.sent {
            return Poll::Ready(Err(io::Error::other("synthetic read failure").into()));
        }

        self.sent = true;
        buffer.put_slice(b"first");
        Poll::Ready(Ok(()))
    }

    fn cancel(self: Pin<&mut Self>) {
        self.closed.store(true, Ordering::SeqCst);
    }

    fn poll_close(self: Pin<&mut Self>, _: &mut Context<'_>) -> Poll<io::Result<()>> {
        Poll::Ready(Ok(()))
    }
}

#[test]
fn download_failure_is_a_stream_error_and_closes_its_cursor() {
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap();

    runtime.block_on(async {
        let closed = Arc::new(AtomicBool::new(false));

        let body = ReadBody::new(FailingSource {
            sent: false,
            closed: Arc::clone(&closed),
        });

        let mut stream = download(body);
        assert_eq!(stream.next().await.unwrap().unwrap(), b"first".as_slice());

        let error = stream.next().await.unwrap().unwrap_err();

        assert_eq!(
            error.downcast_ref::<io::Error>().unwrap().kind(),
            io::ErrorKind::Other
        );

        assert!(closed.load(Ordering::SeqCst));
    });
}
