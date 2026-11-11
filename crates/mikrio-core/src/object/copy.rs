//! Structured local copy inputs; source selection is independent of destination conditions.

use super::{
    BucketName, ByteLength, ObjectKey, ObjectTags, PutRequest, ReadConditions, ReadRequest,
    Representation, StoredObject, UserMetadata, WriteConditions,
};

use crate::integrity::{ChecksumAlgorithm, PayloadFacts, PutIntegrity, PutIntegrityInput};
use crate::{EpochMillis, OperationError, OperationFailure};

/// Select source fields or replace them with the corresponding request fields.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum CopyDirective {
    #[default]
    Copy,
    Replace,
}

/// An exact object address within this store, without HTTP encoding.
#[derive(Debug, Clone)]
pub struct ObjectRef {
    pub bucket: BucketName,
    pub key: ObjectKey,
}

/// Independent source selection and destination publication options.
#[derive(Debug, Clone)]
pub struct CopyRequest {
    pub bucket: BucketName,
    pub key: ObjectKey,
    pub source: ObjectRef,
    pub expected_owner: Option<String>,
    pub expected_source_owner: Option<String>,
    pub source_conditions: ReadConditions,
    pub conditions: WriteConditions,
    pub metadata_directive: CopyDirective,
    pub representation: Representation,
    pub metadata: UserMetadata,
    pub tagging_directive: CopyDirective,
    pub tags: Option<ObjectTags>,
    pub checksum_algorithm: Option<ChecksumAlgorithm>,
}

/// The newly published bytes and original publication time of this attempt.
#[derive(Debug)]
pub struct CopiedObject {
    pub facts: PayloadFacts,
    pub last_modified: EpochMillis,
}

impl CopyRequest {
    pub(crate) fn validate(&self) -> Result<(), OperationError> {
        if self.tagging_directive == CopyDirective::Copy && self.tags.is_some() {
            return Err(OperationError::new(OperationFailure::InvalidArgument));
        }

        if self.bucket == self.source.bucket
            && self.key == self.source.key
            && self.metadata_directive != CopyDirective::Replace
        {
            return Err(OperationError::new(OperationFailure::InvalidRequest));
        }

        Ok(())
    }

    pub(crate) fn source_request(&self) -> ReadRequest {
        ReadRequest {
            bucket: self.source.bucket.clone(),
            key: self.source.key.clone(),
            expected_owner: self.expected_source_owner.clone(),
            conditions: self.source_conditions.clone(),
            range: None,
            part_number: None,
            response: Representation::default(),
        }
    }

    pub(crate) fn destination(&self, source: &StoredObject) -> Result<PutRequest, OperationError> {
        let length = ByteLength::for_put(source.length.get())
            .map_err(|error| OperationError::caused(OperationFailure::EntityTooLarge, error))?;

        let inherited = source
            .checksums
            .keys()
            .next()
            .map(|algorithm| ChecksumAlgorithm::parse(algorithm))
            .transpose()
            .map_err(|error| OperationError::caused(OperationFailure::StoreCorrupt, error))?;

        let (representation, metadata) = match self.metadata_directive {
            CopyDirective::Copy => (source.representation.clone(), source.metadata.clone()),
            CopyDirective::Replace => (self.representation.clone(), self.metadata.clone()),
        };

        let tags = match self.tagging_directive {
            CopyDirective::Copy => source.tags.clone(),
            CopyDirective::Replace => self.tags.clone().unwrap_or_default(),
        };

        Ok(PutRequest {
            bucket: self.bucket.clone(),
            key: self.key.clone(),
            expected_owner: self.expected_owner.clone(),
            content_length: Some(length),
            representation,
            metadata,
            tags: Some(tags),
            integrity: PutIntegrity::new(PutIntegrityInput {
                checksum_algorithm: self.checksum_algorithm.or(inherited),
                ..PutIntegrityInput::default()
            })?,
            conditions: self.conditions.clone(),
        })
    }
}
