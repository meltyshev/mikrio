//! Whole bucket configuration changes use the current authority and one transaction.

use rusqlite::{Transaction, params};

use super::{MetadataDatabase, MetadataError, access, buckets, decode_error};
use crate::access::{BucketAccessRequest, GetBucketPolicyOutput, OwnerInfo, Policy, PolicyKind};
use crate::object::Authority;
use crate::{OperationError, OperationFailure, PublicAccessBlock, StoreId};

impl MetadataDatabase {
    pub(crate) fn put_bucket_policy(
        &mut self,
        authority: &Authority,
        request: &BucketAccessRequest,
        document: &str,
    ) -> Result<(), OperationError> {
        let generation = StoreId::generate().map_err(MetadataError::Entropy)?;

        #[cfg(test)]
        let _protocol = crate::protocol_fault::Scope::enter("bucket.policy.put");

        self.write(|transaction| {
            let current = current(transaction, authority, request, "s3:PutBucketPolicy")?;

            let policy = Policy::parse(
                document,
                PolicyKind::Bucket {
                    name: request.bucket.as_str(),
                    owner_account_id: current.owner.account_id(),
                },
            )
            .map_err(|error| OperationError::caused(OperationFailure::InvalidArgument, error))?;

            for principal_id in policy.referenced_principals() {
                super::admin::principal(transaction, principal_id).map_err(|error| {
                    if error.failure() == OperationFailure::NoSuchPrincipal {
                        return OperationError::new(OperationFailure::InvalidArgument);
                    }

                    error
                })?;
            }

            if let Some(flags) = current.public_access_block {
                flags.check_policy_install(&policy)?;
            }

            changed(transaction.execute(
                "UPDATE buckets SET policy_generation = ?2, policy_json = ?3 WHERE bucket_id = ?1",
                params![
                    current.bucket_id.as_slice(),
                    generation.as_bytes().as_slice(),
                    policy.document()
                ],
            )?)
        })
    }

    pub(crate) fn get_bucket_policy(
        &mut self,
        authority: &Authority,
        request: &BucketAccessRequest,
    ) -> Result<GetBucketPolicyOutput, OperationError> {
        self.read(|transaction| {
            let current = current(transaction, authority, request, "s3:GetBucketPolicy")?;

            let policy = current
                .policy
                .ok_or_else(|| OperationError::new(OperationFailure::NoSuchBucketPolicy))?;

            Ok(GetBucketPolicyOutput {
                document: policy.document().to_owned(),
            })
        })
    }

    pub(crate) fn delete_bucket_policy(
        &mut self,
        authority: &Authority,
        request: &BucketAccessRequest,
    ) -> Result<(), OperationError> {
        #[cfg(test)]
        let _protocol = crate::protocol_fault::Scope::enter("bucket.policy.delete");

        self.write(|transaction| {
            let current = current(transaction, authority, request, "s3:DeleteBucketPolicy")?;

            changed(transaction.execute(
                "UPDATE buckets SET policy_generation = NULL, policy_json = NULL WHERE bucket_id = ?1",
                [current.bucket_id.as_slice()],
            )?)
        })
    }

    pub(crate) fn get_bucket_policy_status(
        &mut self,
        authority: &Authority,
        request: &BucketAccessRequest,
    ) -> Result<bool, OperationError> {
        self.read(|transaction| {
            let current = current(transaction, authority, request, "s3:GetBucketPolicyStatus")?;

            let policy = current
                .policy
                .ok_or_else(|| OperationError::new(OperationFailure::NoSuchBucketPolicy))?;

            Ok(policy.is_public())
        })
    }

    pub(crate) fn put_public_access_block(
        &mut self,
        authority: &Authority,
        request: &BucketAccessRequest,
        configuration: PublicAccessBlock,
    ) -> Result<(), OperationError> {
        #[cfg(test)]
        let _protocol = crate::protocol_fault::Scope::enter("bucket.public_access_block.put");

        self.write(|transaction| {
            let current = current(
                transaction,
                authority,
                request,
                "s3:PutBucketPublicAccessBlock",
            )?;

            changed(transaction.execute(
                "UPDATE buckets SET block_public_acls = ?2, ignore_public_acls = ?3,
                 block_public_policy = ?4, restrict_public_buckets = ?5 WHERE bucket_id = ?1",
                params![
                    current.bucket_id.as_slice(),
                    configuration.block_public_acls,
                    configuration.ignore_public_acls,
                    configuration.block_public_policy,
                    configuration.restrict_public_buckets
                ],
            )?)
        })
    }

