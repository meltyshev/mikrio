//! Bounded polling for parser futures backed by finite, in-memory streams.

use std::future::Future;
use std::pin::pin;
use std::task::{Context, Poll, Waker};

const MAX_POLLS: usize = 1_000_000;

#[expect(
    clippy::panic,
    reason = "Exhausting the poll budget is a fuzz finding."
)]
pub(super) fn complete<T>(future: impl Future<Output = T>) -> T {
    let mut future = pin!(future);
    let mut context = Context::from_waker(Waker::noop());

    // Only poll futures whose progress needs no I/O or external wakeup. Repeated
    // polling permits cooperative yields; the budget detects a stalled parser.
    for _ in 0..MAX_POLLS {
        if let Poll::Ready(value) = future.as_mut().poll(&mut context) {
            return value;
        }
    }

    panic!("finite parser input did not complete");
}
