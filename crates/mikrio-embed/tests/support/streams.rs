//! Observation-only stream fixture shared by the native host probes.
//! No SQL, object publication, filesystem payload, or successful Store method.

use std::collections::VecDeque;
use std::fmt;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};

use bytes::Bytes;
use mikrio_embed::chunks::{CHUNK_BYTES, ChunkBudget, ChunkError, ChunkReservation};
use tokio::sync::watch;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Failure {
    Closed,
    InUse,
    TooLarge,
    Producer,
    InvalidArgument,
    LengthMismatch,
}

impl Failure {
    pub(crate) const fn code(self) -> &'static str {
        match self {
            Self::Closed => "BodyClosed",
            Self::InUse => "BodyInUse",
            Self::TooLarge => "BodyTooLarge",
            Self::Producer => "BodyReadFailed",
            Self::InvalidArgument => "InvalidArgument",
            // This probe checks exact length, not a selected HTTP error mapping.
            Self::LengthMismatch => "LengthMismatch",
        }
    }
}

impl fmt::Display for Failure {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(self.code())
    }
}

impl std::error::Error for Failure {}

impl From<ChunkError> for Failure {
    fn from(error: ChunkError) -> Self {
        match error {
            ChunkError::Closed => Self::Closed,
            ChunkError::TooLarge => Self::InvalidArgument,
        }
    }
}

#[derive(Clone, Copy)]
enum Phase {
    Open,
    // The producer finished; buffered chunks remain readable.
    End,
    // The reader drained every chunk after successful producer termination.
    Eof,
    Failed(Failure),
    Closed,
}

struct State {
    phase: Phase,
    chunks: VecDeque<Bytes>,
    accepted_bytes: u64,
    consumed_bytes: u64,
}

/// An echo transport with controlled producer termination and backpressure.
pub(crate) struct Stream {
    budget: ChunkBudget,
    expected_bytes: Option<u64>,
    state: Mutex<State>,
    changed: watch::Sender<()>,
    // Acquiring a claim pairs with ActiveRead's release after settlement or
    // cancellation. Payload and phase changes remain under the state mutex.
    is_reading: AtomicBool,
}

impl Stream {
    pub(crate) fn new(expected_bytes: Option<u64>) -> Arc<Self> {
        let (changed, _) = watch::channel(());

        Arc::new(Self {
            budget: ChunkBudget::default(),
            expected_bytes,
            state: Mutex::new(State {
                phase: Phase::Open,
                chunks: VecDeque::new(),
                accepted_bytes: 0,
                consumed_bytes: 0,
            }),
            changed,
            is_reading: AtomicBool::new(false),
        })
    }

    pub(crate) async fn reserve(self: &Arc<Self>) -> Result<Reservation, Failure> {
        {
            let state = self.state.lock().map_err(|_| Failure::Producer)?;

            if !matches!(state.phase, Phase::Open) {
                return Err(Failure::Closed);
            }
        }

        let chunk = self.budget.reserve().await?;

        Ok(Reservation {
            stream: Arc::clone(self),
            chunk,
        })
    }

    /// Successful producer termination is distinct from an empty input chunk.
    pub(crate) fn end(&self) -> Result<(), Failure> {
        let result = {
            let mut state = self.state.lock().map_err(|_| Failure::Producer)?;

            if !matches!(state.phase, Phase::Open) {
                return Err(Failure::Closed);
            }

            if self
                .expected_bytes
                .is_some_and(|expected_bytes| expected_bytes != state.accepted_bytes)
            {
                state.phase = Phase::Failed(Failure::LengthMismatch);
                state.chunks.clear();
                Err(Failure::LengthMismatch)
            } else {
                state.phase = Phase::End;
                Ok(())
            }
        };

        self.budget.close();
        self.changed.send_replace(());
        result
    }

