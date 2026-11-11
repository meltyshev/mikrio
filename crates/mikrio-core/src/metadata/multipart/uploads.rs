//! Bounded upload namespace traversal with paired markers and delimiter seeks.

use rusqlite::{Row, Transaction, params};

use super::super::namespace::{common_prefix, successor};
use super::super::{MetadataDatabase, MetadataError, decode_error};
use crate::access::{Action, Context, OwnerInfo};
use crate::multipart::{ListUploadsRequest, ListedUpload, UploadMarker, UploadPage};
use crate::object::{Authority, ObjectKey};
use crate::{AccessRequest, EpochMillis, OperationError, UploadId};

const INCLUSIVE: &str = "SELECT state, initiated_at_ms, checksum_algorithm, checksum_type,
    completion_claim_id, completion_blob_id, completion_parts_digest, initiator_id, upload_id, key
    FROM multipart_uploads WHERE bucket_id = ?1 AND key >= ?2
    ORDER BY key, upload_id LIMIT ?4";
const EXCLUSIVE: &str = "SELECT state, initiated_at_ms, checksum_algorithm, checksum_type,
    completion_claim_id, completion_blob_id, completion_parts_digest, initiator_id, upload_id, key
    FROM multipart_uploads WHERE bucket_id = ?1 AND key > ?2
    ORDER BY key, upload_id LIMIT ?4";
const PAIRED: &str = "SELECT state, initiated_at_ms, checksum_algorithm, checksum_type,
    completion_claim_id, completion_blob_id, completion_parts_digest, initiator_id, upload_id, key
    FROM multipart_uploads WHERE bucket_id = ?1 AND (key, upload_id) > (?2, ?3)
    ORDER BY key, upload_id LIMIT ?4";
const INCLUSIVE_UPPER: &str = "SELECT state, initiated_at_ms, checksum_algorithm, checksum_type,
    completion_claim_id, completion_blob_id, completion_parts_digest, initiator_id, upload_id, key
    FROM multipart_uploads WHERE bucket_id = ?1 AND key >= ?2 AND key < ?5
    ORDER BY key, upload_id LIMIT ?4";
const EXCLUSIVE_UPPER: &str = "SELECT state, initiated_at_ms, checksum_algorithm, checksum_type,
    completion_claim_id, completion_blob_id, completion_parts_digest, initiator_id, upload_id, key
    FROM multipart_uploads WHERE bucket_id = ?1 AND key > ?2 AND key < ?5
    ORDER BY key, upload_id LIMIT ?4";
const PAIRED_UPPER: &str = "SELECT state, initiated_at_ms, checksum_algorithm, checksum_type,
    completion_claim_id, completion_blob_id, completion_parts_digest, initiator_id, upload_id, key
    FROM multipart_uploads WHERE bucket_id = ?1 AND (key, upload_id) > (?2, ?3) AND key < ?5
    ORDER BY key, upload_id LIMIT ?4";

enum Cursor {
    Inclusive(Vec<u8>),
    Exclusive(Vec<u8>),
    Paired(Vec<u8>, String),
}

impl Cursor {
    fn key(&self) -> &[u8] {
        match self {
            Self::Inclusive(key) | Self::Exclusive(key) | Self::Paired(key, _) => key,
        }
    }

    fn upload(&self) -> Option<&str> {
        match self {
            Self::Paired(_, upload) => Some(upload),
            _ => None,
        }
    }
}

impl MetadataDatabase {
    pub(crate) fn list_multipart_uploads(
        &mut self,
        authority: &Authority,
        request: &ListUploadsRequest,
    ) -> Result<UploadPage, OperationError> {
        self.read(|transaction| {
            let bucket_id = authorize(transaction, authority, request)?;
            let owner = super::super::admin::owner_info(transaction)?;
            let prefix = request.prefix.as_deref().unwrap_or_default();
            let upper = successor(prefix.as_bytes());
            let mut cursor = initial_cursor(request, prefix);

            let delimiter = request
                .delimiter
                .as_deref()
                .filter(|value| !value.is_empty());

            let mut remaining = request.max_uploads.get();
            let mut last = None;

            let mut page = UploadPage {
                uploads: Vec::new(),
                common_prefixes: Vec::new(),
                next_marker: None,
                owner,
            };

            loop {
                let candidates = select(
                    transaction,
                    bucket_id,
                    &cursor,
                    upper.as_deref(),
                    remaining + 1,
                    &page.owner,
                    prefix,
                    delimiter,
                )?;

                if candidates.is_empty() {
                    return Ok(page);
                }

                for upload in candidates {
                    let group = common_prefix(&upload.key, prefix, delimiter);

                    let excluded = group.filter(|group| {
                        request
                            .key_marker
                            .as_deref()
                            .is_some_and(|marker| *group <= marker)
                    });

                    if let Some(group) = excluded {
                        cursor =
                            Cursor::Inclusive(successor(group.as_bytes()).ok_or(
                                MetadataError::Corrupt("invalid upload delimiter boundary"),
                            )?);

                        continue;
                    }

                    if remaining == 0 {
                        page.next_marker = last;
                        return Ok(page);
                    }

                    if let Some(group) = group {
                        cursor =
                            Cursor::Inclusive(successor(group.as_bytes()).ok_or(
                                MetadataError::Corrupt("invalid upload delimiter boundary"),
                            )?);

                        last = Some(UploadMarker {
                            key: group.into(),
                            upload_id: None,
                        });

                        page.common_prefixes.push(group.into());
                    } else {
                        let upload_id = upload.upload_id.to_string();
                        cursor = Cursor::Paired(upload.key.as_bytes().to_vec(), upload_id.clone());

                        last = Some(UploadMarker {
                            key: upload.key.clone(),
                            upload_id: Some(upload_id),
                        });

                        page.uploads.push(upload);
                    }

                    remaining -= 1;
                }
            }
        })
    }
}

