#![allow(clippy::unwrap_used, reason = "owned bounded transport fixtures")]

use tokio::io::{AsyncReadExt, AsyncWriteExt};

use super::*;

#[test]
fn socket_stall_times_out_only_while_output_awaits_the_peer() {
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap();

    runtime.block_on(async {
        let (writer, _peer) = tokio::io::duplex(1);

        let mut socket = Socket {
            inner: writer,
            writing: Stall::new(Duration::from_millis(40)),
        };

        // Server work before any write has no peer-progress deadline.
        tokio::time::sleep(Duration::from_millis(80)).await;
        socket.write_all(b"a").await.unwrap();

        assert_eq!(
            socket.write_all(b"b").await.unwrap_err().kind(),
            io::ErrorKind::TimedOut
        );
    });
}

#[test]
fn healthy_socket_progress_can_outlast_the_stall_interval() {
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap();

    runtime.block_on(async {
        let (writer, mut peer) = tokio::io::duplex(1);

        let mut socket = Socket {
            inner: writer,
            writing: Stall::new(Duration::from_millis(150)),
        };

        let start = tokio::time::Instant::now();

        let producer = async {
            socket.write_all(b"abcdefg").await.unwrap();
            socket.shutdown().await.unwrap();
        };

        let consumer = async {
            let mut bytes = Vec::new();

            for _ in 0..7 {
                tokio::time::sleep(Duration::from_millis(40)).await;
                bytes.push(peer.read_u8().await.unwrap());
            }

            bytes
        };

        let ((), bytes) = futures_util::future::join(producer, consumer).await;
        assert_eq!(bytes, b"abcdefg");
        assert!(start.elapsed() > Duration::from_millis(150));
    });
}

#[test]
fn ready_flush_cannot_erase_a_blocked_write_deadline() {
    struct Blocked;

    impl AsyncWrite for Blocked {
        fn poll_write(
            self: Pin<&mut Self>,
            _: &mut Context<'_>,
            _: &[u8],
        ) -> Poll<io::Result<usize>> {
            Poll::Pending
        }

        fn poll_flush(self: Pin<&mut Self>, _: &mut Context<'_>) -> Poll<io::Result<()>> {
            Poll::Ready(Ok(()))
        }

        fn poll_shutdown(self: Pin<&mut Self>, _: &mut Context<'_>) -> Poll<io::Result<()>> {
            Poll::Ready(Ok(()))
        }
    }

    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap();

    runtime.block_on(async {
        let mut socket = Socket {
            inner: Blocked,
            writing: Stall::new(Duration::from_millis(40)),
        };

        let result = std::future::poll_fn(|context| {
            assert!(Pin::new(&mut socket).poll_flush(context).is_ready());
            Pin::new(&mut socket).poll_write(context, b"data")
        })
        .await;

        assert_eq!(result.unwrap_err().kind(), io::ErrorKind::TimedOut);
    });
}
