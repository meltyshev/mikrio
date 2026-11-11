//! Object ACL reads select the current mapping and owner in one snapshot.

use super::GetObjectAclOutput;
use crate::{ObjectRef, StorageError, Store};

/// Validated object identity and optional bucket-owner assertion.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GetObjectAclInput {
    object: ObjectRef,
    expected_bucket_owner: Option<String>,
}

impl GetObjectAclInput {
    /// Start a builder with the required literal bucket and key.
    #[must_use]
    pub fn builder(bucket: impl Into<String>, key: impl Into<String>) -> GetObjectAclInputBuilder {
        GetObjectAclInputBuilder {
            bucket: bucket.into(),
            key: key.into(),
            expected_bucket_owner: None,
        }
    }

    /// Exact validated bucket name.
    #[must_use]
    pub fn bucket(&self) -> &str {
        self.object.bucket()
    }

    /// Exact validated key, without path normalization or URL encoding.
    #[must_use]
    pub fn key(&self) -> &str {
        self.object.key()
    }

    /// Optional account assertion; it grants no permission.
    #[must_use]
    pub fn expected_bucket_owner(&self) -> Option<&str> {
        self.expected_bucket_owner.as_deref()
    }
}

/// Builder for [`GetObjectAclInput`]. Validation occurs in [`Self::build`].
#[derive(Debug, Clone)]
pub struct GetObjectAclInputBuilder {
    bucket: String,
    key: String,
    expected_bucket_owner: Option<String>,
}

impl GetObjectAclInputBuilder {
    /// Require the bucket to belong to this twelve-digit account.
    #[must_use]
    pub fn expected_bucket_owner(mut self, account: impl Into<String>) -> Self {
        self.expected_bucket_owner = Some(account.into());
        self
    }

    /// Validate and own inputs without accessing a store.
    ///
    /// # Errors
    /// Returns `InvalidBucketName` for an invalid bucket or `InvalidArgument`
    /// for an invalid key or malformed account assertion.
    pub fn build(self) -> Result<GetObjectAclInput, StorageError> {
        let object = ObjectRef::new(&self.bucket, &self.key)?;

        if let Some(account) = &self.expected_bucket_owner {
            mikrio_core::access::validate_account_id(account)
                .map_err(StorageError::from_identity)?;
        }

        Ok(GetObjectAclInput {
            object,
            expected_bucket_owner: self.expected_bucket_owner,
        })
    }
}

impl Store {
    /// Read the current object's canonical owner and sole full-control grant.
    /// The detached result owns no payload cursor or guard.
    ///
    /// # Errors
    /// Returns `NoSuchBucket`, `NoSuchKey`, owner, lifecycle or storage failures.
    pub async fn get_object_acl(
        &self,
        input: GetObjectAclInput,
    ) -> Result<GetObjectAclOutput, StorageError> {
        let (bucket, key) = input.object.into_core();

        let request = mikrio_core::object::ReadRequest {
            bucket,
            key,
            expected_owner: input.expected_bucket_owner,
            conditions: mikrio_core::object::ReadConditions::default(),
            range: None,
            part_number: None,
            response: mikrio_core::object::Representation::default(),
        };

        self.connection
            .get_object_acl(request)
            .await
            .map(|owner| GetObjectAclOutput::from_core(&owner))
            .map_err(StorageError::from_operation)
    }
}
