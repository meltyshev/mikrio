//! Owned upload/body transfers through the embedding boundary.

#[path = "../../mikrio-core/tests/support/directory.rs"]
mod test_directory;

use std::panic::{AssertUnwindSafe, catch_unwind, resume_unwind};

use mikrio::{CreateBucketInput, GetObjectInput, OpenOptions, PutObjectInput, Store};
use mikrio_embed::body::{Body, BodyError};
use mikrio_embed::chunks::ChunkError;
use mikrio_embed::upload::Upload;
use test_directory::TestDirectory;
use tokio::io::AsyncReadExt;

#[expect(clippy::unwrap_used, reason = "controlled test runtime setup")]
fn runtime() -> tokio::runtime::Runtime {
    tokio::runtime::Builder::new_current_thread()
        .enable_time()
        .build()
        .unwrap()
}

#[test]
fn host_upload_has_explicit_eof_and_refuses_a_late_foreign_callback() {
    runtime().block_on(async {
        let (mut upload, mut reader) = Upload::channel();

        let read = tokio::spawn(async move {
            let mut bytes = Vec::new();
            reader.read_to_end(&mut bytes).await.unwrap();
            bytes
        });

        upload.next().await.unwrap().write(&[]).unwrap();
        upload.next().await.unwrap().write(b"hello").unwrap();
        upload.next().await.unwrap().write(b" world").unwrap();
        upload.next().await.unwrap().finish();
        assert_eq!(read.await.unwrap(), b"hello world");
        assert!(upload.next().await.is_none());

        let (mut upload, mut reader) = Upload::channel();
        let read = tokio::spawn(async move { reader.read_u8().await });
        let late = upload.next().await.unwrap();
        read.abort();
        assert!(read.await.unwrap_err().is_cancelled());
        assert_eq!(late.write(b"late"), Err(ChunkError::Closed));
    });
}

#[test]
fn abandoned_host_request_is_source_failure_not_successful_short_eof() {
    runtime().block_on(async {
        let (mut upload, mut reader) = Upload::channel();

        let read = tokio::spawn(async move {
            let mut bytes = Vec::new();
            reader.read_to_end(&mut bytes).await
        });

        drop(upload.next().await.unwrap());
        let error = read.await.unwrap().unwrap_err();
        assert_eq!(error.kind(), std::io::ErrorKind::Other);
        assert_eq!(error.to_string(), "host body request abandoned");
    });
}

#[test]
fn host_body_claims_span_collections_and_abandonment_closes_the_cursor() {
    let directory = TestDirectory::new().unwrap();
    let executor = runtime();

    let store = executor
        .block_on(Store::open(OpenOptions::new(
            directory.path().join("store"),
        )))
        .unwrap();

    // Retain the store outside the scenario so cleanup is awaited even when
    // an assertion unwinds through the scenario's bodies and read claims.
    let outcome = catch_unwind(AssertUnwindSafe(|| {
        executor.block_on(async {
            store
                .create_bucket(CreateBucketInput::builder("bucket").build().unwrap())
                .await
                .unwrap();

            store
                .put_object(
                    PutObjectInput::builder("bucket", "key", b"abcdef".to_vec())
                        .build()
                        .unwrap(),
                )
                .await
                .unwrap();

            let output = store
                .get_object(GetObjectInput::builder("bucket", "key").build().unwrap())
                .await
                .unwrap();

            let body = Body::new(output.into_body(), 6);
            let mut reading = body.start_read().unwrap();
            assert!(matches!(body.start_read(), Err(BodyError::InUse)));
            assert_eq!(reading.next(2).await.unwrap().unwrap(), b"ab");
            reading.finish();

            let (mut collection, length) = body.collection(4).unwrap();
            assert_eq!(length, 4);
            assert_eq!(collection.next(3).await.unwrap().unwrap(), b"cde");
            assert!(matches!(body.start_read(), Err(BodyError::InUse)));

            drop(collection);
            assert!(matches!(body.start_read(), Err(BodyError::Closed)));
            body.close().await.unwrap();
            body.close().await.unwrap();

            let output = store
                .get_object(GetObjectInput::builder("bucket", "key").build().unwrap())
                .await
                .unwrap();

            let body = Body::new(output.into_body(), 6);
            assert!(matches!(body.collection(5), Err(BodyError::TooLarge)));
            body.close().await.unwrap();

            let output = store
                .get_object(GetObjectInput::builder("bucket", "key").build().unwrap())
                .await
                .unwrap();

            let body = Body::new(output.into_body(), 6);
            let (mut collection, _) = body.collection(6).unwrap();
            assert_eq!(collection.next(99).await.unwrap().unwrap(), b"abcdef");
            assert!(collection.next(1).await.unwrap().is_none());
            drop(collection);

            let (mut eof, length) = body.collection(0).unwrap();
            assert_eq!(length, 0);
            assert!(eof.next(1).await.unwrap().is_none());
            drop(eof);

            body.close().await.unwrap();

            let output = store
                .get_object(GetObjectInput::builder("bucket", "key").build().unwrap())
                .await
                .unwrap();

            let body = Body::new(output.into_body(), 6);
            store.close().await.unwrap();
            body
        })
    }));

    let cleanup = executor.block_on(store.close());
    drop(store);
    drop(executor);

    let orphaned = match outcome {
        Ok(body) => {
            cleanup.unwrap();
            body
        }
        Err(panic) => {
            if let Err(error) = cleanup {
                report_cleanup_failure(&error);
            }

            resume_unwind(panic);
        }
    };

    // A host cursor can outlive its executor; lost replies must fail, not end a
    // response successfully or report an upload producer failure.
    runtime().block_on(async {
        let mut reading = orphaned.start_read().unwrap();
        assert_eq!(reading.next(1).await.unwrap_err().code(), "IoError");
        drop(reading);
        assert_eq!(orphaned.close().await.unwrap_err().code(), "IoError");
    });

    directory.finish().unwrap();
}

#[expect(
    clippy::print_stderr,
    reason = "test cleanup failure must remain visible beside the original panic"
)]
fn report_cleanup_failure(error: &mikrio::StorageError) {
    eprintln!("test store cleanup failed: {error}");
}
