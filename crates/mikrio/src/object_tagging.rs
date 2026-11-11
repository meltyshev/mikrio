//! Whole-set object tagging over the shared core worker.

use crate::{StorageError, Store, Tag};

mod inputs;

pub use inputs::{
    DeleteObjectTaggingInput, DeleteObjectTaggingInputBuilder, GetObjectTaggingInput,
    GetObjectTaggingInputBuilder, PutObjectTaggingInput, PutObjectTaggingInputBuilder,
};

/// Detached current tags, usable after the connection closes.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct GetObjectTaggingOutput {
    tags: Vec<Tag>,
}

impl GetObjectTaggingOutput {
    /// Complete set in ascending UTF-8 key order; empty for an untagged object.
    #[must_use]
    pub fn tags(&self) -> &[Tag] {
        &self.tags
    }
}

/// Confirmation that the complete tag set was replaced.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PutObjectTaggingOutput;

/// Confirmation that the current object has no tags.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct DeleteObjectTaggingOutput;

impl Store {
    /// Read the complete tag set without opening a payload cursor.
    ///
    /// # Errors
    /// Preserves missing targets, owner, lifecycle and storage failures.
    pub async fn get_object_tagging(
        &self,
        input: GetObjectTaggingInput,
    ) -> Result<GetObjectTaggingOutput, StorageError> {
        let tags = self
            .connection
            .get_object_tags(input.request)
            .await
            .map_err(StorageError::from_operation)?;

        Ok(GetObjectTaggingOutput {
            tags: tags
                .into_entries()
                .into_iter()
                .map(|(key, value)| Tag { key, value })
                .collect(),
        })
    }

    /// Replace all tags atomically, preserving payload identity and timestamps.
    /// An empty set clears tags. There is no tag-set compare-and-swap contract.
    /// Dropping this future cannot undo a mutation already admitted to commit.
    ///
    /// # Errors
    /// Preserves missing targets, owner, lifecycle and storage failures.
    /// Dispatched mutation failures retain their authoritative outcome.
    pub async fn put_object_tagging(
        &self,
        input: PutObjectTaggingInput,
    ) -> Result<PutObjectTaggingOutput, StorageError> {
        self.connection
            .replace_object_tags(input.request, input.tags)
            .await
            .map_err(StorageError::from_operation)?;

        Ok(PutObjectTaggingOutput)
    }

    /// Clear tags on an existing object without changing its payload or age.
    /// Dropping this future cannot undo a mutation already admitted to commit.
    ///
    /// # Errors
    /// Preserves missing targets, owner, lifecycle and storage failures.
    /// Dispatched mutation failures retain their authoritative outcome.
    pub async fn delete_object_tagging(
        &self,
        input: DeleteObjectTaggingInput,
    ) -> Result<DeleteObjectTaggingOutput, StorageError> {
        self.connection
            .delete_object_tags(input.request)
            .await
            .map_err(StorageError::from_operation)?;

        Ok(DeleteObjectTaggingOutput)
    }
}
