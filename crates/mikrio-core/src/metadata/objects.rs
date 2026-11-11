//! Object mappings, reservations and publication in short worker-owned transactions.

use std::collections::BTreeMap;

use rusqlite::{OptionalExtension, Row, Transaction, params};
use serde::Deserialize;

use super::{MetadataDatabase, MetadataError, buckets::require_bucket, decode_error, now};
use crate::integrity::{ChecksumAlgorithm, ChecksumType, ObjectChecksum, PayloadFacts};
use crate::object::{
    Authority, ByteLength, CopyRequest, ObjectTags, PutRequest, ReadConditionFailure, ReadRequest,
    Representation, RepresentationInput, StoredObject, UserMetadata,
};
use crate::{BlobId, EpochMillis, MutationOutcome, OperationError, OperationFailure, StoreId};

pub(super) mod access;
mod attributes;
pub(in crate::metadata) mod layout;

#[derive(Clone, Copy)]
enum WriteRequest<'a> {
    Put(&'a PutRequest),
    Copy(&'a CopyRequest, &'a PutRequest),
}

impl<'a> WriteRequest<'a> {
    fn input(self) -> &'a PutRequest {
        match self {
            Self::Put(input) | Self::Copy(_, input) => input,
        }
    }

    fn kind(self) -> &'static str {
        match self {
            Self::Put(_) => "put_object",
            Self::Copy(_, _) => "copy_object",
        }
    }

    fn authorize(
        self,
        transaction: &Transaction<'_>,
        authority: &Authority,
        bucket_id: [u8; 16],
    ) -> Result<(), OperationError> {
        match self {
            Self::Put(input) => access::put(transaction, authority, input, bucket_id),
            Self::Copy(request, input) => {
                access::copy(transaction, authority, request, input, bucket_id)
            }
        }
    }
}

const SELECT_OBJECT: &str = "SELECT o.generation, o.blob_id, b.blob_id, b.shard, b.state,
    b.byte_length, b.etag, b.md5_digest, b.part_count, b.checksum_type, b.checksums_json,
    o.content_type, o.content_encoding, o.content_disposition, o.content_language,
    o.cache_control, o.expires_at_ms, o.user_metadata_json, o.tags_json,
    o.last_modified_at_ms, o.lifecycle_anchor_at_ms
    FROM objects AS o LEFT JOIN blobs AS b ON b.blob_id = o.blob_id
    WHERE o.bucket_id = ?1 AND o.key = ?2";

impl MetadataDatabase {
    pub(crate) fn get_object_acl(
        &mut self,
        authority: &Authority,
        request: &ReadRequest,
    ) -> Result<crate::access::OwnerInfo, OperationError> {
        self.read(|transaction| {
            let bucket_id = access::bucket(
                transaction,
                authority,
                &request.bucket,
                request.expected_owner.as_deref(),
            )?;

            let selected = object(transaction, bucket_id, request.key.as_bytes())?;

            access::object_acl(
                transaction,
                authority,
                request,
                bucket_id,
                selected.as_ref(),
            )?;

            selected.ok_or_else(|| OperationError::new(OperationFailure::NoSuchKey))?;
            super::admin::owner_info(transaction)
        })
    }

    pub(crate) fn get_object_tags(
        &mut self,
        authority: &Authority,
        request: &ReadRequest,
    ) -> Result<ObjectTags, OperationError> {
        self.read(|transaction| {
            let bucket_id = access::bucket(
                transaction,
                authority,
                &request.bucket,
                request.expected_owner.as_deref(),
            )?;

            let selected = object(transaction, bucket_id, request.key.as_bytes())?;

            access::tagging(
                transaction,
                authority,
                request,
                bucket_id,
                selected.as_ref(),
                access::Tagging::Get,
            )?;

            selected
                .map(|object| object.tags)
                .ok_or_else(|| OperationError::new(OperationFailure::NoSuchKey))
        })
    }

