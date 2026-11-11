//! Private controlled-transport evidence, not durable PUT/GET coverage.

#[path = "support/streams.rs"]
mod support;

use std::future::{Future, poll_fn};
use std::task::Poll;

use mikrio_embed::chunks::CHUNK_BYTES;
use support::{Failure, Stream};

#[expect(clippy::unwrap_used, reason = "controlled test runtime setup")]
fn runtime() -> tokio::runtime::Runtime {
    tokio::runtime::Builder::new_current_thread()
        .build()
        .unwrap()
}

#[test]
fn empty_chunks_are_not_eof_and_explicit_close_is_distinct() {
    runtime().block_on(async {
        let stream = Stream::new(Some(3));
        stream.reserve().await.unwrap().write(&[]).unwrap();

        let mut read = Box::pin(stream.read(2));

        assert!(
            poll_fn(|context| Poll::Ready(read.as_mut().poll(context)))
                .await
                .is_pending()
        );

        assert!(stream.is_reading());
        assert_eq!(stream.read(1).await, Err(Failure::InUse));

        stream.reserve().await.unwrap().write(b"abc").unwrap();
        assert_eq!(read.await.unwrap(), Some(b"ab".to_vec()));

        stream.end().unwrap();
        assert_eq!(stream.read(2).await.unwrap(), Some(b"c".to_vec()));
        assert_eq!(stream.read(1).await.unwrap(), None);
        assert_eq!(stream.read(1).await.unwrap(), None);
        assert_eq!(stream.read_all(0).await.unwrap(), b"");

        stream.close().await.unwrap();
        stream.close().await.unwrap();
        assert_eq!(stream.read(1).await, Err(Failure::Closed));
        assert_eq!(stream.allocations().0, 0);
    });
}

#[test]
fn failed_producer_or_length_never_becomes_successful_eof() {
    runtime().block_on(async {
        let short = Stream::new(Some(2));
        short.reserve().await.unwrap().write(b"a").unwrap();
        assert_eq!(short.end(), Err(Failure::LengthMismatch));
        assert_eq!(short.read(1).await, Err(Failure::LengthMismatch));
        assert_eq!(short.read(1).await, Err(Failure::Closed));

        let long = Stream::new(Some(1));

        assert_eq!(
            long.reserve().await.unwrap().write(b"ab"),
            Err(Failure::LengthMismatch)
        );

        assert_eq!(long.end(), Err(Failure::Closed));
        assert_eq!(long.read(1).await, Err(Failure::LengthMismatch));

        let failed = Stream::new(None);
        failed.reserve().await.unwrap().write(b"partial").unwrap();
        failed.fail(Failure::Producer).unwrap();
        assert_eq!(failed.read(1).await, Err(Failure::Producer));
        assert_eq!(failed.end(), Err(Failure::Closed));
        assert_eq!(failed.allocations().0, 0);
        assert_eq!(Failure::Producer.code(), "BodyReadFailed");
    });
}

#[test]
fn read_all_limit_closes_without_collecting_the_remainder() {
    runtime().block_on(async {
        let stream = Stream::new(None);

        stream
            .reserve()
            .await
            .unwrap()
            .write(&vec![1; CHUNK_BYTES])
            .unwrap();

        stream
            .reserve()
            .await
            .unwrap()
            .write(&vec![2; CHUNK_BYTES])
            .unwrap();

        stream.end().unwrap();
        assert_eq!(stream.read_all(17).await, Err(Failure::TooLarge));
        assert_eq!(stream.read(1).await, Err(Failure::Closed));
        assert_eq!(stream.allocations(), (0, 2 * CHUNK_BYTES));
    });
}

#[test]
fn close_settles_pending_read_and_rejects_a_late_foreign_reservation() {
    runtime().block_on(async {
        let stream = Stream::new(None);
        let late = stream.reserve().await.unwrap();
        let mut read = Box::pin(stream.read(1));

        assert!(
            poll_fn(|context| Poll::Ready(read.as_mut().poll(context)))
                .await
                .is_pending()
        );

        let mut close = Box::pin(stream.close());

        assert!(
            poll_fn(|context| Poll::Ready(close.as_mut().poll(context)))
                .await
                .is_pending()
        );

        assert_eq!(read.await, Err(Failure::Closed));
        close.await.unwrap();
        assert_eq!(late.write(b"late"), Err(Failure::Closed));
        assert_eq!(stream.allocations(), (0, 0));
    });
}

#[test]
fn abandoned_read_is_terminal_and_releases_all_owned_chunks() {
    runtime().block_on(async {
        let stream = Stream::new(None);
        let mut read = Box::pin(stream.read(1));

        assert!(
            poll_fn(|context| Poll::Ready(read.as_mut().poll(context)))
                .await
                .is_pending()
        );

        drop(read);
        assert_eq!(stream.read(1).await, Err(Failure::Closed));
        assert!(matches!(stream.reserve().await, Err(Failure::Closed)));
        stream.close().await.unwrap();
        assert_eq!(stream.allocations(), (0, 0));
    });
}

#[test]
fn collection_keeps_one_cursor_across_host_copies_and_checks_terminal_length() {
    runtime().block_on(async {
        let stream = Stream::new(Some(3));
        stream.reserve().await.unwrap().write(b"abc").unwrap();
        assert_eq!(stream.read(1).await.unwrap(), Some(b"a".to_vec()));

        let mut collection = stream.collection(2).unwrap();
        assert_eq!(collection.length, 2);
        assert_eq!(stream.read(1).await, Err(Failure::InUse));
        assert_eq!(collection.next().await.unwrap(), Some(b"bc".to_vec()));

        stream.end().unwrap();
        assert_eq!(collection.next().await.unwrap(), None);

        drop(collection);
        assert_eq!(stream.read(1).await.unwrap(), None);

        let incomplete = Stream::new(Some(2));
        let mut collection = incomplete.collection(2).unwrap();
        incomplete.reserve().await.unwrap().write(b"a").unwrap();
        assert_eq!(collection.next().await.unwrap(), Some(b"a".to_vec()));
        assert_eq!(incomplete.end(), Err(Failure::LengthMismatch));
        assert_eq!(collection.next().await, Err(Failure::LengthMismatch));

        drop(collection);
        assert_eq!(incomplete.read(1).await, Err(Failure::Closed));

        let canceled = Stream::new(Some(1));
        drop(canceled.collection(1).unwrap());
        assert_eq!(canceled.read(1).await, Err(Failure::Closed));
    });
}
