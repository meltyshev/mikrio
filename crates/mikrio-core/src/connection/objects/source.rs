//! Bounded reads of guarded immutable inputs, shared by copy and assembly.

use std::future::Future;
use std::io::{self, Read, Seek, SeekFrom};
use std::path::PathBuf;
use std::pin::Pin;
use std::sync::Arc;
use std::task::{Context, Poll, ready};

use tokio::io::{AsyncRead, ReadBuf};

use crate::connection::Operation;
use crate::connection::objects::{CHUNK_BYTES, native};
use crate::integrity::{Checksum, ChecksumAlgorithm, ExpectedDigests, PayloadVerifier};
use crate::{BlobId, Guard, OperationError, OperationFailure};

type Pending = Pin<Box<dyn Future<Output = Result<(Reader, Vec<u8>), OperationError>> + Send>>;

#[derive(Clone, Copy)]
pub(in crate::connection) struct Input {
    pub(crate) blob_id: BlobId,
    pub(crate) length: u64,
    pub(crate) md5: [u8; 16],
    pub(crate) checksum: Option<Checksum>,
}

pub(in crate::connection) struct Source {
    reader: Option<Reader>,
    pending: Option<Pending>,
    bytes: Vec<u8>,
    position: usize,
    finished: bool,
}

// The descriptor is closed before protection and operation admission are released.
struct Reader {
    file: Option<std::fs::File>,
    _guards: Arc<[Arc<Guard>]>,
    operation: Arc<Operation>,
    path: PathBuf,
    inputs: Box<dyn Iterator<Item = Input> + Send>,
    current: Option<Input>,
    verifier: Option<PayloadVerifier>,
    remaining: u64,
    range: Option<crate::multipart::CopyRange>,
}

impl Source {
    pub(in crate::connection) fn new(
        operation: Arc<Operation>,
        path: PathBuf,
        inputs: impl Iterator<Item = Input> + Send + 'static,
        guards: Arc<[Arc<Guard>]>,
    ) -> Self {
        Self {
            reader: Some(Reader {
                file: None,
                _guards: guards,
                operation,
                path,
                inputs: Box::new(inputs),
                current: None,
                verifier: None,
                remaining: 0,
                range: None,
            }),
            pending: None,
            bytes: Vec::new(),
            position: 0,
            finished: false,
        }
    }

    /// Select a checked slice of the single guarded input. Full selections retain
    /// digest verification; partial selections have the ranged-GET integrity boundary.
    pub(in crate::connection) fn with_range(
        mut self,
        range: Option<crate::multipart::CopyRange>,
    ) -> Self {
        if let Some(reader) = &mut self.reader {
            reader.range = range;
        }

        self
    }
}

impl AsyncRead for Source {
    fn poll_read(
        mut self: Pin<&mut Self>,
        context: &mut Context<'_>,
        buffer: &mut ReadBuf<'_>,
    ) -> Poll<io::Result<()>> {
        if buffer.remaining() == 0 || self.finished {
            return Poll::Ready(Ok(()));
        }

        if self.position < self.bytes.len() {
            let count = buffer.remaining().min(self.bytes.len() - self.position);
            buffer.put_slice(&self.bytes[self.position..self.position + count]);
            self.position += count;
            return Poll::Ready(Ok(()));
        }

        if let Some(reader) = self.reader.take() {
            let bytes = std::mem::take(&mut self.bytes);
            self.pending = Some(Box::pin(native(move || reader.read_chunk(bytes))));
        }

        let Some(pending) = &mut self.pending else {
            return Poll::Ready(Err(io::Error::other(OperationError::new(
                OperationFailure::IoError,
            ))));
        };

        let result = ready!(pending.as_mut().poll(context));
        self.pending = None;
        let (reader, bytes) = result.map_err(io::Error::other)?;
        self.finished = bytes.is_empty();
        self.reader = Some(reader);
        self.bytes = bytes;
        self.position = 0;
        let count = buffer.remaining().min(self.bytes.len());
        buffer.put_slice(&self.bytes[..count]);
        self.position = count;
        Poll::Ready(Ok(()))
    }
}

impl Reader {
    fn read_chunk(mut self, mut bytes: Vec<u8>) -> Result<(Self, Vec<u8>), OperationError> {
        loop {
            if self.current.is_none() {
                self.current = self.inputs.next();
            }

            let Some(input) = self.current else {
                return Ok((self, Vec::new()));
            };

            if self.file.is_none() {
                let directory = self
                    .operation
                    .directory(&self.path)?
                    .open_child("objects")?
                    .open_child(&format!("{:02x}", input.blob_id.shard()))?;

                let mut file =
                    directory
                        .open_file(&input.blob_id.to_string())
                        .map_err(|error| {
                            let failure = if error.kind() == io::ErrorKind::NotFound {
                                OperationFailure::StoreCorrupt
                            } else {
                                OperationFailure::IoError
                            };

                            OperationError::caused(failure, error)
                        })?;

                if file.metadata()?.len() != input.length {
                    return Err(OperationError::new(OperationFailure::StoreCorrupt));
                }

                self.remaining = input.length;

                if let Some(range) = self.range {
                    self.remaining = range
                        .length(
                            crate::object::ByteLength::new(input.length)
                                .map_err(|_| OperationError::new(OperationFailure::StoreCorrupt))?,
                        )?
                        .get();

                    file.seek(SeekFrom::Start(range.first()))?;
                }

                if self.remaining == input.length {
                    self.verifier = Some(PayloadVerifier::new(
                        Some(self.remaining),
                        self.remaining,
                        input
                            .checksum
                            .map_or(ChecksumAlgorithm::Crc64Nvme, Checksum::algorithm),
                        ExpectedDigests::default(),
                    )?);
                }

                self.file = Some(file);
            }

            let count = usize::try_from(
                self.remaining
                    .min(u64::try_from(CHUNK_BYTES).map_err(io::Error::other)?),
            )
            .map_err(io::Error::other)?;

            bytes.resize(count, 0);

            #[cfg(test)]
            crate::protocol_fault::hit("stored.read", "before")?;

            self.file
                .as_mut()
                .ok_or_else(|| OperationError::new(OperationFailure::IoError))?
                .read_exact(&mut bytes)
                .map_err(|error| {
                    let failure = if error.kind() == io::ErrorKind::UnexpectedEof {
                        OperationFailure::StoreCorrupt
                    } else {
                        OperationFailure::IoError
                    };

                    OperationError::caused(failure, error)
                })?;

            #[cfg(test)]
            crate::protocol_fault::hit("stored.read", "after")?;

            if let Some(verifier) = &mut self.verifier {
                verifier.update(&bytes)?;
            }

            self.remaining -= u64::try_from(count).map_err(io::Error::other)?;

            if count != 0 {
                self.operation.progressed();
            }

            if self.remaining == 0 {
                if let Some(verifier) = self.verifier.take() {
                    let facts = verifier.finish(ExpectedDigests::default())?;

                    if *facts.md5() != input.md5 {
                        return Err(OperationError::new(OperationFailure::StoreCorrupt));
                    }

                    if input
                        .checksum
                        .is_some_and(|expected| expected != facts.checksum())
                    {
                        return Err(OperationError::new(OperationFailure::StoreCorrupt));
                    }
                }

                self.file = None;
                self.current = None;
            }

            if !bytes.is_empty() {
                return Ok((self, bytes));
            }
        }
    }
}
