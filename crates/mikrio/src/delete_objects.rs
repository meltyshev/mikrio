//! Ordered batch inputs, detached results and interruption progress.

use mikrio_core::object::{BucketName, DeleteObjectsRequest, ObjectKey};

use crate::{ErrorCode, MutationOutcome, StorageError};

/// One literal key with an optional quoted `ETag` or `*` existence condition.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ObjectIdentifier(mikrio_core::object::ObjectIdentifier);

impl ObjectIdentifier {
    /// Start a builder for one original input position.
    #[must_use]
    pub fn builder(key: impl Into<String>) -> ObjectIdentifierBuilder {
        ObjectIdentifierBuilder {
            key: key.into(),
            etag: None,
        }
    }

    /// Exact application key.
    #[must_use]
    pub fn key(&self) -> &str {
        self.0.key.as_str()
    }

    /// Optional quoted entity tag or existence condition.
    #[must_use]
    pub fn etag(&self) -> Option<&str> {
        self.0.etag.as_deref()
    }
}

/// Validates one batch entry without accessing storage.
#[derive(Debug, Clone)]
pub struct ObjectIdentifierBuilder {
    key: String,
    etag: Option<String>,
}

impl ObjectIdentifierBuilder {
    /// Require a matching quoted entity tag, or `*` for existence.
    #[must_use]
    pub fn etag(mut self, etag: impl Into<String>) -> Self {
        self.etag = Some(etag.into());
        self
    }

    /// Validate and own this entry without dispatching work.
    ///
    /// # Errors
    /// Rejects invalid keys and malformed entity-tag conditions.
    pub fn build(self) -> Result<ObjectIdentifier, StorageError> {
        let entry = mikrio_core::object::ObjectIdentifier {
            key: ObjectKey::parse(&self.key).map_err(StorageError::from_object)?,
            etag: self.etag,
        };

        entry.validate().map_err(StorageError::from_object)?;
        Ok(ObjectIdentifier(entry))
    }
}

/// One to one thousand independently evaluated entries in caller order.
#[derive(Debug, Clone)]
pub struct DeleteObjectsInput {
    bucket: BucketName,
    expected_bucket_owner: Option<String>,
    objects: Vec<ObjectIdentifier>,
    quiet: bool,
}

impl DeleteObjectsInput {
    /// Start with the required bucket and ordered entries; quiet defaults false.
    #[must_use]
    pub fn builder(
        bucket: impl Into<String>,
        objects: Vec<ObjectIdentifier>,
    ) -> DeleteObjectsInputBuilder {
        DeleteObjectsInputBuilder {
            bucket: bucket.into(),
            expected_bucket_owner: None,
            objects,
            quiet: false,
        }
    }

    /// Exact bucket name.
    #[must_use]
    pub fn bucket(&self) -> &str {
        self.bucket.as_str()
    }

    /// Optional twelve-digit account assertion, never an authorization grant.
    #[must_use]
    pub fn expected_bucket_owner(&self) -> Option<&str> {
        self.expected_bucket_owner.as_deref()
    }

    /// Entries retain their original positions, including duplicate keys.
    #[must_use]
    pub fn objects(&self) -> &[ObjectIdentifier] {
        &self.objects
    }

    /// Suppress normal successes, preserving errors and interrupted progress.
    #[must_use]
    pub const fn quiet(&self) -> bool {
        self.quiet
    }
}

/// Builder for [`DeleteObjectsInput`]; construction performs no mutation.
#[derive(Debug, Clone)]
pub struct DeleteObjectsInputBuilder {
    bucket: String,
    expected_bucket_owner: Option<String>,
    objects: Vec<ObjectIdentifier>,
    quiet: bool,
}

impl DeleteObjectsInputBuilder {
    /// Assert ownership by this twelve-digit account, preserving leading zeroes.
    #[must_use]
    pub fn expected_bucket_owner(mut self, owner: impl Into<String>) -> Self {
        self.expected_bucket_owner = Some(owner.into());
        self
    }

    /// Suppress only the normal success list.
    #[must_use]
    pub const fn quiet(mut self, quiet: bool) -> Self {
        self.quiet = quiet;
        self
    }