    pub(crate) fn get_public_access_block(
        &mut self,
        authority: &Authority,
        request: &BucketAccessRequest,
    ) -> Result<PublicAccessBlock, OperationError> {
        self.read(|transaction| {
            let current = current(
                transaction,
                authority,
                request,
                "s3:GetBucketPublicAccessBlock",
            )?;

            current.public_access_block.ok_or_else(|| {
                OperationError::new(OperationFailure::NoSuchPublicAccessBlockConfiguration)
            })
        })
    }

    pub(crate) fn delete_public_access_block(
        &mut self,
        authority: &Authority,
        request: &BucketAccessRequest,
    ) -> Result<(), OperationError> {
        #[cfg(test)]
        let _protocol = crate::protocol_fault::Scope::enter("bucket.public_access_block.delete");

        self.write(|transaction| {
            let current = current(
                transaction,
                authority,
                request,
                "s3:PutBucketPublicAccessBlock",
            )?;

            changed(transaction.execute(
                "UPDATE buckets SET block_public_acls = NULL, ignore_public_acls = NULL,
                 block_public_policy = NULL, restrict_public_buckets = NULL WHERE bucket_id = ?1",
                [current.bucket_id.as_slice()],
            )?)
        })
    }

    pub(crate) fn put_bucket_ownership_controls(
        &mut self,
        authority: &Authority,
        request: &BucketAccessRequest,
    ) -> Result<(), OperationError> {
        #[cfg(test)]
        let _protocol = crate::protocol_fault::Scope::enter("bucket.ownership.put");

        // The fixed owner-enforced mode has no mutable row. This still settles
        // a write transaction so namespace/access changes cannot race success.
        self.write(|transaction| {
            current(
                transaction,
                authority,
                request,
                "s3:PutBucketOwnershipControls",
            )?;

            Ok(())
        })
    }

    pub(crate) fn get_bucket_ownership_controls(
        &mut self,
        authority: &Authority,
        request: &BucketAccessRequest,
    ) -> Result<(), OperationError> {
        self.read(|transaction| {
            current(
                transaction,
                authority,
                request,
                "s3:GetBucketOwnershipControls",
            )?;

            Ok(())
        })
    }

    pub(crate) fn get_bucket_acl(
        &mut self,
        authority: &Authority,
        request: &BucketAccessRequest,
    ) -> Result<OwnerInfo, OperationError> {
        self.read(|transaction| {
            Ok(current(transaction, authority, request, "s3:GetBucketAcl")?.owner)
        })
    }
}

pub(super) struct Current {
    pub(super) bucket_id: [u8; 16],
    owner: OwnerInfo,
    policy: Option<Policy>,
    public_access_block: Option<PublicAccessBlock>,
}

pub(super) fn current(
    transaction: &Transaction<'_>,
    authority: &Authority,
    request: &BucketAccessRequest,
    action: &str,
) -> Result<Current, OperationError> {
    let bucket_id = buckets::require_bucket(
        transaction,
        &request.bucket,
        request.expected_owner.as_deref(),
    )?;

    let owner = super::admin::owner_info(transaction)?;

    let (generation, document, flags) = transaction
        .query_row(
            "SELECT policy_generation, policy_json, block_public_acls, ignore_public_acls,
         block_public_policy, restrict_public_buckets FROM buckets WHERE bucket_id = ?1",
            [bucket_id.as_slice()],
            |row| {
                Ok((
                    row.get(0)?,
                    row.get(1)?,
                    [row.get(2)?, row.get(3)?, row.get(4)?, row.get(5)?],
                ))
            },
        )
        .map_err(decode_error)?;

    let policy = access::policy(
        generation,
        document,
        PolicyKind::Bucket {
            name: request.bucket.as_str(),
            owner_account_id: owner.account_id(),
        },
    )?;

    let public_access_block = access::public_access_block(flags)?;

    if let Authority::Http(identity) = authority {
        buckets::authorize(
            transaction,
            identity,
            &request.bucket,
            action,
            request.expected_owner.as_deref(),
        )?;
    }

    Ok(Current {
        bucket_id,
        owner,
        policy,
        public_access_block,
    })
}

pub(super) fn changed(rows: usize) -> Result<(), OperationError> {
    if rows != 1 {
        return Err(MetadataError::Corrupt("bucket configuration lost its live identity").into());
    }

    Ok(())
}