    #[cfg(test)]
    pub(crate) fn replace_object_tags(
        &mut self,
        request: &ReadRequest,
        tags: &ObjectTags,
    ) -> Result<(), OperationError> {
        self.change_object_tags(&Authority::Native, request, Some(tags))
    }

    pub(crate) fn change_object_tags(
        &mut self,
        authority: &Authority,
        request: &ReadRequest,
        tags: Option<&ObjectTags>,
    ) -> Result<(), OperationError> {
        let generation = StoreId::generate().map_err(MetadataError::Entropy)?;

        let encoded = tags
            .map_or_else(|| Ok("{}".to_owned()), ObjectTags::to_json)
            .map_err(corrupt_value)?;

        #[cfg(test)]
        let _protocol = crate::protocol_fault::Scope::enter("tags.replace");

        self.write(|transaction| {
            let bucket_id = access::bucket(
                transaction,
                authority,
                &request.bucket,
                request.expected_owner.as_deref(),
            )?;

            let selected = object(transaction, bucket_id, request.key.as_bytes())?;

            access::tagging(
                transaction,
                authority,
                request,
                bucket_id,
                selected.as_ref(),
                tags.map_or(access::Tagging::Delete, access::Tagging::Put),
            )?;

            let current =
                selected.ok_or_else(|| OperationError::new(OperationFailure::NoSuchKey))?;

            let changed = transaction.execute(
                "UPDATE objects SET generation = ?4, tags_json = ?5
                 WHERE bucket_id = ?1 AND key = ?2 AND generation = ?3",
                params![
                    bucket_id.as_slice(),
                    request.key.as_bytes(),
                    current.generation.as_slice(),
                    generation.as_bytes().as_slice(),
                    encoded
                ],
            )?;

            if changed != 1 {
                return Err(MetadataError::Corrupt("tag update lost its object generation").into());
            }

            Ok(())
        })
    }

    pub(crate) fn reserve_put(
        &mut self,
        authority: &Authority,
        request: &PutRequest,
        blob_id: BlobId,
    ) -> Result<Option<[u8; 16]>, OperationError> {
        self.reserve_object(authority, WriteRequest::Put(request), blob_id)
    }

    pub(crate) fn reserve_copy(
        &mut self,
        authority: &Authority,
        request: &CopyRequest,
        input: &PutRequest,
        blob_id: BlobId,
    ) -> Result<Option<[u8; 16]>, OperationError> {
        self.reserve_object(authority, WriteRequest::Copy(request, input), blob_id)
    }

