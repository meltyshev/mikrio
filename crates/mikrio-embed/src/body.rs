//! Host cursor ownership over one native body, with joinable close and bounded
//! commands. A collection retains the read claim across separate host copies.

use std::fmt;
use std::future::{Future, poll_fn};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::task::Poll;

use mikrio::{ObjectBody, StorageError};
use tokio::sync::{mpsc, oneshot, watch};

use crate::chunks::CHUNK_BYTES;

/// Host ownership failures remain distinct from native storage failures.
#[derive(Debug)]
pub enum BodyError {
    /// A second read cannot consume the same cursor concurrently.
    InUse,
    /// Explicit close, cancellation or abandoned collection ended this cursor.
    Closed,
    /// The known remaining length exceeds the caller's collection limit.
    TooLarge,
    /// A chunk bound must be positive.
    InvalidArgument,
    /// A native read or cleanup failed with its original cause.
    Native(StorageError),
    /// The managed task could not deliver a result.
    Unavailable,
}

impl BodyError {
    /// Stable classification shared by the host-language adapters.
    #[must_use]
    pub const fn code(&self) -> &'static str {
        match self {
            Self::InUse => "BodyInUse",
            Self::Closed => "BodyClosed",
            Self::TooLarge => "BodyTooLarge",
            Self::InvalidArgument => "InvalidArgument",
            Self::Native(error) => error.code().as_str(),
            Self::Unavailable => "IoError",
        }
    }
}

impl fmt::Display for BodyError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(match self {
            Self::InUse => "object body is already being read",
            Self::Closed => "object body is closed",
            Self::TooLarge => "object body exceeds the collection limit",
            Self::InvalidArgument => "chunk size must be positive",
            Self::Native(_) => "object body I/O failed",
            Self::Unavailable => "object body executor is unavailable",
        })
    }
}

impl std::error::Error for BodyError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::Native(error) => Some(error),
            _ => None,
        }
    }
}

struct State {
    commands: mpsc::Sender<Command>,
    stop: watch::Sender<bool>,
    // Acquiring a claim pairs with Reading's release after completion or cancel.
    // The command task alone owns the cursor; this flag rejects overlapping hosts.
    is_reading: AtomicBool,
    // Only the driver writes this count. Collection loads acquire its latest
    // published length after obtaining the exclusive read claim.
    remaining_bytes: Arc<AtomicU64>,
}

/// Cloneable host handle, never a second cursor or independent body selection.
#[derive(Clone)]
pub struct Body(Arc<State>);

enum Command {
    Read {
        max_bytes: usize,
        reply: oneshot::Sender<Result<Option<Vec<u8>>, BodyError>>,
    },
    Close {
        reply: oneshot::Sender<Result<(), BodyError>>,
    },
}

impl Body {
    /// Transfer a native body on its current executor. One bounded command task
    /// owns it and services repeated close attempts until the last host handle
    /// drops. No foreign object enters the task.
    /// `length` must be the body's exact remaining byte length.
    ///
    /// # Panics
    /// Panics when called outside a Tokio runtime.
    #[must_use]
    pub fn new(body: ObjectBody, length: u64) -> Self {
        let (commands, receiver) = mpsc::channel(1);
        let (stop, stopped) = watch::channel(false);
        let remaining_bytes = Arc::new(AtomicU64::new(length));

        let task = tokio::spawn(crate::contain(drive(
            body,
            receiver,
            stopped,
            Arc::clone(&remaining_bytes),
        )));

        // Result delivery belongs to command replies. A panic drops the receiver
        // and native body, making every pending/future host call fail closed.
        drop(task);

        Self(Arc::new(State {
            commands,
            stop,
            is_reading: AtomicBool::new(false),
            remaining_bytes,
        }))
    }

    /// Acquire one cursor claim without waiting behind another host read.
    ///
    /// # Errors
    /// Returns `Closed` or `InUse` without changing another read's ownership.
    pub fn start_read(&self) -> Result<Reading, BodyError> {
        if *self.0.stop.borrow() {
            return Err(BodyError::Closed);
        }

        self.0
            .is_reading
            .compare_exchange(false, true, Ordering::AcqRel, Ordering::Acquire)
            .map_err(|_| BodyError::InUse)?;

        Ok(Reading {
            body: self.clone(),
            is_complete: false,
        })
    }

    /// Reserve a whole collection before its final host allocation.
    ///
    /// # Errors
    /// Returns `Closed` or `InUse` when a cursor claim cannot be acquired.
    /// Returns `TooLarge` and closes the body when its remaining length exceeds
    /// `max_bytes` or cannot fit in `usize`; no payload buffer is allocated.
    pub fn collection(&self, max_bytes: usize) -> Result<(Reading, usize), BodyError> {
        let reading = self.start_read()?;

        let length = usize::try_from(self.0.remaining_bytes.load(Ordering::Acquire))
            .map_err(|_| BodyError::TooLarge)?;

        if length > max_bytes {
            return Err(BodyError::TooLarge);
        }

        Ok((reading, length))
    }

