//! Bounded owned byte transfer at foreign/runtime boundaries.
//!
//! A reservation precedes allocation. Foreign adapters await a reservation, then
//! copy a bounded slice synchronously on the owning interpreter thread. They
//! must check cancellation before using a late reservation and must not retain
//! a foreign buffer reference on a native executor. This is not a storage writer.

use std::fmt;
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};

use bytes::Bytes;
use tokio::sync::{OwnedSemaphorePermit, Semaphore};

/// Internal transfer size, not a public Store tuning option.
pub const CHUNK_BYTES: usize = 64 * 1024;
const CHUNK_SLOTS: usize = 2;

/// Admission/allocation failure at the internal byte boundary.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ChunkError {
    /// Admission has closed; a late reservation cannot create another chunk.
    Closed,
    /// The caller must drain its source through bounded slices.
    TooLarge,
}

impl fmt::Display for ChunkError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(match self {
            Self::Closed => "byte transfer is closed",
            Self::TooLarge => "byte slice exceeds the internal chunk limit",
        })
    }
}

impl std::error::Error for ChunkError {}

struct State {
    slots: Arc<Semaphore>,
    retained_bytes: AtomicUsize,
    peak_bytes: AtomicUsize,
}

/// A shared allocation budget. Dropping a waiter cannot release another chunk's
/// reservation. Cloned/sliced byte views retain their entire allocation charge.
#[derive(Clone)]
pub struct ChunkBudget(Arc<State>);

impl Default for ChunkBudget {
    fn default() -> Self {
        Self(Arc::new(State {
            slots: Arc::new(Semaphore::new(CHUNK_SLOTS)),
            retained_bytes: AtomicUsize::new(0),
            peak_bytes: AtomicUsize::new(0),
        }))
    }
}

impl ChunkBudget {
    /// Wait for capacity without allocating or copying producer bytes.
    ///
    /// # Errors
    /// Returns `Closed` if shutdown revokes admission while this caller waits.
    pub async fn reserve(&self) -> Result<ChunkReservation, ChunkError> {
        let permit = Arc::clone(&self.0.slots)
            .acquire_owned()
            .await
            .map_err(|_| ChunkError::Closed)?;

        Ok(ChunkReservation {
            permit,
            state: Arc::clone(&self.0),
        })
    }

    /// Copy from a Rust-owned borrow after obtaining capacity. Host adapters use
    /// `reserve` followed by synchronous `copy` on their interpreter thread.
    ///
    /// # Errors
    /// Rejects oversized slices before waiting, or returns `Closed` on shutdown.
    pub async fn copy(&self, input: &[u8]) -> Result<Bytes, ChunkError> {
        validate_length(input)?;

        if input.is_empty() {
            return self.empty();
        }

        self.reserve().await?.copy(input)
    }

    /// Revoke new copies and wake capacity waiters. Existing owned bytes retain
    /// their memory and permit until the final view drops; close is not release.
    pub fn close(&self) {
        self.0.slots.close();
    }

    /// Observe payload allocation sizes, excluding caller-owned source buffers
    /// and fixed bookkeeping. Atomics are measurements, not the admission fence.
    /// Returns `(retained_bytes, peak_bytes)` from independent relaxed loads,
    /// rather than a synchronized snapshot of both counters.
    #[must_use]
    pub fn allocations(&self) -> (usize, usize) {
        (
            self.0.retained_bytes.load(Ordering::Relaxed),
            self.0.peak_bytes.load(Ordering::Relaxed),
        )
    }

    fn empty(&self) -> Result<Bytes, ChunkError> {
        if self.0.slots.is_closed() {
            return Err(ChunkError::Closed);
        }

        Ok(Bytes::new())
    }
}

/// One capacity reservation; dropping an unused reservation allocates no bytes.
pub struct ChunkReservation {
    permit: OwnedSemaphorePermit,
    state: Arc<State>,
}

impl ChunkReservation {
    /// Copy one bounded slice before the host may reuse its mutable source.
    /// Empty chunks remain empty bytes; this does not represent stream EOF.
    ///
    /// # Errors
    /// Rejects oversized slices and reservations delivered after shutdown.
    pub fn copy(self, input: &[u8]) -> Result<Bytes, ChunkError> {
        validate_length(input)?;

        if self.state.slots.is_closed() {
            return Err(ChunkError::Closed);
        }

        if input.is_empty() {
            return Ok(Bytes::new());
        }

        let bytes = input.to_vec().into_boxed_slice();
        let byte_count = bytes.len();

        // At most CHUNK_SLOTS reservations hold CHUNK_BYTES each. Neither
        // counter arithmetic nor the payload allocation can grow with input size.
        let retained_bytes = self
            .state
            .retained_bytes
            .fetch_add(byte_count, Ordering::Relaxed)
            + byte_count;

        self.state
            .peak_bytes
            .fetch_max(retained_bytes, Ordering::Relaxed);

        Ok(Bytes::from_owner(Allocation {
            bytes,
            _charge: Charge {
                state: self.state,
                byte_count,
            },
            _permit: self.permit,
        }))
    }
}