    /// Validate the bucket, owner assertion and one-to-one-thousand entry bound.
    ///
    /// # Errors
    /// Returns `InvalidBucketName` or `InvalidArgument` before any storage work.
    pub fn build(self) -> Result<DeleteObjectsInput, StorageError> {
        let bucket = BucketName::parse(&self.bucket).map_err(StorageError::from_object)?;

        if !(1..=1000).contains(&self.objects.len()) {
            return Err(StorageError::from_object(
                mikrio_core::object::ObjectValueError::BatchSize,
            ));
        }

        if let Some(owner) = &self.expected_bucket_owner {
            mikrio_core::access::validate_account_id(owner).map_err(StorageError::from_identity)?;
        }

        Ok(DeleteObjectsInput {
            bucket,
            expected_bucket_owner: self.expected_bucket_owner,
            objects: self.objects,
            quiet: self.quiet,
        })
    }
}

/// Acknowledgment of deletion, including a successful unconditional no-op.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DeletedObject {
    key: String,
}

impl DeletedObject {
    /// Exact input key.
    #[must_use]
    pub fn key(&self) -> &str {
        &self.key
    }
}

/// Safe item failure details, detached from local causes and engine state.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DeleteError {
    code: ErrorCode,
    message: String,
}

impl DeleteError {
    /// Stable machine-readable storage error code.
    #[must_use]
    pub const fn code(&self) -> ErrorCode {
        self.code
    }

    /// Descriptive text without SQL, paths or underlying native errors.
    #[must_use]
    pub fn message(&self) -> &str {
        &self.message
    }

    fn from_core(failure: mikrio_core::OperationFailure) -> Self {
        Self {
            code: ErrorCode::from_operation(failure),
            message: failure.message().into(),
        }
    }
}

/// A known failed item in an otherwise completed batch.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DeleteObjectError {
    key: String,
    error: DeleteError,
}

impl DeleteObjectError {
    /// Exact input key; duplicate failures retain input order.
    #[must_use]
    pub fn key(&self) -> &str {
        &self.key
    }

    /// Stable machine-readable storage error code.
    #[must_use]
    pub const fn code(&self) -> ErrorCode {
        self.error.code()
    }

    /// Safe descriptive item failure.
    #[must_use]
    pub fn message(&self) -> &str {
        self.error.message()
    }
}

/// Completed per-item results; both collections are always present.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DeleteObjectsOutput {
    deleted: Vec<DeletedObject>,
    errors: Vec<DeleteObjectError>,
}

impl DeleteObjectsOutput {
    /// Successful input positions, empty in quiet mode.
    #[must_use]
    pub fn deleted(&self) -> &[DeletedObject] {
        &self.deleted
    }

    /// Known failures, including in quiet mode.
    #[must_use]
    pub fn errors(&self) -> &[DeleteObjectError] {
        &self.errors
    }
}

/// Outcome for one original input position in an interrupted batch.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DeleteObjectProgress {
    index: usize,
    key: String,
    mutation_outcome: MutationOutcome,
    error: Option<DeleteError>,
}

impl DeleteObjectProgress {
    /// Zero-based original position, independent of quiet mode or duplicate keys.
    #[must_use]
    pub const fn index(&self) -> usize {
        self.index
    }

    /// Exact original key.
    #[must_use]
    pub fn key(&self) -> &str {
        &self.key
    }

    /// Committed, not committed or uncertain; never aggregate `Partial`.
    #[must_use]
    pub const fn mutation_outcome(&self) -> MutationOutcome {
        self.mutation_outcome
    }

    /// Known failure details, absent for successful and undispatched entries.
    #[must_use]
    pub const fn error(&self) -> Option<&DeleteError> {
        self.error.as_ref()
    }
}

/// Detached outcomes covering every original input in an interrupted batch.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DeleteObjectsProgress {
    items: Vec<DeleteObjectProgress>,
}

impl DeleteObjectsProgress {
    /// Every original position in order, including those never dispatched.
    #[must_use]
    pub fn items(&self) -> &[DeleteObjectProgress] {
        &self.items
    }

    fn outcome(&self) -> MutationOutcome {
        if self
            .items
            .iter()
            .any(|item| item.mutation_outcome == MutationOutcome::Unknown)
        {
            return MutationOutcome::Unknown;
        }

        let committed_count = self
            .items
            .iter()
            .filter(|item| item.mutation_outcome == MutationOutcome::Committed)
            .count();

        if committed_count == 0 {
            return MutationOutcome::NotCommitted;
        }

        if committed_count == self.items.len() {
            return MutationOutcome::Committed;
        }

        MutationOutcome::Partial
    }
}

impl StorageError {
    /// Indexed batch progress; absence never establishes rollback.
    #[must_use]
    pub const fn partial_result(&self) -> Option<&DeleteObjectsProgress> {
        self.partial_result.as_ref()
    }

