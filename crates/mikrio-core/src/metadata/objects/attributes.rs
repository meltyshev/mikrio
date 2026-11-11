//! Attribute facts and completed layout come from the same authorized snapshot.

use super::{MetadataDatabase, access, layout, object};
use crate::object::{
    Attribute, AttributesRequest, Authority, ReadConditions, ReadRequest, Representation,
    StoredAttributes, StoredPartPage,
};

use crate::{OperationError, OperationFailure};

impl MetadataDatabase {
    pub(crate) fn select_attributes(
        &mut self,
        authority: &Authority,
        request: &AttributesRequest,
    ) -> Result<StoredAttributes, OperationError> {
        if request.max_parts.get() == 0 || request.part_number_marker > 10_000 {
            return Err(OperationError::new(OperationFailure::InvalidArgument));
        }

        let resource = ReadRequest {
            bucket: request.bucket.clone(),
            key: request.key.clone(),
            expected_owner: request.expected_owner.clone(),
            conditions: ReadConditions::default(),
            range: None,
            part_number: None,
            response: Representation::default(),
        };

        self.read(|tx| {
            let bucket_id = access::bucket(
                tx,
                authority,
                &resource.bucket,
                resource.expected_owner.as_deref(),
            )?;

            let selected = object(tx, bucket_id, resource.key.as_bytes())?;
            access::attributes(tx, authority, &resource, bucket_id, selected.as_ref())?;

            let selected =
                selected.ok_or_else(|| OperationError::new(OperationFailure::NoSuchKey))?;

            let contains = |attribute| request.attributes.contains(attribute);

            let object_parts = if contains(Attribute::ObjectParts) {
                if selected.checksum_type == Some(crate::integrity::ChecksumType::Composite) {
                    layout::page(
                        tx,
                        &selected,
                        request.part_number_marker,
                        request.max_parts.get(),
                    )?
                } else {
                    selected.part_count.map(|count| StoredPartPage {
                        total_parts_count: count,
                        entries_available: false,
                        parts: Vec::new(),
                        next_part_number_marker: None,
                    })
                }
            } else {
                None
            };

            let checksum = if contains(Attribute::Checksum) {
                selected.checksum_type.map(|mode| {
                    let checksums = selected
                        .checksums
                        .into_iter()
                        .map(|(algorithm, value)| {
                            let value = if mode == crate::integrity::ChecksumType::Composite {
                                value
                                    .split_once('-')
                                    .map_or_else(|| value.clone(), |(digest, _)| digest.to_owned())
                            } else {
                                value
                            };

                            (algorithm, value)
                        })
                        .collect();

                    (mode, checksums)
                })
            } else {
                None
            };

            Ok(StoredAttributes {
                object_size: contains(Attribute::ObjectSize).then_some(selected.length),
                etag: contains(Attribute::Etag).then_some(selected.etag),
                checksum,
                storage_class: contains(Attribute::StorageClass).then_some("STANDARD"),
                object_parts,
                last_modified: selected.last_modified,
            })
        })
    }
}