fn validate_length(input: &[u8]) -> Result<(), ChunkError> {
    if input.len() > CHUNK_BYTES {
        return Err(ChunkError::TooLarge);
    }

    Ok(())
}

// Field destruction order matters: free the backing allocation, remove its
// measurement charge, then release capacity. Every Bytes view owns this owner.
struct Allocation {
    bytes: Box<[u8]>,
    _charge: Charge,
    _permit: OwnedSemaphorePermit,
}

impl AsRef<[u8]> for Allocation {
    fn as_ref(&self) -> &[u8] {
        &self.bytes
    }
}

struct Charge {
    state: Arc<State>,
    byte_count: usize,
}

impl Drop for Charge {
    fn drop(&mut self) {
        self.state
            .retained_bytes
            .fetch_sub(self.byte_count, Ordering::Relaxed);
    }
}

#[cfg(test)]
mod tests {
    use std::future::{Future, poll_fn};
    use std::task::Poll;

    use super::*;

    fn runtime() -> tokio::runtime::Runtime {
        tokio::runtime::Builder::new_current_thread()
            .build()
            .unwrap()
    }

    #[test]
    fn slices_keep_the_full_charge_and_backpressure_until_last_view_drops() {
        runtime().block_on(async {
            let budget = ChunkBudget::default();
            let first = budget.copy(&vec![7; CHUNK_BYTES]).await.unwrap();
            let view = first.slice(0..1);
            drop(first);

            let second = budget.copy(&vec![8; CHUNK_BYTES]).await.unwrap();
            assert_eq!(budget.allocations(), (2 * CHUNK_BYTES, 2 * CHUNK_BYTES));

            let mut waiting = Box::pin(budget.reserve());

            assert!(
                poll_fn(|context| Poll::Ready(waiting.as_mut().poll(context)))
                    .await
                    .is_pending()
            );

            drop(view);
            let third = waiting.await.unwrap().copy(&[9]).unwrap();
            assert_eq!(budget.allocations(), (CHUNK_BYTES + 1, 2 * CHUNK_BYTES));
            assert_eq!(third.as_ref(), &[9]);

            drop(second);
            drop(third);
            assert_eq!(budget.allocations(), (0, 2 * CHUNK_BYTES));
        });
    }

    #[test]
    fn close_wakes_waiters_refuses_late_copy_and_retains_live_allocations() {
        runtime().block_on(async {
            let budget = ChunkBudget::default();
            let live = budget.copy(&[1, 2, 3]).await.unwrap();
            let late = budget.reserve().await.unwrap();
            let mut waiting = Box::pin(budget.reserve());

            assert!(
                poll_fn(|context| Poll::Ready(waiting.as_mut().poll(context)))
                    .await
                    .is_pending()
            );

            budget.close();
            assert!(matches!(waiting.await, Err(ChunkError::Closed)));
            assert_eq!(late.copy(&[4]), Err(ChunkError::Closed));
            assert_eq!(budget.copy(&[]).await, Err(ChunkError::Closed));
            assert_eq!(budget.allocations(), (3, 3));
            assert_eq!(live.as_ref(), &[1, 2, 3]);

            drop(live);
            assert_eq!(budget.allocations(), (0, 3));
        });
    }

    #[test]
    fn canceled_waiter_allocates_nothing_and_oversized_input_fails_before_waiting() {
        runtime().block_on(async {
            let budget = ChunkBudget::default();
            let first = budget.reserve().await.unwrap();
            let second = budget.reserve().await.unwrap();
            let mut waiting = Box::pin(budget.reserve());

            assert!(
                poll_fn(|context| Poll::Ready(waiting.as_mut().poll(context)))
                    .await
                    .is_pending()
            );

            drop(waiting);
            assert_eq!(budget.allocations(), (0, 0));

            assert_eq!(
                budget.copy(&vec![0; CHUNK_BYTES + 1]).await,
                Err(ChunkError::TooLarge)
            );

            assert_eq!(budget.copy(&[]).await.unwrap(), Bytes::new());

            drop(first);
            drop(second);
            let returned = budget.copy(b"reused").await.unwrap();
            assert_eq!(returned.as_ref(), b"reused");
        });
    }

    #[test]
    fn a_bounded_copy_does_not_retain_the_large_mutable_producer_allocation() {
        runtime().block_on(async {
            let budget = ChunkBudget::default();
            let mut producer = vec![42; 8 * 1024 * 1024];
            let chunk = budget.copy(&producer[..1024]).await.unwrap();

            producer.fill(0);
            drop(producer);
            assert_eq!(chunk.as_ref(), &[42; 1024]);
            assert_eq!(budget.allocations(), (1024, 1024));

            drop(chunk);
            assert_eq!(budget.allocations(), (0, 1024));
        });
    }
}
