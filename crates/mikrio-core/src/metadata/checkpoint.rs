//! Passive SQLite checkpointing never acquires a shard or removes a sidecar.

use super::{MetadataDatabase, MetadataError};
use crate::OperationError;

impl MetadataDatabase {
    #[cfg(test)]
    pub(crate) fn test_checkpoint_authorizer(
        &mut self,
        denied: std::sync::Arc<std::sync::atomic::AtomicBool>,
        hits: std::sync::Arc<std::sync::atomic::AtomicUsize>,
    ) -> rusqlite::Result<()> {
        use rusqlite::hooks::{AuthAction, AuthContext, Authorization};
        use std::sync::atomic::Ordering;

        self.connection
            .authorizer(Some(move |context: AuthContext<'_>| {
                if let AuthAction::Pragma {
                    pragma_name: "wal_checkpoint",
                    pragma_value: Some("PASSIVE"),
                } = context.action
                {
                    hits.fetch_add(1, Ordering::SeqCst);

                    if denied.load(Ordering::SeqCst) {
                        return Authorization::Deny;
                    }
                }

                Authorization::Allow
            }))
    }

    pub(crate) fn checkpoint(&self) -> Result<(), OperationError> {
        if self.unusable || !self.connection.is_autocommit() {
            return Err(MetadataError::Corrupt("checkpoint requires a settled connection").into());
        }

        // PASSIVE never waits for a reader/writer to finish. The scheduler calls
        // at most once per sweep cycle; actual kernel I/O remains owned work.
        let (busy, pages, copied): (i64, i64, i64) =
            self.connection
                .query_row("PRAGMA wal_checkpoint(PASSIVE)", [], |row| {
                    Ok((row.get(0)?, row.get(1)?, row.get(2)?))
                })?;

        if !(0..=1).contains(&busy) || pages < -1 || copied < -1 || copied > pages {
            return Err(MetadataError::Corrupt("invalid checkpoint result").into());
        }

        Ok(())
    }
}
