//! Guard-owned, bounded payload writing shared by object and multipart publication.

use std::future::poll_fn;
use std::io::Write;
use std::path::PathBuf;
use std::pin::pin;
use std::sync::Arc;

use tokio::io::{AsyncRead, ReadBuf};

use super::{CHUNK_BYTES, native};
use crate::connection::Operation;
use crate::integrity::{ExpectedDigests, PayloadFacts, PayloadVerifier};
use crate::{BlobId, DurableDirectory, Guard, OperationError, OperationFailure};

pub(in crate::connection) struct Output {
    pub operation: Arc<Operation>,
    pub path: PathBuf,
    pub blob_id: BlobId,
    pub guard: Arc<Guard>,
}

struct Staging {
    file: std::fs::File,
    source: DurableDirectory,
    destination: DurableDirectory,
    guard: Arc<Guard>,
    _operation: Arc<Operation>,
}

impl Output {
    pub(in crate::connection) async fn available(&self) -> Result<bool, OperationError> {
        let Self {
            operation,
            path,
            blob_id,
            guard,
        } = self;

        let blob_id = *blob_id;
        let candidate_path = path.clone();
        let candidate_owner = Arc::clone(operation);
        let candidate_guard = Arc::clone(guard);

        native(move || {
            let _guard = candidate_guard;
            let root = candidate_owner.directory(&candidate_path)?;

            for parent in ["staging", "objects"] {
                let directory = root
                    .open_child(parent)?
                    .open_child(&format!("{:02x}", blob_id.shard()))?;

                match directory.open_file(&blob_id.to_string()) {
                    Ok(_) => return Ok(false),
                    Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
                    Err(error) => {
                        return Err(OperationError::caused(
                            OperationFailure::StoreCorrupt,
                            error,
                        ));
                    }
                }
            }

            Ok(true)
        })
        .await
    }

    pub(in crate::connection) async fn publish<R: AsyncRead + Send + 'static>(
        self,
        source: R,
        verifier: PayloadVerifier,
        terminal: impl FnOnce() -> std::io::Result<ExpectedDigests> + Send + 'static,
    ) -> Result<(Arc<Guard>, PayloadFacts), OperationError> {
        self.transfer(source, verifier, terminal, |error| {
            OperationError::caused(OperationFailure::BodyReadFailed, error)
        })
        .await
    }

    pub(in crate::connection) async fn publish_stored<R: AsyncRead + Send + 'static>(
        self,
        source: R,
        verifier: PayloadVerifier,
    ) -> Result<(Arc<Guard>, PayloadFacts), OperationError> {
        self.transfer(
            source,
            verifier,
            || Ok(ExpectedDigests::default()),
            |error| {
                let Some(cause) = error.into_inner() else {
                    return OperationError::new(OperationFailure::IoError);
                };

                match cause.downcast::<OperationError>() {
                    Ok(error) => *error,
                    Err(cause) => OperationError::caused(
                        OperationFailure::IoError,
                        std::io::Error::other(cause),
                    ),
                }
            },
        )
        .await
    }

    async fn transfer<R: AsyncRead + Send + 'static>(
        self,
        source: R,
        mut verifier: PayloadVerifier,
        terminal: impl FnOnce() -> std::io::Result<ExpectedDigests> + Send + 'static,
        read_error: fn(std::io::Error) -> OperationError,
    ) -> Result<(Arc<Guard>, PayloadFacts), OperationError> {
        let Self {
            operation,
            path,
            blob_id,
            guard,
        } = self;

        let native_operation = Arc::clone(&operation);

        let mut staging = native(move || {
            let root = native_operation.directory(&path)?;

            let source = root
                .open_child("staging")?
                .open_child(&format!("{:02x}", blob_id.shard()))?;

            let destination = root
                .open_child("objects")?
                .open_child(&format!("{:02x}", blob_id.shard()))?;

            #[cfg(test)]
            crate::protocol_fault::hit("staging", "create.before")?;

            let file = source.create_file(&blob_id.to_string())?;

            #[cfg(test)]
            crate::protocol_fault::hit("staging", "create.after")?;

            Ok(Staging {
                file,
                source,
                destination,
                guard,
                _operation: native_operation,
            })
        })
        .await?;

        let mut source = pin!(source);
        let mut chunk = vec![0; CHUNK_BYTES];

        loop {
            let mut buffer = ReadBuf::new(&mut chunk);

            operation
                .interrupted(poll_fn(|context| {
                    source.as_mut().poll_read(context, &mut buffer)
                }))
                .await?
                .map_err(read_error)?;

            let length = buffer.filled().len();

            if length == 0 {
                break;
            }

            verifier.update(buffer.filled())?;

            (staging, chunk) = native(move || {
                #[cfg(test)]
                crate::protocol_fault::hit("staging", "write.before")?;

                // Keep the initialized buffer intact; short reads must neither
                // write its stale tail nor clear that unused tail for the next read.
                staging.file.write_all(&chunk[..length])?;

                #[cfg(test)]
                crate::test_metrics::payload_written(length);

                #[cfg(test)]
                crate::protocol_fault::hit("staging", "write.after")?;

                Ok((staging, chunk))
            })
            .await?;

            operation.progressed();
            tokio::task::yield_now().await;
        }

        let facts = verifier.finish(terminal()?)?;

        #[cfg(test)]
        crate::protocol_fault::hit("integrity", "verified")?;

        let guard = native(move || {
            staging.source.publish_new(
                &blob_id.to_string(),
                &staging.destination,
                &blob_id.to_string(),
            )?;

            #[cfg(test)]
            crate::protocol_fault::hit("payload", "published")?;

            // File descriptor closes before its physical protection can end.
            drop(staging.file);
            Ok(staging.guard)
        })
        .await?;

        operation.progressed();
        Ok((guard, facts))
    }
}
