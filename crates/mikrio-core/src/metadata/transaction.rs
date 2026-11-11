//! Short operation transactions, with explicit settlement before outcome reporting.

use rusqlite::{DropBehavior, Transaction, TransactionBehavior};

use super::MetadataDatabase;
use crate::{MutationOutcome, OperationError, OperationFailure};

#[cfg(test)]
#[derive(Clone, Copy)]
pub(super) enum Fault {
    BeforeCommit,
    AfterCommit,
}

impl MetadataDatabase {
    pub(super) fn write<T>(
        &mut self,
        work: impl FnOnce(&Transaction<'_>) -> Result<T, OperationError>,
    ) -> Result<T, OperationError> {
        if self.unusable || !self.connection.is_autocommit() {
            return Err(OperationError::new(OperationFailure::IoError).uncommitted());
        }

        #[cfg(test)]
        crate::protocol_fault::metadata("begin.before")
            .map_err(|error| OperationError::from(error).uncommitted())?;

        let mut transaction = self
            .connection
            .transaction_with_behavior(TransactionBehavior::Immediate)
            .map_err(|error| OperationError::from(error).uncommitted())?;

        let result: Result<T, OperationError> = (|| {
            #[cfg(test)]
            crate::protocol_fault::metadata("begun")?;

            let value = work(&transaction)?;

            #[cfg(test)]
            crate::protocol_fault::metadata("commit.before")?;

            Ok(value)
        })();

        let value = match result {
            Ok(value) => value,
            Err(error) => {
                if !transaction.is_autocommit()
                    && let Err(rollback) = transaction.execute_batch("ROLLBACK")
                {
                    transaction.set_drop_behavior(DropBehavior::Ignore);
                    self.unusable = true;
                    // No COMMIT was attempted. Disable this connection so these
                    // provisional changes can never be published by later work.
                    return Err(error.settlement(rollback).uncommitted());
                }

                return Err(error.uncommitted());
            }
        };

        #[cfg(test)]
        let fault = self.transaction_fault.take();

        #[cfg(test)]
        if matches!(fault, Some(Fault::BeforeCommit)) {
            if let Err(error) = transaction.execute_batch("ROLLBACK") {
                transaction.set_drop_behavior(DropBehavior::Ignore);
                self.unusable = true;
                return Err(OperationError::from(error).uncommitted());
            }

            return Err(OperationError::new(OperationFailure::IoError).uncommitted());
        }

        if let Err(error) = transaction.execute_batch("COMMIT") {
            if transaction.is_autocommit() {
                return Err(OperationError::from(error).mutation(MutationOutcome::Unknown));
            }

            if let Err(rollback) = transaction.execute_batch("ROLLBACK") {
                transaction.set_drop_behavior(DropBehavior::Ignore);
                self.unusable = true;

                return Err(OperationError::from(error)
                    .settlement(rollback)
                    .mutation(MutationOutcome::Unknown));
            }

            return Err(OperationError::from(error).uncommitted());
        }

        #[cfg(test)]
        crate::protocol_fault::metadata("commit.after")
            .map_err(|error| OperationError::from(error).mutation(MutationOutcome::Unknown))?;

        #[cfg(test)]
        if matches!(fault, Some(Fault::AfterCommit)) {
            // Simulate acknowledgment loss after the actual durable effect.
            // Current metadata cannot attribute this particular request.
            return Err(
                OperationError::new(OperationFailure::IoError).mutation(MutationOutcome::Unknown)
            );
        }

        Ok(value)
    }

    pub(super) fn read<T>(
        &mut self,
        work: impl FnOnce(&Transaction<'_>) -> Result<T, OperationError>,
    ) -> Result<T, OperationError> {
        if self.unusable || !self.connection.is_autocommit() {
            return Err(OperationError::new(OperationFailure::IoError));
        }

        let mut transaction = self.connection.transaction()?;
        let result = work(&transaction);

        if !transaction.is_autocommit()
            && let Err(rollback) = transaction.execute_batch("ROLLBACK")
        {
            transaction.set_drop_behavior(DropBehavior::Ignore);
            self.unusable = true;

            return Err(match result {
                Ok(_) => OperationError::from(rollback),
                Err(error) => error.settlement(rollback),
            });
        }

        result
    }
}
