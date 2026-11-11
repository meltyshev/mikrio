//! Ordered lifecycle rules and their generation change in one current-authority transaction.

use std::collections::BTreeSet;

use rusqlite::{Row, Transaction, params};

use super::{MetadataDatabase, MetadataError, bucket_configuration, decode_error};
use crate::access::BucketAccessRequest;
use crate::lifecycle::{LifecycleConfiguration, LifecycleFilter, LifecycleRule, MAX_RULES};
use crate::object::Authority;
use crate::{OperationError, OperationFailure, StoreId};

pub(crate) mod expiration;
pub(crate) mod uploads;

impl MetadataDatabase {
    pub(crate) fn put_bucket_lifecycle(
        &mut self,
        authority: &Authority,
        request: &BucketAccessRequest,
        configuration: &LifecycleConfiguration,
    ) -> Result<(), OperationError> {
        let generation = StoreId::generate().map_err(MetadataError::Entropy)?;
        let mut rules = configuration.rules().to_vec();

        for rule in &mut rules {
            if rule.id.as_ref().is_none_or(String::is_empty) {
                rule.id = Some(
                    StoreId::generate()
                        .map_err(MetadataError::Entropy)?
                        .to_string(),
                );
            }
        }

        // This also checks generated IDs against caller-supplied IDs before SQL.
        let configuration = LifecycleConfiguration::new(rules).map_err(|error| {
            OperationError::caused(OperationFailure::InvalidArgument, error).uncommitted()
        })?;

        #[cfg(test)]
        let _protocol = crate::protocol_fault::Scope::enter("bucket.lifecycle.put");

        self.write(|transaction| {
            let current = bucket_configuration::current(
                transaction,
                authority,
                request,
                "s3:PutLifecycleConfiguration",
            )?;

            let bucket_id = current.bucket_id.as_slice();

            transaction.execute(
                "DELETE FROM bucket_lifecycle_rules WHERE bucket_id = ?1",
                [bucket_id],
            )?;

            let mut insert = transaction.prepare_cached(
                "INSERT INTO bucket_lifecycle_rules
                 (bucket_id, rule_index, rule_id, is_enabled, filter_kind, filter_prefix,
                  expiration_days, abort_incomplete_multipart_days)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)",
            )?;

            for (index, rule) in configuration.rules().iter().enumerate() {
                let kind = match rule.filter {
                    LifecycleFilter::All => "all",
                    LifecycleFilter::Prefix(_) => "prefix",
                    LifecycleFilter::LegacyPrefix(_) => "legacy_prefix",
                };

                insert.execute(params![
                    bucket_id,
                    i64::try_from(index).map_err(|_| corrupt())?,
                    rule.id,
                    rule.is_enabled,
                    kind,
                    rule.filter.prefix().as_bytes(),
                    rule.expiration_days,
                    rule.abort_incomplete_multipart_days,
                ])?;
            }

            bucket_configuration::changed(transaction.execute(
                "UPDATE buckets SET lifecycle_generation = ?2 WHERE bucket_id = ?1",
                params![bucket_id, generation.as_bytes().as_slice()],
            )?)
        })
    }

    pub(crate) fn get_bucket_lifecycle(
        &mut self,
        authority: &Authority,
        request: &BucketAccessRequest,
    ) -> Result<Option<LifecycleConfiguration>, OperationError> {
        self.read(|transaction| {
            let current = bucket_configuration::current(
                transaction,
                authority,
                request,
                "s3:GetLifecycleConfiguration",
            )?;

            configuration(transaction, &current.bucket_id)
        })
    }

    pub(crate) fn delete_bucket_lifecycle(
        &mut self,
        authority: &Authority,
        request: &BucketAccessRequest,
    ) -> Result<(), OperationError> {
        #[cfg(test)]
        let _protocol = crate::protocol_fault::Scope::enter("bucket.lifecycle.delete");

        self.write(|transaction| {
            let current = bucket_configuration::current(
                transaction,
                authority,
                request,
                "s3:PutLifecycleConfiguration",
            )?;

            let bucket_id = current.bucket_id.as_slice();

            transaction.execute(
                "DELETE FROM bucket_lifecycle_rules WHERE bucket_id = ?1",
                [bucket_id],
            )?;

            bucket_configuration::changed(transaction.execute(
                "UPDATE buckets SET lifecycle_generation = NULL WHERE bucket_id = ?1",
                [bucket_id],
            )?)
        })
    }
}