fn initial_cursor(request: &ListUploadsRequest, prefix: &str) -> Cursor {
    let Some(key) = request.key_marker.as_deref().filter(|key| *key >= prefix) else {
        return Cursor::Inclusive(prefix.as_bytes().to_vec());
    };

    match &request.upload_id_marker {
        Some(upload) => Cursor::Paired(key.as_bytes().to_vec(), upload.clone()),
        None => Cursor::Exclusive(key.as_bytes().to_vec()),
    }
}

fn authorize(
    transaction: &Transaction<'_>,
    authority: &Authority,
    request: &ListUploadsRequest,
) -> Result<[u8; 16], OperationError> {
    let bucket_id = super::super::objects::access::bucket(
        transaction,
        authority,
        &request.bucket,
        request.expected_owner.as_deref(),
    )?;

    if let Authority::Http(identity) = authority {
        // This action does not carry ListBucket's prefix/delimiter policy keys.
        super::super::access::authorize(
            transaction,
            identity,
            &AccessRequest {
                bucket: request.bucket.as_str(),
                expected_bucket_id: Some(bucket_id),
                key: None,
                action: Action::parse("s3:ListBucketMultipartUploads")
                    .map_err(|_| MetadataError::Corrupt("invalid upload-list action"))?,
                context: &Context::default(),
                expected_owner: request.expected_owner.as_deref(),
            },
        )?;
    }

    Ok(bucket_id)
}

fn select(
    transaction: &Transaction<'_>,
    bucket_id: [u8; 16],
    cursor: &Cursor,
    upper: Option<&[u8]>,
    limit: u32,
    owner: &OwnerInfo,
    prefix: &str,
    delimiter: Option<&str>,
) -> Result<Vec<ListedUpload>, OperationError> {
    let sql = match (cursor, upper) {
        (Cursor::Inclusive(_), None) => INCLUSIVE,
        (Cursor::Exclusive(_), None) => EXCLUSIVE,
        (Cursor::Paired(..), None) => PAIRED,
        (Cursor::Inclusive(_), Some(_)) => INCLUSIVE_UPPER,
        (Cursor::Exclusive(_), Some(_)) => EXCLUSIVE_UPPER,
        (Cursor::Paired(..), Some(_)) => PAIRED_UPPER,
    };

    let mut statement = transaction.prepare_cached(sql)?;

    let mut rows = match upper {
        Some(upper) => statement.query(params![
            bucket_id.as_slice(),
            cursor.key(),
            cursor.upload(),
            i64::from(limit),
            upper
        ])?,
        None => statement.query(params![
            bucket_id.as_slice(),
            cursor.key(),
            cursor.upload(),
            i64::from(limit)
        ])?,
    };

    let mut uploads = Vec::new();

    while let Some(row) = rows.next()? {
        let upload = decode(row, owner)?;
        let grouped = common_prefix(&upload.key, prefix, delimiter).is_some();
        uploads.push(upload);

        // Resume at the group's successor without reading its descendants.
        if grouped {
            break;
        }
    }

    Ok(uploads)
}

fn decode(row: &Row<'_>, owner: &OwnerInfo) -> Result<ListedUpload, OperationError> {
    let time = EpochMillis::new(row.get(1).map_err(decode_error)?)
        .map_err(|_| MetadataError::Corrupt("invalid listed upload timestamp"))?;

    let encoded: String = row.get(8).map_err(decode_error)?;

    let upload_id = UploadId::parse(&encoded, time)
        .map_err(|_| MetadataError::Corrupt("invalid listed upload identity"))?;

    let key: Vec<u8> = row.get(9).map_err(decode_error)?;

    let key = ObjectKey::from_bytes(&key)
        .map_err(|_| MetadataError::Corrupt("invalid listed upload key"))?;

    let selected = super::upload::decode(row, &upload_id)?;
    super::access::validate_initiator(&selected.initiator, owner)?;

    Ok(ListedUpload {
        key: key.as_str().into(),
        upload_id,
        checksum: selected.checksum,
        initiator: selected.initiator,
    })
}

/// Observe actual cached traversal statements without adding production counters.
#[cfg(test)]
pub(in crate::metadata) fn indexed_work(
    connection: &rusqlite::Connection,
) -> rusqlite::Result<(i32, i32)> {
    use rusqlite::StatementStatus;
    let mut steps = 0;
    let mut runs = 0;

    for sql in [
        INCLUSIVE,
        EXCLUSIVE,
        PAIRED,
        INCLUSIVE_UPPER,
        EXCLUSIVE_UPPER,
        PAIRED_UPPER,
    ] {
        let statement = connection.prepare_cached(sql)?;
        assert_eq!(statement.get_status(StatementStatus::FullscanStep), 0);
        assert_eq!(statement.get_status(StatementStatus::Sort), 0);
        assert_eq!(statement.get_status(StatementStatus::AutoIndex), 0);
        steps += statement.get_status(StatementStatus::VmStep);
        runs += statement.get_status(StatementStatus::Run);
    }

    Ok((steps, runs))
}