    fn reserve_object(
        &mut self,
        authority: &Authority,
        write: WriteRequest<'_>,
        blob_id: BlobId,
    ) -> Result<Option<[u8; 16]>, OperationError> {
        let request = write.input();
        let created_at = now()?;

        #[cfg(test)]
        let _protocol = crate::protocol_fault::Scope::enter("reservation");

        self.write(|transaction| {
            let bucket_id = access::bucket(
                transaction,
                authority,
                &request.bucket,
                request.expected_owner.as_deref(),
            )?;

            write.authorize(transaction, authority, bucket_id)?;

            if transaction
                .prepare_cached("SELECT 1 FROM blobs WHERE blob_id = ?1")?
                .exists([blob_id.as_bytes().as_slice()])?
            {
                return Ok(None);
            }

            transaction.execute(
                "INSERT INTO blobs (blob_id, shard, state, write_kind, target_bucket_id,
                 target_key, created_at_ms, gc_pending, gc_attempt_count)
                 VALUES (?1, ?2, 'reserved', ?6, ?3, ?4, ?5, 1, 0)",
                params![
                    blob_id.as_bytes().as_slice(),
                    i64::from(blob_id.shard()),
                    bucket_id.as_slice(),
                    request.key.as_bytes(),
                    created_at.get(),
                    write.kind()
                ],
            )?;

            Ok(Some(bucket_id))
        })
    }

    pub(crate) fn publish_put(
        &mut self,
        authority: &Authority,
        request: &PutRequest,
        bucket_id: [u8; 16],
        blob_id: BlobId,
        facts: &PayloadFacts,
    ) -> Result<(), OperationError> {
        self.publish_object(
            authority,
            WriteRequest::Put(request),
            bucket_id,
            blob_id,
            facts,
        )
        .map(|_| ())
    }

    pub(crate) fn publish_copy(
        &mut self,
        authority: &Authority,
        request: &CopyRequest,
        input: &PutRequest,
        bucket_id: [u8; 16],
        blob_id: BlobId,
        facts: &PayloadFacts,
    ) -> Result<EpochMillis, OperationError> {
        self.publish_object(
            authority,
            WriteRequest::Copy(request, input),
            bucket_id,
            blob_id,
            facts,
        )
    }

    fn publish_object(
        &mut self,
        authority: &Authority,
        write: WriteRequest<'_>,
        bucket_id: [u8; 16],
        blob_id: BlobId,
        facts: &PayloadFacts,
    ) -> Result<EpochMillis, OperationError> {
        let request = write.input();
        let generation = StoreId::generate().map_err(MetadataError::Entropy)?;
        let modified = now()?;
        let metadata = request.metadata.to_json().map_err(corrupt_value)?;

        let tags = request
            .tags
            .clone()
            .unwrap_or_default()
            .to_json()
            .map_err(corrupt_value)?;

        let etag = facts.etag();
        let checksum = facts.checksum();

        let checksums = serde_json::to_string(&BTreeMap::from([(
            checksum.algorithm().as_str(),
            checksum.encode(),
        )]))
        .map_err(|error| OperationError::caused(OperationFailure::IoError, error))?;

        let length = ByteLength::for_put(facts.length()).map_err(corrupt_value)?;

        #[cfg(test)]
        let _protocol = crate::protocol_fault::Scope::enter("publication");

        let result = self.write(|transaction| {
            let current_bucket = access::bucket(
                transaction,
                authority,
                &request.bucket,
                request.expected_owner.as_deref(),
            )?;

            if current_bucket != bucket_id {
                return Err(OperationError::new(OperationFailure::NoSuchBucket));
            }

            write.authorize(transaction, authority, bucket_id)?;

            let previous = write_destination(
                transaction,
                bucket_id,
                request.key.as_bytes(),
                &request.conditions,
            )?;

            let changed = transaction.execute(
                "UPDATE blobs SET state = 'ready', target_bucket_id = NULL, target_key = NULL,
                 byte_length = ?2, etag = ?3, md5_digest = ?4, checksum_type = 'FULL_OBJECT',
                 checksums_json = ?5, gc_pending = 0, gc_retry_at_ms = NULL,
                 gc_attempt_count = 0, gc_error_code = NULL
                 WHERE blob_id = ?1 AND state = 'reserved' AND write_kind = ?8
                 AND target_bucket_id = ?6 AND target_key = ?7",
                params![
                    blob_id.as_bytes().as_slice(),
                    length.as_stored(),
                    etag,
                    facts.md5().as_slice(),
                    checksums,
                    bucket_id.as_slice(),
                    request.key.as_bytes(),
                    write.kind()
                ],
            )?;

            if changed != 1 {
                return Err(MetadataError::Corrupt("publication lost its reservation").into());
            }

            #[cfg(test)]
            crate::protocol_fault::metadata("blob.ready")?;

            let fields = request.representation.fields();

            transaction.execute(
                "INSERT INTO objects (bucket_id, key, generation, blob_id, content_type,
                 content_encoding, content_disposition, content_language, cache_control,
                 expires_at_ms, user_metadata_json, tags_json, last_modified_at_ms, lifecycle_anchor_at_ms)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?13)
                 ON CONFLICT (bucket_id, key) DO UPDATE SET generation = excluded.generation,
                 blob_id = excluded.blob_id, content_type = excluded.content_type,
                 content_encoding = excluded.content_encoding, content_disposition = excluded.content_disposition,
                 content_language = excluded.content_language, cache_control = excluded.cache_control,
                 expires_at_ms = excluded.expires_at_ms, user_metadata_json = excluded.user_metadata_json,
                 tags_json = excluded.tags_json, last_modified_at_ms = excluded.last_modified_at_ms,
                 lifecycle_anchor_at_ms = excluded.lifecycle_anchor_at_ms",
                params![
                    bucket_id.as_slice(),
                    request.key.as_bytes(),
                    generation.as_bytes().as_slice(),
                    blob_id.as_bytes().as_slice(),
                    fields.content_type,
                    fields.content_encoding,
                    fields.content_disposition,
                    fields.content_language,
                    fields.cache_control,
                    fields.expires.map(EpochMillis::get),
                    metadata,
                    tags,
                    modified.get()
                ],
            )?;

            #[cfg(test)]
            crate::protocol_fault::metadata("object.replaced")?;

            if let Some(previous) = previous {
                retire(transaction, previous.blob_id)?;

                #[cfg(test)]
                crate::protocol_fault::metadata("predecessor.retired")?;
            }

            Ok(())
        });

        self.resolve_blob_publication(result, blob_id)?;
        Ok(modified)
    }

    pub(super) fn resolve_blob_publication(
        &mut self,
        result: Result<(), OperationError>,
        blob_id: BlobId,
    ) -> Result<(), OperationError> {
        let Err(error) = result else {
            return Ok(());
        };

        if error.outcome() != Some(MutationOutcome::Unknown) {
            return Err(error);
        }

        // The caller retains the output shard through this fresh snapshot.
        // A subsequent replacement cannot remove this attempt's ready witness.
        let witness = self.read(|transaction| {
            #[cfg(test)]
            crate::protocol_fault::hit("publication", "witness.before")?;

            let state: Option<String> = transaction
                .query_row(
                    "SELECT state FROM blobs WHERE blob_id = ?1",
                    [blob_id.as_bytes().as_slice()],
                    |row| row.get(0),
                )
                .optional()
                .map_err(decode_error)?;

            #[cfg(test)]
            crate::protocol_fault::hit("publication", "witness.after")?;

            Ok(state)
        });

        if witness.is_ok_and(|state| state.as_deref() == Some("ready")) {
            return Ok(());
        }

        Err(error)
    }

    pub(crate) fn select_object(
        &mut self,
        authority: &Authority,
        request: &ReadRequest,
    ) -> Result<StoredObject, OperationError> {
        if request.range.is_some() && request.part_number.is_some() {
            return Err(OperationError::new(OperationFailure::InvalidRequest));
        }

        let is_anonymous = matches!(
            authority,
            Authority::Http(identity) if matches!(identity.0, crate::access::Identity::Anonymous)
        );

        if is_anonymous && request.response != Representation::default() {
            return Err(OperationError::new(OperationFailure::AccessDenied));
        }

        self.read(|transaction| {
            let bucket_id = access::bucket(
                transaction,
                authority,
                &request.bucket,
                request.expected_owner.as_deref(),
            )?;

            let selected = object(transaction, bucket_id, request.key.as_bytes())?;

            let tag_count = access::read(
                transaction,
                authority,
                request,
                bucket_id,
                selected.as_ref(),
            )?;

            let mut selected =
                selected.ok_or_else(|| OperationError::new(OperationFailure::NoSuchKey))?;

            selected.tag_count = tag_count;

            request
                .conditions
                .evaluate(&selected.etag, selected.last_modified)
                .map_err(|error| match error {
                    ReadConditionFailure::NotModified => OperationError::not_modified(&selected),
                    ReadConditionFailure::PreconditionFailed => {
                        OperationError::new(OperationFailure::PreconditionFailed)
                    }
                })?;

            selected.range = request
                .range
                .as_ref()
                .map(|range| range.select(selected.length))
                .transpose()?;

            if let Some(number) = request.part_number {
                layout::select_part(transaction, &mut selected, number.get())?;
            }

            selected.representation.override_with(&request.response);

            if selected.range.is_some() {
                // Stored full-object checksums do not describe a returned slice.
                selected.checksums.clear();
                selected.checksum_type = None;
            }

            Ok(selected)
        })
    }

    pub(crate) fn prepare_delete_objects(
        &mut self,
        authority: &Authority,
        request: &crate::object::DeleteObjectsRequest,
    ) -> Result<(), OperationError> {
        self.read(|transaction| {
            access::bucket(
                transaction,
                authority,
                &request.bucket,
                request.expected_owner.as_deref(),
            )?;

            Ok(())
        })
    }

    pub(crate) fn delete_object(
        &mut self,
        authority: &Authority,
        request: &ReadRequest,
        if_match: Option<&str>,
    ) -> Result<(), OperationError> {
        #[cfg(test)]
        let _protocol = crate::protocol_fault::Scope::enter("object.delete");

        self.write(|transaction| {
            let bucket_id = access::bucket(
                transaction,
                authority,
                &request.bucket,
                request.expected_owner.as_deref(),
            )?;

            access::delete(transaction, authority, request, bucket_id, if_match)?;

            let selected = object(transaction, bucket_id, request.key.as_bytes())?;

            let Some(selected) = selected else {
                return if if_match.is_some() {
                    Err(OperationError::new(OperationFailure::NoSuchKey))
                } else {
                    Ok(())
                };
            };

            if if_match.is_some_and(|expected| expected != "*" && expected != selected.etag) {
                return Err(OperationError::new(OperationFailure::PreconditionFailed));
            }

            delete_selected(transaction, request.key.as_bytes(), &selected)
        })
    }
}

