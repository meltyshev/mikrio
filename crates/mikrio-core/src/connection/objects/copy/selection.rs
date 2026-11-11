//! One source snapshot protected with a new independent output reservation target.

use std::collections::BTreeMap;
use std::path::PathBuf;
use std::sync::Arc;

use crate::connection::Operation;
use crate::connection::objects::{acquire, select as select_object, source, write};
use crate::integrity::{Checksum, ChecksumAlgorithm, ChecksumType};
use crate::object::{Authority, ReadRequest, StoredObject};
use crate::{BlobId, Guard, MetadataError, OperationError, OperationFailure, StoreId};

pub(in crate::connection) struct Selection {
    pub object: StoredObject,
    pub output: write::Output,
    pub guards: Arc<[Arc<Guard>]>,
}

impl Selection {
    pub(in crate::connection) fn input(&self) -> Result<source::Input, OperationError> {
        let checksum = if self.object.checksum_type == Some(ChecksumType::FullObject) {
            self.object
                .checksums
                .first_key_value()
                .map(|(algorithm, value)| {
                    Checksum::parse(ChecksumAlgorithm::parse(algorithm)?, value)
                })
                .transpose()
                .map_err(|error| OperationError::caused(OperationFailure::StoreCorrupt, error))?
        } else {
            None
        };

        Ok(source::Input {
            blob_id: self.object.blob_id,
            length: self.object.length.get(),
            md5: self.object.md5,
            checksum,
        })
    }
}

pub(in crate::connection) async fn select(
    operation: Arc<Operation>,
    path: PathBuf,
    authority: Authority,
    request: &ReadRequest,
) -> Result<Option<Selection>, OperationError> {
    let candidate = read(Arc::clone(&operation), authority.clone(), request, None).await?;

    #[cfg(test)]
    crate::protocol_fault::hit("copy", "selected")?;

    let blob_id = BlobId::from_bytes(
        *StoreId::generate()
            .map_err(MetadataError::Entropy)?
            .as_bytes(),
    );

    #[cfg(test)]
    let blob_id = operation
        .state
        .context
        .test_hooks
        .copy_outputs
        .lock()
        .map_err(|_| OperationError::new(OperationFailure::IoError))?
        .remove(candidate.blob_id.as_bytes())
        .unwrap_or(blob_id);

    let shards = [candidate.blob_id, blob_id]
        .into_iter()
        .map(|blob| (blob.shard(), blob))
        .collect::<BTreeMap<_, _>>();

    let mut guards = BTreeMap::new();

    for (shard, blob) in shards {
        let guard = acquire(Arc::clone(&operation), path.clone(), blob).await?;
        guards.insert(shard, guard);
    }

    let source_guard = guards
        .get(&candidate.blob_id.shard())
        .ok_or_else(|| OperationError::new(OperationFailure::IoError))?;

    let object = read(
        Arc::clone(&operation),
        authority,
        request,
        Some(Arc::clone(source_guard)),
    )
    .await?;

    if object.blob_id != candidate.blob_id || object.generation != candidate.generation {
        return Ok(None);
    }

    let output_guard = guards
        .get(&blob_id.shard())
        .ok_or_else(|| OperationError::new(OperationFailure::IoError))?;

    let output = write::Output {
        operation,
        path,
        blob_id,
        guard: Arc::clone(output_guard),
    };

    if !output.available().await? {
        return Ok(None);
    }

    Ok(Some(Selection {
        object,
        output,
        guards: guards.into_values().collect::<Vec<_>>().into(),
    }))
}

async fn read(
    operation: Arc<Operation>,
    authority: Authority,
    request: &ReadRequest,
    guard: Option<Arc<Guard>>,
) -> Result<StoredObject, OperationError> {
    select_object(operation, authority, request.clone(), guard)
        .await
        .map_err(|error| {
            if error.failure() == OperationFailure::NotModified {
                return OperationError::new(OperationFailure::PreconditionFailed);
            }

            error
        })
}