// One validated configuration is retained at a time, even for sixteen distinct
// buckets. The raw page stays bounded and keeps its original row-ID ordering.
fn filter_candidates<T>(
    transaction: &Transaction<'_>,
    page: &mut super::sweep::Page<Result<Option<T>, OperationError>>,
    bucket_id: impl Fn(&T) -> [u8; 16],
    eligible: impl Fn(&LifecycleConfiguration, &T) -> Result<bool, OperationError>,
) {
    let buckets: BTreeSet<_> = page
        .candidates
        .iter()
        .filter_map(|(_, candidate)| candidate.as_ref().ok()?.as_ref().map(&bucket_id))
        .collect();

    for current_bucket in buckets {
        let mut failure = None;

        let rules = match configuration(transaction, &current_bucket) {
            Ok(current) => current,
            Err(error) => {
                failure = Some(error);
                None
            }
        };

        for (_, candidate) in &mut page.candidates {
            let Ok(Some(selected)) = candidate else {
                continue;
            };

            if bucket_id(selected) != current_bucket {
                continue;
            }

            if let Some(error) = failure.take() {
                // Report the bucket failure once; later passes retry it while
                // this pass can still progress through unrelated buckets.
                *candidate = Err(error);
                continue;
            }

            let Some(rules) = &rules else {
                *candidate = Ok(None);
                continue;
            };

            match eligible(rules, selected) {
                Ok(true) => {}
                Ok(false) => *candidate = Ok(None),
                Err(error) => *candidate = Err(error),
            }
        }
    }
}

fn configuration(
    transaction: &Transaction<'_>,
    bucket_id: &[u8; 16],
) -> Result<Option<LifecycleConfiguration>, OperationError> {
    let generation: Option<Vec<u8>> = transaction
        .query_row(
            "SELECT lifecycle_generation FROM buckets WHERE bucket_id = ?1",
            [bucket_id.as_slice()],
            |row| row.get(0),
        )
        .map_err(decode_error)?;

    if generation.as_ref().is_some_and(|value| value.len() != 16) {
        return Err(corrupt());
    }

    let mut statement = transaction.prepare_cached(
        "SELECT rule_index, rule_id, is_enabled, filter_kind, filter_prefix,
                expiration_days, abort_incomplete_multipart_days
         FROM bucket_lifecycle_rules WHERE bucket_id = ?1 ORDER BY rule_index LIMIT 1001",
    )?;

    let mut rows = statement.query([bucket_id.as_slice()])?;
    let mut rules = Vec::new();

    while let Some(row) = rows.next()? {
        if generation.is_none() || rules.len() == MAX_RULES {
            return Err(corrupt());
        }

        let index: i64 = row.get(0).map_err(decode_error)?;

        if index != i64::try_from(rules.len()).map_err(|_| corrupt())? {
            return Err(corrupt());
        }

        rules.push(decode_rule(row)?);
    }

    if generation.is_none() {
        return Ok(None);
    }

    LifecycleConfiguration::new(rules)
        .map(Some)
        .map_err(|_| corrupt())
}

fn decode_rule(row: &Row<'_>) -> Result<LifecycleRule, OperationError> {
    let id: String = row.get(1).map_err(decode_error)?;
    let enabled: i64 = row.get(2).map_err(decode_error)?;
    let kind: String = row.get(3).map_err(decode_error)?;
    let prefix: Vec<u8> = row.get(4).map_err(decode_error)?;
    let prefix = String::from_utf8(prefix).map_err(|_| corrupt())?;

    if id.is_empty() || !matches!(enabled, 0 | 1) {
        return Err(corrupt());
    }

    let filter = match kind.as_str() {
        "all" if prefix.is_empty() => LifecycleFilter::All,
        "prefix" => LifecycleFilter::Prefix(prefix),
        "legacy_prefix" => LifecycleFilter::LegacyPrefix(prefix),
        _ => return Err(corrupt()),
    };

    Ok(LifecycleRule {
        id: Some(id),
        is_enabled: enabled == 1,
        filter,
        expiration_days: row.get(5).map_err(decode_error)?,
        abort_incomplete_multipart_days: row.get(6).map_err(decode_error)?,
    })
}

fn corrupt() -> OperationError {
    MetadataError::Corrupt("invalid stored lifecycle configuration").into()
}
