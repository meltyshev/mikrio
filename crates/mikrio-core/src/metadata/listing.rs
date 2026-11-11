//! Consistent bounded namespace pages; delimiter groups advance by indexed seek.

use rusqlite::{Row, Transaction, params};

use super::namespace::{common_prefix, successor};
use super::{MetadataDatabase, MetadataError, decode_error};
use crate::access::{Action, Context};
use crate::object::{
    Authority, ByteLength, ListObjectsRequest, ListedObject, ObjectKey, ObjectPage,
};
use crate::{AccessRequest, EpochMillis, OperationError, OperationFailure};

mod token;
use token::{Cursor, Scope};

// Every form uses the (bucket_id, key) index, including an empty prefix. The
// inclusive boundary also represents a byte successor after a delimiter group.
const INCLUSIVE: &str = "SELECT o.key, o.blob_id, b.blob_id, b.state, b.byte_length,
    b.etag, o.last_modified_at_ms, b.md5_digest, b.part_count, b.shard FROM objects AS o
    LEFT JOIN blobs AS b ON b.blob_id = o.blob_id
    WHERE o.bucket_id = ?1 AND o.key >= ?2 ORDER BY o.key LIMIT ?3";
const EXCLUSIVE: &str = "SELECT o.key, o.blob_id, b.blob_id, b.state, b.byte_length,
    b.etag, o.last_modified_at_ms, b.md5_digest, b.part_count, b.shard FROM objects AS o
    LEFT JOIN blobs AS b ON b.blob_id = o.blob_id
    WHERE o.bucket_id = ?1 AND o.key > ?2 ORDER BY o.key LIMIT ?3";
const INCLUSIVE_UPPER: &str = "SELECT o.key, o.blob_id, b.blob_id, b.state, b.byte_length,
    b.etag, o.last_modified_at_ms, b.md5_digest, b.part_count, b.shard FROM objects AS o
    LEFT JOIN blobs AS b ON b.blob_id = o.blob_id
    WHERE o.bucket_id = ?1 AND o.key >= ?2 AND o.key < ?4 ORDER BY o.key LIMIT ?3";
const EXCLUSIVE_UPPER: &str = "SELECT o.key, o.blob_id, b.blob_id, b.state, b.byte_length,
    b.etag, o.last_modified_at_ms, b.md5_digest, b.part_count, b.shard FROM objects AS o
    LEFT JOIN blobs AS b ON b.blob_id = o.blob_id
    WHERE o.bucket_id = ?1 AND o.key > ?2 AND o.key < ?4 ORDER BY o.key LIMIT ?3";

impl MetadataDatabase {
    pub(crate) fn list_objects(
        &mut self,
        authority: &Authority,
        request: &ListObjectsRequest,
    ) -> Result<ObjectPage, OperationError> {
        request.validate()?;

        self.read(|transaction| {
            let bucket_id = super::buckets::require_bucket(
                transaction,
                &request.bucket,
                request.expected_owner.as_deref(),
            )?;

            authorize(transaction, authority, request, bucket_id)?;
            let scope = Scope::load(transaction, bucket_id, request)?;
            let prefix = request.prefix.as_deref().unwrap_or_default();
            let upper = successor(prefix.as_bytes());

            let mut cursor = scope
                .decode(request.continuation_token.as_deref())?
                .unwrap_or_else(|| initial_cursor(request, prefix));

            let mut output = ObjectPage::default();

            let limit = usize::try_from(request.max_keys.get())
                .map_err(|_| OperationError::new(OperationFailure::InvalidArgument))?;

            if limit == 0 {
                return Ok(output);
            }

            let delimiter = request
                .delimiter
                .as_deref()
                .filter(|value| !value.is_empty());

            // A continuation position replaces StartAfter. It already records
            // the first page's excluded group and may survive that key's deletion.
            let start_after = request
                .continuation_token
                .is_none()
                .then_some(request.start_after.as_deref())
                .flatten();

            loop {
                let count = output.contents.len() + output.common_prefixes.len();

                let candidates = select(
                    transaction,
                    bucket_id,
                    &cursor,
                    upper.as_deref(),
                    limit - count + 1,
                    prefix,
                    delimiter,
                )?;

                if candidates.is_empty() {
                    return Ok(output);
                }

                for object in candidates {
                    let group = common_prefix(&object.key, prefix, delimiter);

                    if let Some(group) = group {
                        let next = successor(group.as_bytes()).ok_or_else(|| {
                            MetadataError::Corrupt("invalid delimiter seek boundary")
                        })?;

                        if start_after.is_some_and(|after| group <= after) {
                            cursor = Cursor::Inclusive(next);
                            continue;
                        }
                    }

                    if output.contents.len() + output.common_prefixes.len() == limit {
                        output.next_continuation_token = Some(scope.encode(&cursor)?);
                        return Ok(output);
                    }

                    if let Some(group) = group {
                        cursor =
                            Cursor::Inclusive(successor(group.as_bytes()).ok_or_else(|| {
                                MetadataError::Corrupt("invalid delimiter seek boundary")
                            })?);

                        output.common_prefixes.push(group.to_owned());
                    } else {
                        cursor = Cursor::Exclusive(object.key.as_bytes().to_vec());
                        output.contents.push(object);
                    }
                }
            }
        })
    }
}

