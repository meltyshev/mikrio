//! Incomplete upload retirement is bounded and independent from object expiration.

use std::sync::Arc;

use tokio::sync::oneshot;

use super::{Command, Operation, queue, retention, unavailable};
use crate::OperationError;

pub(super) async fn expire_uploads(operation: Arc<Operation>) -> Result<(), OperationError> {
    let (reply, response) = oneshot::channel();

    queue(
        &operation,
        Command::UploadExpirations {
            reply,
            _operation: Arc::clone(&operation),
        },
    )?;

    let Some(page) = response.await.map_err(|_| unavailable())?? else {
        return Ok(());
    };

    retention::expire_page(operation, page, |candidate, reply, operation| {
        Command::ExpireUpload {
            candidate,
            reply,
            _operation: operation,
        }
    })
    .await
}
