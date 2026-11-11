//! Live namespace queries and bounded logical retirement.

use rusqlite::{OptionalExtension, Transaction, params};

use super::{MetadataDatabase, MetadataError};
use crate::object::{Authority, BucketName};
use crate::{EpochMillis, OperationError, OperationFailure, StoreId};

impl MetadataDatabase {
    pub(crate) fn create_bucket(&mut self, bucket: &BucketName) -> Result<(), OperationError> {
        self.create_bucket_authorized(&Authority::Native, bucket)
    }

    pub(crate) fn create_bucket_authorized(
        &mut self,
        authority: &Authority,
        bucket: &BucketName,
    ) -> Result<(), OperationError> {
        let bucket_id = StoreId::generate().map_err(MetadataError::Entropy)?;

        let created_at = super::now()?;

        #[cfg(test)]
        let _protocol = crate::protocol_fault::Scope::enter("bucket.create");

        self.write(|transaction| {
            let existing = find_bucket(transaction, bucket)?;

            if let Authority::Http(identity) = authority {
                if existing.is_some() {
                    authorize(transaction, identity, bucket, "s3:CreateBucket", None)?;
                } else {
                    super::access::authorize_creation(transaction, identity, bucket.as_str())?;
                }
            }

            if existing.is_some() {
                let region = region(transaction)?;

                if region == "us-east-1" {
                    return Ok(());
                }

                return Err(OperationError::new(OperationFailure::BucketAlreadyOwnedByYou));
            }

            transaction.execute(
                "INSERT INTO buckets (bucket_id, name, block_public_acls, ignore_public_acls, block_public_policy, restrict_public_buckets, created_at_ms) VALUES (?1, ?2, 1, 1, 1, 1, ?3)",
                params![bucket_id.as_bytes().as_slice(), bucket.as_str(), created_at.get()],
            )?;

            Ok(())
        })
    }

    pub(crate) fn head_bucket(
        &mut self,
        bucket: &BucketName,
        expected_owner: Option<&str>,
    ) -> Result<String, OperationError> {
        self.head_bucket_authorized(&Authority::Native, bucket, expected_owner, "s3:ListBucket")
    }

    pub(crate) fn head_bucket_authorized(
        &mut self,
        authority: &Authority,
        bucket: &BucketName,
        expected_owner: Option<&str>,
        action: &str,
    ) -> Result<String, OperationError> {
        self.read(|transaction| {
            require_bucket(transaction, bucket, expected_owner)?;

            if let Authority::Http(identity) = authority {
                authorize(transaction, identity, bucket, action, expected_owner)?;
            }

            region(transaction)
        })
    }

    pub(crate) fn delete_bucket(
        &mut self,
        bucket: &BucketName,
        expected_owner: Option<&str>,
    ) -> Result<(), OperationError> {
        self.delete_bucket_authorized(&Authority::Native, bucket, expected_owner)
    }

    pub(crate) fn delete_bucket_authorized(
        &mut self,
        authority: &Authority,
        bucket: &BucketName,
        expected_owner: Option<&str>,
    ) -> Result<(), OperationError> {
        #[cfg(test)]
        let _protocol = crate::protocol_fault::Scope::enter("bucket.delete");

        self.write(|transaction| {
            let bucket_id = require_bucket(transaction, bucket, expected_owner)?;

            if let Authority::Http(identity) = authority {
                authorize(
                    transaction,
                    identity,
                    bucket,
                    "s3:DeleteBucket",
                    expected_owner,
                )?;
            }

            if transaction
                .prepare_cached("SELECT 1 FROM objects WHERE bucket_id = ?1 LIMIT 1")?
                .exists([bucket_id.as_slice()])?
            {
                return Err(OperationError::new(OperationFailure::BucketNotEmpty));
            }

            let changed = transaction.execute(
                "UPDATE buckets SET name = NULL WHERE bucket_id = ?1 AND name = ?2",
                params![bucket_id.as_slice(), bucket.as_str()],
            )?;

            if changed != 1 {
                return Err(MetadataError::Corrupt("bucket retirement lost its identity").into());
            }

            Ok(())
        })
    }
}

pub(super) fn authorize(
    transaction: &Transaction<'_>,
    identity: &crate::access::HttpIdentity,
    bucket: &BucketName,
    action: &str,
    expected_owner: Option<&str>,
) -> Result<(), OperationError> {
    let action = crate::access::Action::parse(action)
        .map_err(|_| MetadataError::Corrupt("invalid internal bucket action"))?;

    super::access::authorize(
        transaction,
        identity,
        &crate::AccessRequest {
            bucket: bucket.as_str(),
            expected_bucket_id: None,
            key: None,
            action,
            context: &crate::access::Context::default(),
            expected_owner,
        },
    )?;

    Ok(())
}

pub(super) fn require_bucket(
    transaction: &Transaction<'_>,
    bucket: &BucketName,
    expected_owner: Option<&str>,
) -> Result<[u8; 16], OperationError> {
    let bucket_id = find_bucket(transaction, bucket)?
        .ok_or_else(|| OperationError::new(OperationFailure::NoSuchBucket))?;

    let owner: String = transaction
        .query_row(
            "SELECT owner_account_id FROM store_metadata WHERE singleton_id = 1",
            [],
            |row| row.get(0),
        )
        .map_err(super::decode_error)?;

    crate::access::validate_account_id(&owner)
        .map_err(|_| MetadataError::Corrupt("invalid store owner"))?;

    if expected_owner.is_some_and(|expected| expected != owner) {
        return Err(OperationError::new(OperationFailure::AccessDenied));
    }

    Ok(bucket_id)
}

fn find_bucket(
    transaction: &Transaction<'_>,
    bucket: &BucketName,
) -> Result<Option<[u8; 16]>, OperationError> {
    let row = transaction
        .prepare_cached("SELECT bucket_id, created_at_ms FROM buckets WHERE name = ?1")?
        .query_row([bucket.as_str()], |row| {
            Ok((row.get::<_, Vec<u8>>(0)?, row.get::<_, i64>(1)?))
        })
        .optional()
        .map_err(super::decode_error)?;

    let Some((bucket_id, created_at)) = row else {
        return Ok(None);
    };

    EpochMillis::new(created_at).map_err(|_| MetadataError::Corrupt("invalid bucket timestamp"))?;

    let bucket_id = bucket_id
        .try_into()
        .map_err(|_| MetadataError::Corrupt("invalid bucket identity"))?;

    Ok(Some(bucket_id))
}

pub(super) fn region(transaction: &Transaction<'_>) -> Result<String, OperationError> {
    let region: String = transaction
        .query_row(
            "SELECT region FROM store_metadata WHERE singleton_id = 1",
            [],
            |row| row.get(0),
        )
        .map_err(super::decode_error)?;

    if !super::valid_region(&region) {
        return Err(MetadataError::Corrupt("invalid store region").into());
    }

    Ok(region)
}