fn initial_cursor(request: &ListObjectsRequest, prefix: &str) -> Cursor {
    match request.start_after.as_deref() {
        Some(after) if after >= prefix => Cursor::Exclusive(after.as_bytes().to_vec()),
        _ => Cursor::Inclusive(prefix.as_bytes().to_vec()),
    }
}

fn authorize(
    transaction: &Transaction<'_>,
    authority: &Authority,
    request: &ListObjectsRequest,
    bucket_id: [u8; 16],
) -> Result<(), OperationError> {
    let Authority::Http(identity) = authority else {
        return Ok(());
    };

    let mut context = Context::default();

    for (name, value) in [
        ("s3:prefix", &request.prefix),
        ("s3:delimiter", &request.delimiter),
    ] {
        if let Some(value) = value {
            context.insert(name, vec![value.clone()]).map_err(|error| {
                OperationError::caused(OperationFailure::InvalidArgument, error)
            })?;
        }
    }

    let action = Action::parse("s3:ListBucket")
        .map_err(|_| MetadataError::Corrupt("invalid internal listing action"))?;

    super::access::authorize(
        transaction,
        identity,
        &AccessRequest {
            bucket: request.bucket.as_str(),
            expected_bucket_id: Some(bucket_id),
            key: None,
            action,
            context: &context,
            expected_owner: request.expected_owner.as_deref(),
        },
    )?;

    Ok(())
}

fn select(
    transaction: &Transaction<'_>,
    bucket_id: [u8; 16],
    cursor: &Cursor,
    upper: Option<&[u8]>,
    limit: usize,
    prefix: &str,
    delimiter: Option<&str>,
) -> Result<Vec<ListedObject>, OperationError> {
    let sql = match (cursor, upper) {
        (Cursor::Inclusive(_), None) => INCLUSIVE,
        (Cursor::Exclusive(_), None) => EXCLUSIVE,
        (Cursor::Inclusive(_), Some(_)) => INCLUSIVE_UPPER,
        (Cursor::Exclusive(_), Some(_)) => EXCLUSIVE_UPPER,
    };

    let mut statement = transaction.prepare_cached(sql)?;
    let limit = i64::try_from(limit).map_err(|_| MetadataError::Corrupt("invalid page limit"))?;

    let mut rows = match upper {
        Some(upper) => statement.query(params![bucket_id, cursor.bytes(), limit, upper])?,
        None => statement.query(params![bucket_id, cursor.bytes(), limit])?,
    };

    let mut output = Vec::new();

    while let Some(row) = rows.next()? {
        let object = decode(row)?;
        let grouped = common_prefix(&object.key, prefix, delimiter).is_some();
        output.push(object);

        // Stop before stepping through descendants. The next query seeks past
        // this group, while ordinary entries share the current statement run.
        if grouped {
            break;
        }
    }

    Ok(output)
}

fn decode(row: &Row<'_>) -> Result<ListedObject, OperationError> {
    let key: Vec<u8> = row.get(0).map_err(decode_error)?;

    let key = ObjectKey::from_bytes(&key)
        .map_err(|_| MetadataError::Corrupt("invalid listed object key"))?;

    let blob_id: Vec<u8> = row.get(1).map_err(decode_error)?;
    let stored_blob_id: Option<Vec<u8>> = row.get(2).map_err(decode_error)?;
    let state: Option<String> = row.get(3).map_err(decode_error)?;

    if blob_id.len() != 16
        || stored_blob_id.as_deref() != Some(blob_id.as_slice())
        || state.as_deref() != Some("ready")
    {
        return Err(MetadataError::Corrupt("missing or non-ready listed payload").into());
    }

    let size = ByteLength::from_stored(row.get(4).map_err(decode_error)?)
        .map_err(|_| MetadataError::Corrupt("invalid listed object length"))?;

    let etag: String = row.get(5).map_err(decode_error)?;
    let md5: Vec<u8> = row.get(7).map_err(decode_error)?;
    let part_count: Option<i64> = row.get(8).map_err(decode_error)?;
    super::objects::validate_etag(&etag, &md5, part_count)?;
    let shard: i64 = row.get(9).map_err(decode_error)?;

    if blob_id.first().copied().map(i64::from) != Some(shard) {
        return Err(MetadataError::Corrupt("listed object blob shard mismatch").into());
    }

    let last_modified = EpochMillis::new(row.get(6).map_err(decode_error)?)
        .map_err(|_| MetadataError::Corrupt("invalid listed object timestamp"))?;

    Ok(ListedObject {
        key: key.as_str().into(),
        size: size.get(),
        etag,
        last_modified,
    })
}

/// Inspect the actual cached production statements without shipping counters.
#[cfg(test)]
pub(super) fn indexed_work(connection: &rusqlite::Connection) -> rusqlite::Result<(i32, i32)> {
    use rusqlite::StatementStatus;

    let mut steps = 0;
    let mut runs = 0;

    for sql in [INCLUSIVE, EXCLUSIVE, INCLUSIVE_UPPER, EXCLUSIVE_UPPER] {
        let statement = connection.prepare_cached(sql)?;
        assert_eq!(statement.get_status(StatementStatus::FullscanStep), 0);
        assert_eq!(statement.get_status(StatementStatus::Sort), 0);
        assert_eq!(statement.get_status(StatementStatus::AutoIndex), 0);
        steps += statement.get_status(StatementStatus::VmStep);
        runs += statement.get_status(StatementStatus::Run);
    }

    Ok((steps, runs))
}
