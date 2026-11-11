//! Logical expiration uses current policy; physical reclamation keeps its own guards.

use std::sync::Arc;
use std::time::{Duration, Instant, SystemTime};

use tokio::sync::oneshot;

use super::{Command, Operation, advance, queue, unavailable};
use crate::metadata::sweep::Page;
use crate::{EpochMillis, OperationError, OperationFailure};

pub(super) fn now() -> Result<EpochMillis, OperationError> {
    EpochMillis::from_system_time(SystemTime::now())
        .map_err(|error| OperationError::caused(OperationFailure::IoError, error))
}

pub(super) async fn expire_objects(operation: Arc<Operation>) -> Result<(), OperationError> {
    let (reply, response) = oneshot::channel();

    queue(
        &operation,
        Command::Expirations {
            reply,
            _operation: Arc::clone(&operation),
        },
    )?;

    let Some(page) = response.await.map_err(|_| unavailable())?? else {
        return Ok(());
    };

    expire_page(operation, page, |candidate, reply, operation| {
        Command::ExpireObject {
            candidate,
            reply,
            _operation: operation,
        }
    })
    .await
}

pub(super) async fn expire_page<T>(
    operation: Arc<Operation>,
    page: Page<Result<Option<T>, OperationError>>,
    command: impl Fn(T, oneshot::Sender<Result<(), OperationError>>, Arc<Operation>) -> Command,
) -> Result<(), OperationError> {
    let started = Instant::now();
    let mut failure = None;
    let end = page.progress.examined;
    let mut progress = page.progress;
    progress.examined = progress.cursor;
    let mut exhausted = true;

    for (row_id, candidate) in page.candidates {
        if *operation.state.closing.borrow() || started.elapsed() >= Duration::from_millis(50) {
            exhausted = false;
            break;
        }

        let result = match candidate {
            Ok(Some(candidate)) => {
                let (reply, response) = oneshot::channel();

                match queue(
                    &operation,
                    command(candidate, reply, Arc::clone(&operation)),
                ) {
                    Ok(()) => response
                        .await
                        .map_err(|_| unavailable())
                        .and_then(|result| result),
                    Err(error) => Err(error),
                }
            }
            Ok(None) => Ok(()),
            Err(error) => Err(error),
        };

        if let Err(error) = result {
            // A bad/busy candidate remains visible to the next pass. It never
            // holds the cursor ahead of unrelated eligible resources forever.
            failure.get_or_insert(error);
        }

        progress.examined = row_id;
    }

    if exhausted {
        progress.examined = end;
    }

    advance(operation, progress).await?;

    failure.map_or(Ok(()), Err)
}
