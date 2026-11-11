//! Account-wide bucket selection in one short, authorized metadata snapshot.

use rusqlite::{Row, params};

use super::{MetadataDatabase, MetadataError, decode_error};
use crate::object::{Authority, BucketName, BucketPage, ListBucketsRequest, ListedBucket};
use crate::{EpochMillis, OperationError, OperationFailure};

mod token;
use token::Scope;

// The live-name partial index supplies both the range and the order. A negative
// LIMIT is used only by the explicitly unpaginated HTTP request. Its detached
// metadata is materialized before serialization; no SQL survives network writes.
const FIRST: &str = "SELECT bucket_id, name, created_at_ms FROM buckets
    WHERE name IS NOT NULL AND name >= ?1 ORDER BY name LIMIT ?2";
const NEXT: &str = "SELECT bucket_id, name, created_at_ms FROM buckets
    WHERE name IS NOT NULL AND name > ?1 ORDER BY name LIMIT ?2";
const FIRST_PREFIX: &str = "SELECT bucket_id, name, created_at_ms FROM buckets
    WHERE name IS NOT NULL AND name >= ?1 AND name < ?3 ORDER BY name LIMIT ?2";
const NEXT_PREFIX: &str = "SELECT bucket_id, name, created_at_ms FROM buckets
    WHERE name IS NOT NULL AND name > ?1 AND name < ?3 ORDER BY name LIMIT ?2";

impl MetadataDatabase {
    pub(crate) fn list_buckets(
        &mut self,
        authority: &Authority,
        request: &ListBucketsRequest,
    ) -> Result<BucketPage, OperationError> {
        request.validate()?;

        self.read(|transaction| {
            let owner = super::admin::owner_info(transaction)?;

            if let Authority::Http(identity) = authority {
                super::access::authorize_bucket_listing(transaction, identity, owner.account_id())?;
            }

            let region = super::buckets::region(transaction)?;

            if request
                .bucket_region
                .as_ref()
                .is_some_and(|filter| filter != &region)
            {
                return Err(OperationError::new(OperationFailure::InvalidArgument));
            }

            let scope = Scope::load(transaction, request)?;
            let cursor = scope.decode(request.continuation_token.as_deref())?;
            let prefix = request.prefix.as_deref().unwrap_or_default();

            let mut output = BucketPage {
                buckets: Vec::new(),
                continuation_token: None,
                bucket_region: region,
                canonical_owner_id: owner.canonical_id().to_owned(),
            };

            // Literal prefixes need not be valid complete bucket names. No
            // valid stored ASCII name can begin with any of these other bytes.
            if prefix.len() > 63 {
                return Ok(output);
            }

            if !prefix.bytes().all(|byte| {
                byte.is_ascii_lowercase() || byte.is_ascii_digit() || matches!(byte, b'.' | b'-')
            }) {
                return Ok(output);
            }

            let upper = prefix_upper(prefix);

            let sql = match (cursor.is_some(), upper.is_some()) {
                (false, false) => FIRST,
                (true, false) => NEXT,
                (false, true) => FIRST_PREFIX,
                (true, true) => NEXT_PREFIX,
            };

            let limit = request.max_buckets.map(|size| i64::from(size.get()));
            let fetch = limit.map_or(-1, |limit| limit + 1);
            let lower = cursor.as_deref().unwrap_or(prefix);
            let mut statement = transaction.prepare_cached(sql)?;

            let mut rows = match &upper {
                Some(upper) => statement.query(params![lower, fetch, upper])?,
                None => statement.query(params![lower, fetch])?,
            };

            while let Some(row) = rows.next()? {
                // Decode lookahead too: a corrupt next row cannot silently
                // become a short successful page or an unusable token.
                let bucket = decode(row)?;

                if limit.is_some_and(|limit| i64::try_from(output.buckets.len()) == Ok(limit)) {
                    let last = output
                        .buckets
                        .last()
                        .ok_or(MetadataError::Corrupt("empty bucket page"))?;

                    output.continuation_token = Some(scope.encode(&last.name)?);
                    break;
                }

                output.buckets.push(bucket);
            }

            Ok(output)
        })
    }
}

fn prefix_upper(prefix: &str) -> Option<String> {
    let mut bytes = prefix.as_bytes().to_vec();
    *bytes.last_mut()? += 1;
    // The caller has established ASCII and the largest allowed byte is 'z'.
    String::from_utf8(bytes).ok()
}

fn decode(row: &Row<'_>) -> Result<ListedBucket, OperationError> {
    let identity: Vec<u8> = row.get(0).map_err(decode_error)?;

    if identity.len() != 16 {
        return Err(MetadataError::Corrupt("invalid listed bucket identity").into());
    }

    let name: String = row.get(1).map_err(decode_error)?;
    BucketName::parse(&name).map_err(|_| MetadataError::Corrupt("invalid listed bucket name"))?;

    let creation_date = EpochMillis::new(row.get(2).map_err(decode_error)?)
        .map_err(|_| MetadataError::Corrupt("invalid listed bucket timestamp"))?;

    Ok(ListedBucket {
        name,
        creation_date,
    })
}

#[cfg(test)]
pub(super) fn indexed_work(connection: &rusqlite::Connection) -> rusqlite::Result<i32> {
    use rusqlite::StatementStatus;

    let mut steps = 0;

    for sql in [FIRST, NEXT, FIRST_PREFIX, NEXT_PREFIX] {
        let statement = connection.prepare_cached(sql)?;
        assert_eq!(statement.get_status(StatementStatus::Sort), 0);
        assert_eq!(statement.get_status(StatementStatus::AutoIndex), 0);
        steps += statement.get_status(StatementStatus::VmStep);
    }

    Ok(steps)
}
