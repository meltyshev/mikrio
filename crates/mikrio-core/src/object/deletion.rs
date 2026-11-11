//! Bounded ordered deletion inputs and detached per-position outcomes.

use super::{BucketName, ObjectKey, ObjectValueError, validate_entity_tag_condition};
use crate::{MutationOutcome, OperationError, OperationFailure};

/// One literal key and its optional current-generation condition.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ObjectIdentifier {
    pub key: ObjectKey,
    pub etag: Option<String>,
}

impl ObjectIdentifier {
    /// Validate an optional quoted entity tag or existence condition.
    ///
    /// # Errors
    /// Rejects malformed entity-tag conditions.
    pub fn validate(&self) -> Result<(), ObjectValueError> {
        if let Some(etag) = &self.etag {
            validate_entity_tag_condition(etag)?;
        }

        Ok(())
    }
}

/// One bucket and at most one thousand independently evaluated entries.
#[derive(Debug, Clone)]
pub struct DeleteObjectsRequest {
    pub bucket: BucketName,
    pub expected_owner: Option<String>,
    pub objects: Vec<ObjectIdentifier>,
}

impl DeleteObjectsRequest {
    pub(crate) fn validate(&self) -> Result<(), OperationError> {
        if !(1..=1000).contains(&self.objects.len()) {
            return Err(OperationError::new(OperationFailure::InvalidArgument));
        }

        if let Some(owner) = &self.expected_owner {
            crate::access::validate_account_id(owner).map_err(|error| {
                OperationError::caused(OperationFailure::InvalidArgument, error)
            })?;
        }

        for object in &self.objects {
            object.validate().map_err(|error| {
                OperationError::caused(OperationFailure::InvalidArgument, error)
            })?;
        }

        Ok(())
    }
}

/// A settled item; absence of a failure acknowledges an idempotent deletion.
#[derive(Debug)]
pub struct DeleteObjectResult {
    pub key: ObjectKey,
    pub failure: Option<OperationFailure>,
}

/// Outcome for one original input position, including undispatched entries.
#[derive(Debug)]
pub struct DeleteObjectProgress {
    pub key: ObjectKey,
    pub outcome: MutationOutcome,
    pub failure: Option<OperationFailure>,
}

/// Interruption retains the primary cause and, after admission, every position.
#[derive(Debug)]
pub struct DeleteObjectsError {
    pub error: OperationError,
    pub items: Option<Vec<DeleteObjectProgress>>,
}

impl From<OperationError> for DeleteObjectsError {
    fn from(error: OperationError) -> Self {
        Self { error, items: None }
    }
}

impl std::fmt::Display for DeleteObjectsError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        self.error.fmt(formatter)
    }
}

impl std::error::Error for DeleteObjectsError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        Some(&self.error)
    }
}
