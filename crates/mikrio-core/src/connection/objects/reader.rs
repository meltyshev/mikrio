//! One guarded file cursor with an owned driver and joinable cancellation.

use std::future::Future;
use std::io::{self, Read, Seek, SeekFrom};
use std::pin::Pin;
use std::sync::Arc;
use std::task::{Context, Poll};

use crate::body::{BodyError, ReadBody, ReadSource};
use tokio::io::ReadBuf;
use tokio::sync::{SemaphorePermit, mpsc, oneshot, watch};

use super::{CHUNK_BYTES, STREAM_SLOTS, acquire, contain, native, select};
use crate::connection::{ADMISSION_TIMEOUT, CancelOnDrop, Operation};
use crate::object::{Authority, ReadRequest, StoredObject};
use crate::{Connection, Guard, OperationError, OperationFailure};

impl Connection {
    /// Select a current generation and transfer its protected file to one cursor.
    ///
    /// # Errors
    /// Reports missing/conditional targets, bounded contention, corrupt missing
    /// or truncated files, and native lifecycle/I/O errors.
    pub async fn get_object(
        &self,
        request: ReadRequest,
    ) -> Result<(StoredObject, ReadBody), OperationError> {
        self.get(request, Authority::Native).await
    }

    /// Protect one HTTP selection, including its current access/tag snapshot.
    ///
    /// # Errors
    /// Denies before conditional/existence disclosure and preserves native errors.
    pub async fn get_object_http(
        &self,
        identity: Arc<crate::access::HttpIdentity>,
        request: ReadRequest,
    ) -> Result<(StoredObject, ReadBody), OperationError> {
        self.get(request, Authority::Http(identity)).await
    }

    async fn get(
        &self,
        request: ReadRequest,
        authority: Authority,
    ) -> Result<(StoredObject, ReadBody), OperationError> {
        let operation = Arc::new(self.admit()?);
        let cancellation = operation.cancel_on_drop();

        let slot = operation
            .interrupted(tokio::time::timeout(
                ADMISSION_TIMEOUT,
                STREAM_SLOTS.acquire(),
            ))
            .await?
            .map_err(|_| OperationError::new(OperationFailure::StoreBusy))?
            .map_err(|_| OperationError::new(OperationFailure::IoError))?;

        for _ in 0..4 {
            let candidate = select(
                Arc::clone(&operation),
                authority.clone(),
                request.clone(),
                None,
            )
            .await?;

            #[cfg(test)]
            {
                let checkpoint = self
                    .owner
                    .state
                    .context
                    .test_hooks
                    .after_read_selection
                    .lock()
                    .expect("owned read-selection checkpoint")
                    .take();

                if let Some((selected, released)) = checkpoint {
                    selected.send(()).expect("owned selection observer");
                    released.await.expect("owned selection release");
                }
            }

            let guard = acquire(
                Arc::clone(&operation),
                self.info.directory.clone(),
                candidate.blob_id,
            )
            .await?;

            let selected = select(
                Arc::clone(&operation),
                authority.clone(),
                request.clone(),
                Some(Arc::clone(&guard)),
            )
            .await?;

            if selected.blob_id != candidate.blob_id || selected.generation != candidate.generation
            {
                continue;
            }

            #[cfg(test)]
            {
                let checkpoint = self
                    .owner
                    .state
                    .context
                    .test_hooks
                    .after_read_guard
                    .lock()
                    .expect("owned guarded-read checkpoint")
                    .take();

                if let Some((guarded, released)) = checkpoint {
                    guarded.send(()).expect("owned guarded-read observer");
                    released.await.expect("owned guarded-read release");
                }
            }

            let path = self.info.directory.clone();
            let blob_id = selected.blob_id;
            let length = selected.length.get();
            let remaining = selected.content_length();
            let offset = selected.content_offset();
            let owner = Arc::clone(&operation);

            let file = native(move || {
                #[cfg(test)]
                crate::protocol_fault::hit("read", "open.before")?;

                let directory = owner
                    .directory(&path)?
                    .open_child("objects")?
                    .open_child(&format!("{:02x}", blob_id.shard()))?;

                let mut file = directory.open_file(&blob_id.to_string()).map_err(|error| {
                    let failure = if error.kind() == io::ErrorKind::NotFound {
                        OperationFailure::StoreCorrupt
                    } else {
                        OperationFailure::IoError
                    };

                    OperationError::caused(failure, error)
                })?;

                if file.metadata()?.len() != length {
                    return Err(OperationError::new(OperationFailure::StoreCorrupt));
                }

                if offset != 0 {
                    file.seek(SeekFrom::Start(offset))?;
                }

                Ok(FileOwner {
                    file,
                    _guard: guard,
                    _operation: owner,
                    _slot: slot,
                    remaining,
                })
            })
            .await?;

            let (sender, receiver) = mpsc::channel(1);
            let (finished, terminal) = oneshot::channel();
            let closing = operation.state.closing.subscribe();

            drop(tokio::spawn(async move {
                let result = contain(drive(file, receiver, operation)).await;
                let _delivery = finished.send(result.map_err(io::Error::other));
            }));

            let source = FileSource {
                sender,
                pending: None,
                terminal,
                cancellation,
                closing,
                eof: remaining == 0,
                joined: false,
            };

            return Ok((selected, ReadBody::new(source)));
        }

        Err(OperationError::new(OperationFailure::StoreBusy))
    }
}