    /// Cancel reads immediately; actual cleanup remains owned by the driver.
    pub fn cancel(&self) {
        self.0.stop.send_replace(true);
    }

    /// Join cleanup, including after a canceled close or a previous cleanup error.
    ///
    /// # Errors
    /// Reports native cleanup failure or unavailable executor; never a short EOF.
    pub async fn close(&self) -> Result<(), BodyError> {
        self.cancel();

        let (reply, response) = oneshot::channel();

        self.0
            .commands
            .send(Command::Close { reply })
            .await
            .map_err(|_| BodyError::Unavailable)?;

        response.await.map_err(|_| BodyError::Unavailable)?
    }
}

/// Exclusive read claim. Dropping an unfinished read/collection cancels the body.
pub struct Reading {
    body: Body,
    is_complete: bool,
}

impl Reading {
    /// Read the next bounded slice while retaining the cursor claim.
    /// The returned chunk is at most `CHUNK_BYTES`, even for a larger request.
    /// Keep this claim until the request settles; abandoning it cancels the body.
    ///
    /// # Errors
    /// Reports native I/O, closure, unavailable execution and zero-sized bounds.
    /// Native read failures terminate the cursor. Drop the claim on an error;
    /// `finish` is reserved for successful reads.
    pub async fn next(&mut self, max_bytes: usize) -> Result<Option<Vec<u8>>, BodyError> {
        if max_bytes == 0 {
            return Err(BodyError::InvalidArgument);
        }

        if *self.body.0.stop.borrow() {
            return Err(BodyError::Closed);
        }

        let (reply, response) = oneshot::channel();

        self.body
            .0
            .commands
            .send(Command::Read {
                max_bytes: max_bytes.min(CHUNK_BYTES),
                reply,
            })
            .await
            .map_err(|_| BodyError::Unavailable)?;

        let chunk = response.await.map_err(|_| BodyError::Unavailable)??;

        if chunk.is_none() {
            self.is_complete = true;
        }

        Ok(chunk)
    }

    /// End a successful single-chunk read without closing the remaining cursor.
    pub fn finish(mut self) {
        self.is_complete = true;
    }
}

impl Drop for Reading {
    fn drop(&mut self) {
        if !self.is_complete {
            self.body.cancel();
        }

        self.body.0.is_reading.store(false, Ordering::Release);
    }
}

async fn until_stopped<T>(
    stopped: &mut watch::Receiver<bool>,
    future: impl Future<Output = T>,
) -> Option<T> {
    if *stopped.borrow() {
        return None;
    }

    let mut future = std::pin::pin!(future);
    let mut changed = std::pin::pin!(stopped.changed());

    poll_fn(|context| {
        // A ready stop signal wins before another poll can advance the cursor.
        if changed.as_mut().poll(context).is_ready() {
            return Poll::Ready(None);
        }

        future.as_mut().poll(context).map(Some)
    })
    .await
}

async fn drive(
    mut body: ObjectBody,
    mut commands: mpsc::Receiver<Command>,
    mut stopped: watch::Receiver<bool>,
    remaining_bytes: Arc<AtomicU64>,
) {
    let mut is_closing = false;

    loop {
        let command = if is_closing {
            commands.recv().await
        } else {
            let Some(command) = until_stopped(&mut stopped, commands.recv()).await else {
                is_closing = true;

                // Keep the body on failure: an explicit close retries and reports.
                let _cleanup = body.close().await;
                continue;
            };

            command
        };

        let Some(command) = command else {
            let _cleanup = body.close().await;
            return;
        };

        match command {
            Command::Close { reply } => {
                is_closing = true;
                let _delivery = reply.send(body.close().await.map_err(BodyError::Native));
            }
            Command::Read { max_bytes, reply } => {
                let result = if is_closing {
                    Err(BodyError::Closed)
                } else {
                    match until_stopped(&mut stopped, body.read_chunk(max_bytes)).await {
                        Some(result) => result.map_err(BodyError::Native),
                        None => Err(BodyError::Closed),
                    }
                };

                if let Ok(Some(bytes)) = &result {
                    // GET's exact length and the facade's bound make this conversion
                    // infallible on supported hosts; still reject corrupt accounting.
                    let byte_count = u64::try_from(bytes.len()).unwrap_or(u64::MAX);

                    let Some(bytes_left) = remaining_bytes
                        .load(Ordering::Relaxed)
                        .checked_sub(byte_count)
                    else {
                        let _delivery = reply.send(Err(BodyError::Unavailable));
                        is_closing = true;
                        let _cleanup = body.close().await;
                        continue;
                    };

                    // Only this driver advances the cursor; host claims observe it.
                    remaining_bytes.store(bytes_left, Ordering::Release);
                }

                let failed = result.is_err();

                if reply.send(result).is_err() || failed {
                    is_closing = true;
                    let _cleanup = body.close().await;
                }
            }
        }
    }
}