    fn from_batch(failure: mikrio_core::object::DeleteObjectsError) -> Self {
        let mut error = Self::from_operation(failure.error);

        if let Some(items) = failure.items {
            let progress = DeleteObjectsProgress {
                items: items
                    .into_iter()
                    .enumerate()
                    .map(|(index, item)| DeleteObjectProgress {
                        index,
                        key: item.key.as_str().into(),
                        mutation_outcome: match item.outcome {
                            mikrio_core::MutationOutcome::NotCommitted => {
                                MutationOutcome::NotCommitted
                            }
                            mikrio_core::MutationOutcome::Committed => MutationOutcome::Committed,
                            mikrio_core::MutationOutcome::Unknown => MutationOutcome::Unknown,
                        },
                        error: item.failure.map(DeleteError::from_core),
                    })
                    .collect(),
            };

            error.mutation_outcome = Some(progress.outcome());
            error.partial_result = Some(progress);
        }

        error
    }
}

impl crate::Store {
    /// Delete entries in input order, with one current transaction per entry.
    ///
    /// Known failures remain in the normal result. Quiet affects only successes.
    /// Dropping the future never retries a deletion or abandons native settlement.
    ///
    /// # Errors
    /// Interruption or uncertain settlement stops further dispatch and supplies
    /// indexed progress. Admission failures report `NotCommitted` without progress.
    pub async fn delete_objects(
        &self,
        input: DeleteObjectsInput,
    ) -> Result<DeleteObjectsOutput, StorageError> {
        self.delete_objects_until(input, std::future::pending())
            .await
    }

    pub(crate) async fn delete_objects_until(
        &self,
        input: DeleteObjectsInput,
        canceled: impl std::future::Future<Output = ()> + Send,
    ) -> Result<DeleteObjectsOutput, StorageError> {
        let results = self
            .connection
            .delete_objects_until(
                DeleteObjectsRequest {
                    bucket: input.bucket,
                    expected_owner: input.expected_bucket_owner,
                    objects: input.objects.into_iter().map(|object| object.0).collect(),
                },
                canceled,
            )
            .await
            .map_err(StorageError::from_batch)?;

        let mut output = DeleteObjectsOutput {
            deleted: Vec::new(),
            errors: Vec::new(),
        };

        for item in results {
            match item.failure {
                Some(failure) => output.errors.push(DeleteObjectError {
                    key: item.key.as_str().into(),
                    error: DeleteError::from_core(failure),
                }),
                None if !input.quiet => output.deleted.push(DeletedObject {
                    key: item.key.as_str().into(),
                }),
                None => {}
            }
        }

        Ok(output)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn interrupted_batch_projection_retains_positions_and_unknown_dominates() {
        use mikrio_core::MutationOutcome::{Committed, NotCommitted, Unknown};

        let cases = [
            ([Committed, Unknown, NotCommitted], MutationOutcome::Unknown),
            (
                [Committed, NotCommitted, NotCommitted],
                MutationOutcome::Partial,
            ),
            ([NotCommitted; 3], MutationOutcome::NotCommitted),
            ([Committed; 3], MutationOutcome::Committed),
        ];

        for (outcomes, aggregate) in cases {
            let error = StorageError::from_batch(mikrio_core::object::DeleteObjectsError {
                error: mikrio_core::OperationError::from(std::io::Error::other(
                    "synthetic settlement loss",
                )),
                items: Some(
                    outcomes
                        .into_iter()
                        .enumerate()
                        .map(
                            |(index, outcome)| mikrio_core::object::DeleteObjectProgress {
                                key: ObjectKey::parse("duplicate").expect("fixture key"),
                                outcome,
                                failure: (index == 1)
                                    .then_some(mikrio_core::OperationFailure::OperationCanceled),
                            },
                        )
                        .collect(),
                ),
            });

            assert_eq!(error.mutation_outcome(), Some(&aggregate));
            let progress = error.partial_result().expect("retained progress").clone();
            drop(error);
            assert_eq!(progress.items().len(), 3);

            for (index, item) in progress.items().iter().enumerate() {
                assert_eq!(item.index(), index);
                assert_eq!(item.key(), "duplicate");
                assert_ne!(item.mutation_outcome(), MutationOutcome::Partial);

                assert_eq!(
                    item.error().map(DeleteError::code),
                    (index == 1).then_some(ErrorCode::OperationCanceled)
                );
            }
        }
    }
}