// Field order closes the descriptor before its shard and operation owners.
struct FileOwner {
    file: std::fs::File,
    _guard: Arc<Guard>,
    _operation: Arc<Operation>,
    _slot: SemaphorePermit<'static>,
    remaining: u64,
}

struct ReadWork {
    length: usize,
    reply: oneshot::Sender<Result<(Vec<u8>, bool), OperationError>>,
}

async fn drive(
    mut file: FileOwner,
    mut receiver: mpsc::Receiver<ReadWork>,
    operation: Arc<Operation>,
) -> Result<(), OperationError> {
    if file.remaining == 0 {
        return Ok(());
    }

    loop {
        let Ok(Some(work)) = operation.interrupted(receiver.recv()).await else {
            return Ok(());
        };

        let result = native(move || {
            let limit = work
                .length
                .min(usize::try_from(file.remaining).unwrap_or(usize::MAX));

            let mut bytes = vec![0; limit];

            #[cfg(test)]
            crate::protocol_fault::hit("read", "native.before")?;

            let length = file.file.read(&mut bytes)?;

            #[cfg(test)]
            crate::protocol_fault::hit("read", "native.after")?;

            if length == 0 && file.remaining != 0 {
                return Err(OperationError::caused(
                    OperationFailure::StoreCorrupt,
                    io::Error::new(io::ErrorKind::UnexpectedEof, "protected payload truncated"),
                ));
            }

            bytes.truncate(length);

            file.remaining = file
                .remaining
                .checked_sub(u64::try_from(length).map_err(io::Error::other)?)
                .ok_or_else(|| io::Error::other("file cursor length underflow"))?;

            Ok((file, bytes))
        })
        .await;

        let (returned, bytes) = match result {
            Ok(result) => result,
            Err(error) => {
                let _delivery = work.reply.send(Err(error));
                return Ok(());
            }
        };

        file = returned;
        let eof = file.remaining == 0;

        if work.reply.send(Ok((bytes, eof))).is_err() || eof {
            return Ok(());
        }
    }
}

type ReadReply = oneshot::Receiver<Result<(Vec<u8>, bool), OperationError>>;

struct FileSource {
    sender: mpsc::Sender<ReadWork>,
    pending: Option<ReadReply>,
    terminal: oneshot::Receiver<io::Result<()>>,
    cancellation: CancelOnDrop,
    closing: watch::Receiver<bool>,
    eof: bool,
    joined: bool,
}

impl ReadSource for FileSource {
    fn poll_read(
        mut self: Pin<&mut Self>,
        context: &mut Context<'_>,
        buffer: &mut ReadBuf<'_>,
    ) -> Poll<Result<(), BodyError>> {
        if *self.closing.borrow() {
            return Poll::Ready(Err(BodyError::closed()));
        }

        if *self.cancellation.0.borrow() {
            return Poll::Ready(Err(BodyError::canceled()));
        }

        if self.eof {
            return Poll::Ready(Ok(()));
        }

        if self.pending.is_none() {
            let (reply, response) = oneshot::channel();

            let work = ReadWork {
                length: buffer.remaining().min(CHUNK_BYTES),
                reply,
            };

            if self.sender.try_send(work).is_err() {
                return Poll::Ready(Err(io::Error::other("file reader unavailable").into()));
            }

            self.pending = Some(response);
        }

        let Some(response) = &mut self.pending else {
            return Poll::Ready(Err(io::Error::other("missing read response").into()));
        };

        let result = match Pin::new(response).poll(context) {
            Poll::Pending => return Poll::Pending,
            Poll::Ready(result) => result,
        };

        self.pending = None;

        match result {
            Ok(Ok((bytes, eof))) => {
                buffer.put_slice(&bytes);
                self.eof = eof;
                Poll::Ready(Ok(()))
            }
            Ok(Err(error)) => Poll::Ready(Err(io::Error::other(error).into())),
            Err(_) if *self.closing.borrow() => Poll::Ready(Err(BodyError::closed())),
            Err(_) => Poll::Ready(Err(io::Error::other("file reader stopped").into())),
        }
    }

    fn cancel(self: Pin<&mut Self>) {
        self.cancellation.0.send_replace(true);
    }

    fn poll_close(mut self: Pin<&mut Self>, context: &mut Context<'_>) -> Poll<io::Result<()>> {
        if self.joined {
            return Poll::Ready(Ok(()));
        }

        match Pin::new(&mut self.terminal).poll(context) {
            Poll::Pending => Poll::Pending,
            Poll::Ready(result) => {
                self.joined = true;

                Poll::Ready(
                    result.unwrap_or_else(|_| {
                        Err(io::Error::other("file driver result unavailable"))
                    }),
                )
            }
        }
    }
}