    pub(crate) fn fail(&self, error: Failure) -> Result<(), Failure> {
        {
            let mut state = self.state.lock().map_err(|_| Failure::Producer)?;

            state.phase = match state.phase {
                Phase::Open | Phase::End => Phase::Failed(error),
                Phase::Eof if error == Failure::Closed => Phase::Closed,
                Phase::Eof | Phase::Failed(_) | Phase::Closed => return Ok(()),
            };

            state.chunks.clear();
        }

        self.budget.close();
        self.changed.send_replace(());
        Ok(())
    }

    pub(crate) async fn read(
        self: &Arc<Self>,
        max_bytes: usize,
    ) -> Result<Option<Vec<u8>>, Failure> {
        if max_bytes == 0 {
            return Err(Failure::InvalidArgument);
        }

        let mut active = self.start_read()?;
        let result = self.read_owned(max_bytes.min(CHUNK_BYTES)).await;
        active.is_complete = true;
        result
    }

    #[allow(
        dead_code,
        reason = "shared fixture: Node/Rust collect owned Vec; Python fills PyBytes"
    )]
    pub(crate) async fn read_all(self: &Arc<Self>, max_bytes: usize) -> Result<Vec<u8>, Failure> {
        let mut active = self.start_read()?;
        let mut output = Vec::new();

        loop {
            // Request only one byte beyond the caller's remaining bound. Even
            // usize::MAX cannot overflow, and collection stays within max_bytes.
            let limit = (max_bytes - output.len())
                .saturating_add(1)
                .min(CHUNK_BYTES);

            let next = match self.read_owned(limit).await {
                Ok(next) => next,
                Err(error) => {
                    active.is_complete = true;
                    return Err(error);
                }
            };

            let Some(next) = next else {
                active.is_complete = true;
                return Ok(output);
            };

            if next.len() > max_bytes - output.len() {
                self.fail(Failure::Closed)?;
                active.is_complete = true;
                return Err(Failure::TooLarge);
            }

            // Capacity is part of the bound too; doubling the caller's limit
            // through Vec's geometric growth would invalidate the measurement.
            output.reserve_exact(next.len());
            output.extend_from_slice(&next);
        }
    }

    /// GET supplies an exact remaining length. This fixture can also derive it
    /// after producer EOF. Python allocates its final bytes once and fills it
    /// through this exclusive session, without a second full Rust collection.
    #[allow(
        dead_code,
        reason = "shared fixture: Python-specific allocation boundary"
    )]
    pub(crate) fn collection(self: &Arc<Self>, max_bytes: usize) -> Result<Collection, Failure> {
        let active = self.start_read()?;

        let remaining_bytes = {
            let mut state = self.state.lock().map_err(|_| Failure::Producer)?;

            let total_bytes = match state.phase {
                Phase::End | Phase::Eof => state.accepted_bytes,
                Phase::Open => self.expected_bytes.ok_or(Failure::InvalidArgument)?,
                Phase::Failed(error) => {
                    state.phase = Phase::Closed;
                    return Err(error);
                }
                Phase::Closed => return Err(Failure::Closed),
            };

            total_bytes
                .checked_sub(state.consumed_bytes)
                .ok_or(Failure::LengthMismatch)?
        };

        let length = usize::try_from(remaining_bytes).map_err(|_| Failure::TooLarge)?;

        if length > max_bytes {
            return Err(Failure::TooLarge);
        }

        Ok(Collection { active, length })
    }

    pub(crate) async fn close(&self) -> Result<(), Failure> {
        let mut changed = self.changed.subscribe();
        self.fail(Failure::Closed)?;

        while self.is_reading.load(Ordering::Acquire) {
            changed.changed().await.map_err(|_| Failure::Closed)?;
        }

        Ok(())
    }

    pub(crate) fn allocations(&self) -> (usize, usize) {
        self.budget.allocations()
    }

    pub(crate) fn is_reading(&self) -> bool {
        self.is_reading.load(Ordering::Acquire)
    }

    fn start_read(self: &Arc<Self>) -> Result<ActiveRead, Failure> {
        self.is_reading
            .compare_exchange(false, true, Ordering::AcqRel, Ordering::Acquire)
            .map_err(|_| Failure::InUse)?;

        Ok(ActiveRead {
            stream: Arc::clone(self),
            is_complete: false,
        })
    }

    async fn read_owned(&self, max_bytes: usize) -> Result<Option<Vec<u8>>, Failure> {
        let mut changed = self.changed.subscribe();

        loop {
            {
                let mut state = self.state.lock().map_err(|_| Failure::Producer)?;

                match state.phase {
                    Phase::Failed(error) => {
                        state.phase = Phase::Closed;
                        return Err(error);
                    }
                    Phase::Closed => return Err(Failure::Closed),
                    Phase::Eof => return Ok(None),
                    Phase::Open | Phase::End => {}
                }

                if let Some(chunk) = state.chunks.pop_front() {
                    let byte_count = max_bytes.min(chunk.len());
                    let result = chunk[..byte_count].to_vec();

                    state.consumed_bytes = state
                        .consumed_bytes
                        .checked_add(
                            u64::try_from(byte_count).map_err(|_| Failure::InvalidArgument)?,
                        )
                        .ok_or(Failure::InvalidArgument)?;

                    if byte_count < chunk.len() {
                        state.chunks.push_front(chunk.slice(byte_count..));
                    }

                    return Ok(Some(result));
                }

                if matches!(state.phase, Phase::End) {
                    state.phase = Phase::Eof;
                    return Ok(None);
                }
            }

            changed.changed().await.map_err(|_| Failure::Closed)?;
        }
    }
}