/// The caller establishes current deletion authority in this same transaction.
pub(super) fn delete_selected(
    transaction: &Transaction<'_>,
    key: &[u8],
    selected: &StoredObject,
) -> Result<(), OperationError> {
    let changed = transaction.execute(
        "DELETE FROM objects WHERE bucket_id = ?1 AND key = ?2 AND generation = ?3",
        params![
            selected.bucket_id.as_slice(),
            key,
            selected.generation.as_slice()
        ],
    )?;

    if changed != 1 {
        return Err(MetadataError::Corrupt("object deletion lost its generation").into());
    }

    retire(transaction, selected.blob_id)
}

pub(super) fn object(
    transaction: &Transaction<'_>,
    bucket_id: [u8; 16],
    key: &[u8],
) -> Result<Option<StoredObject>, OperationError> {
    let mut statement = transaction.prepare_cached(SELECT_OBJECT)?;
    let mut rows = statement.query(params![bucket_id.as_slice(), key])?;

    let Some(row) = rows.next()? else {
        return Ok(None);
    };

    Ok(Some(decode_object(row, bucket_id)?))
}

fn decode_object(row: &Row<'_>, bucket_id: [u8; 16]) -> Result<StoredObject, MetadataError> {
    let generation: [u8; 16] = row
        .get::<_, Vec<u8>>(0)
        .map_err(decode_error)?
        .try_into()
        .map_err(|_| MetadataError::Corrupt("invalid object generation"))?;

    let blob_id: [u8; 16] = row
        .get::<_, Vec<u8>>(1)
        .map_err(decode_error)?
        .try_into()
        .map_err(|_| MetadataError::Corrupt("invalid object blob identity"))?;

    let stored_id: Option<Vec<u8>> = row.get(2).map_err(decode_error)?;
    let state: Option<String> = row.get(4).map_err(decode_error)?;

    if stored_id.as_deref() != Some(blob_id.as_slice()) || state.as_deref() != Some("ready") {
        return Err(MetadataError::Corrupt("missing or non-ready object blob"));
    }

    let blob_id = BlobId::from_bytes(blob_id);
    let shard: i64 = row.get(3).map_err(decode_error)?;

    if shard != i64::from(blob_id.shard()) {
        return Err(MetadataError::Corrupt("object blob shard mismatch"));
    }

    let length = ByteLength::from_stored(row.get(5).map_err(decode_error)?)
        .map_err(|_| MetadataError::Corrupt("invalid object length"))?;

    let etag: String = row.get(6).map_err(decode_error)?;
    let md5: Vec<u8> = row.get(7).map_err(decode_error)?;
    let part_count: Option<i64> = row.get(8).map_err(decode_error)?;
    validate_etag(&etag, &md5, part_count)?;
    let mode: Option<String> = row.get(9).map_err(decode_error)?;

    let checksum_type = match mode.as_deref() {
        None => None,
        Some("FULL_OBJECT") => Some(ChecksumType::FullObject),
        Some("COMPOSITE") if part_count.is_some() => Some(ChecksumType::Composite),
        _ => return Err(MetadataError::Corrupt("invalid object checksum type")),
    };

    let encoded: String = row.get(10).map_err(decode_error)?;
    let checksums = decode_checksums(&encoded, checksum_type, part_count)?;

    let representation = Representation::new(RepresentationInput {
        content_type: row.get(11).map_err(decode_error)?,
        content_encoding: row.get(12).map_err(decode_error)?,
        content_disposition: row.get(13).map_err(decode_error)?,
        content_language: row.get(14).map_err(decode_error)?,
        cache_control: row.get(15).map_err(decode_error)?,
        expires: row
            .get::<_, Option<i64>>(16)
            .map_err(decode_error)?
            .map(timestamp)
            .transpose()?,
    })
    .map_err(|_| MetadataError::Corrupt("invalid stored representation"))?;

    let metadata: String = row.get(17).map_err(decode_error)?;

    let metadata = UserMetadata::from_json(&metadata)
        .map_err(|_| MetadataError::Corrupt("invalid stored metadata"))?;

    let tags: String = row.get(18).map_err(decode_error)?;

    let tags =
        ObjectTags::from_json(&tags).map_err(|_| MetadataError::Corrupt("invalid stored tags"))?;

    let last_modified = timestamp(row.get(19).map_err(decode_error)?)?;
    timestamp(row.get(20).map_err(decode_error)?)?;

    Ok(StoredObject {
        bucket_id,
        generation,
        blob_id,
        length,
        range: None,
        part: None,
        etag,
        md5: md5
            .try_into()
            .map_err(|_| MetadataError::Corrupt("invalid object MD5"))?,
        part_count: part_count
            .map(u32::try_from)
            .transpose()
            .map_err(|_| MetadataError::Corrupt("invalid object part count"))?,
        representation,
        metadata,
        tag_count: Some(tags.count()),
        tags,
        last_modified,
        checksum_type,
        checksums,
    })
}