/// One read-all cursor spanning bounded host copies. Drop before EOF cancels it.
#[allow(
    dead_code,
    reason = "shared fixture: Python-specific allocation boundary"
)]
pub(crate) struct Collection {
    active: ActiveRead,
    pub(crate) length: usize,
}

#[allow(
    dead_code,
    reason = "shared fixture: Python-specific allocation boundary"
)]
impl Collection {
    pub(crate) async fn next(&mut self) -> Result<Option<Vec<u8>>, Failure> {
        let result = self.active.stream.read_owned(CHUNK_BYTES).await;

        if !matches!(result, Ok(Some(_))) {
            self.active.is_complete = true;
        }

        result
    }
}

/// An owned capacity promise; stale callbacks must still cross the closed fence.
pub(crate) struct Reservation {
    stream: Arc<Stream>,
    chunk: ChunkReservation,
}

impl Reservation {
    pub(crate) fn write(self, input: &[u8]) -> Result<(), Failure> {
        let bytes = self.chunk.copy(input)?;

        {
            let mut state = self.stream.state.lock().map_err(|_| Failure::Producer)?;

            if !matches!(state.phase, Phase::Open) {
                return Err(Failure::Closed);
            }

            let byte_count = u64::try_from(bytes.len()).map_err(|_| Failure::InvalidArgument)?;

            let accepted_bytes = state
                .accepted_bytes
                .checked_add(byte_count)
                .ok_or(Failure::InvalidArgument)?;

            if self
                .stream
                .expected_bytes
                .is_some_and(|expected_bytes| accepted_bytes > expected_bytes)
            {
                state.phase = Phase::Failed(Failure::LengthMismatch);
                state.chunks.clear();
                drop(state);
                self.stream.budget.close();
                self.stream.changed.send_replace(());
                return Err(Failure::LengthMismatch);
            }

            state.accepted_bytes = accepted_bytes;

            if !bytes.is_empty() {
                state.chunks.push_back(bytes);
            }
        }

        self.stream.changed.send_replace(());
        Ok(())
    }
}

struct ActiveRead {
    stream: Arc<Stream>,
    is_complete: bool,
}

impl Drop for ActiveRead {
    fn drop(&mut self) {
        if !self.is_complete {
            // Cursor advancement cannot be resumed after an abandoned read.
            // Poison stays visible; no host callback or panic occurs in Drop.
            let _failure = self.stream.fail(Failure::Closed);
        }

        self.stream.is_reading.store(false, Ordering::Release);
        self.stream.changed.send_replace(());
    }
}