pub(super) fn validate_etag(
    etag: &str,
    md5: &[u8],
    part_count: Option<i64>,
) -> Result<(), MetadataError> {
    let invalid = || MetadataError::Corrupt("invalid stored ETag or digest");

    if md5.len() != 16 {
        return Err(invalid());
    }

    let value = etag
        .strip_prefix('"')
        .and_then(|value| value.strip_suffix('"'))
        .ok_or_else(invalid)?;

    let digest = if let Some(count) = part_count {
        if !(1..=10_000).contains(&count) {
            return Err(invalid());
        }

        let (digest, suffix) = value.split_once('-').ok_or_else(invalid)?;

        if suffix != count.to_string() {
            return Err(invalid());
        }

        digest
    } else {
        value
    };

    let decoded = crate::values::decode_hex::<16>(digest.as_bytes()).map_err(|_| invalid())?;

    if part_count.is_none() && decoded != md5 {
        return Err(invalid());
    }

    Ok(())
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Checksums {
    #[serde(rename = "CRC32", default, deserialize_with = "checksum_value")]
    crc32: Option<String>,
    #[serde(rename = "CRC32C", default, deserialize_with = "checksum_value")]
    crc32c: Option<String>,
    #[serde(rename = "CRC64NVME", default, deserialize_with = "checksum_value")]
    crc64_nvme: Option<String>,
    #[serde(rename = "SHA1", default, deserialize_with = "checksum_value")]
    sha1: Option<String>,
    #[serde(rename = "SHA256", default, deserialize_with = "checksum_value")]
    sha256: Option<String>,
}

impl Checksums {
    fn entries(self) -> [(ChecksumAlgorithm, Option<String>); 5] {
        [
            (ChecksumAlgorithm::Crc32, self.crc32),
            (ChecksumAlgorithm::Crc32c, self.crc32c),
            (ChecksumAlgorithm::Crc64Nvme, self.crc64_nvme),
            (ChecksumAlgorithm::Sha1, self.sha1),
            (ChecksumAlgorithm::Sha256, self.sha256),
        ]
    }
}

fn checksum_value<'de, D: serde::Deserializer<'de>>(
    deserializer: D,
) -> Result<Option<String>, D::Error> {
    String::deserialize(deserializer).map(Some)
}

pub(super) fn decode_checksums(
    encoded: &str,
    mode: Option<ChecksumType>,
    part_count: Option<i64>,
) -> Result<BTreeMap<String, String>, MetadataError> {
    let invalid = || MetadataError::Corrupt("invalid stored checksums");

    if encoded.len() > 4096 {
        return Err(invalid());
    }

    let values: Checksums = serde_json::from_str(encoded).map_err(|_| invalid())?;
    let mut checksums = BTreeMap::new();

    for (algorithm, value) in values.entries() {
        let Some(value) = value else {
            continue;
        };

        let checksum = ObjectChecksum::parse(algorithm, mode.ok_or_else(invalid)?, &value)
            .map_err(|_| invalid())?;

        if checksum
            .part_count()
            .is_some_and(|count| Some(i64::from(count)) != part_count)
        {
            return Err(invalid());
        }

        checksums.insert(algorithm.as_str().into(), value);
    }

    if checksums.is_empty() != mode.is_none() {
        return Err(invalid());
    }

    Ok(checksums)
}

pub(super) fn retire(transaction: &Transaction<'_>, blob_id: BlobId) -> Result<(), OperationError> {
    if transaction.execute(
        "UPDATE blobs SET gc_pending = 1 WHERE blob_id = ?1 AND state = 'ready'",
        [blob_id.as_bytes().as_slice()],
    )? != 1
    {
        return Err(MetadataError::Corrupt("retirement lost its ready blob").into());
    }

    Ok(())
}

fn timestamp(value: i64) -> Result<EpochMillis, MetadataError> {
    EpochMillis::new(value).map_err(|_| MetadataError::Corrupt("invalid stored timestamp"))
}

fn corrupt_value(error: crate::object::ObjectValueError) -> OperationError {
    OperationError::caused(OperationFailure::StoreCorrupt, error)
}

pub(super) fn write_destination(
    transaction: &Transaction<'_>,
    bucket_id: [u8; 16],
    key: &[u8],
    conditions: &crate::object::WriteConditions,
) -> Result<Option<StoredObject>, OperationError> {
    let previous = object(transaction, bucket_id, key)?;

    if let Some(expected) = conditions.if_match() {
        let current = previous
            .as_ref()
            .ok_or_else(|| OperationError::new(OperationFailure::NoSuchKey))?;

        if expected != "*" && expected != current.etag {
            return Err(OperationError::new(OperationFailure::PreconditionFailed));
        }
    }

    if conditions.if_none_match().is_some() && previous.is_some() {
        return Err(OperationError::new(OperationFailure::PreconditionFailed));
    }

    Ok(previous)
}
