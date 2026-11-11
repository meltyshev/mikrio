//! Detached policy, public-access, ownership and ACL result values.

/// Four independent flags. Every replacement explicitly supplies every flag.
///
/// Construct this value with a struct literal; no omitted-field defaults exist.
/// An all-false configuration remains distinct from an absent configuration.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[allow(
    clippy::struct_excessive_bools,
    reason = "S3 specifies four independent flags, not one state"
)]
pub struct PublicAccessBlockConfiguration {
    /// Reject public ACLs; disabling this does not enable mutable ACL support.
    pub block_public_acls: bool,
    /// Ignore public ACLs; ACL authorization remains disabled in either case.
    pub ignore_public_acls: bool,
    /// Reject installation of a public policy, without removing an existing one.
    pub block_public_policy: bool,
    /// Restrict access through an existing public policy.
    pub restrict_public_buckets: bool,
}

impl PublicAccessBlockConfiguration {
    pub(super) fn into_core(self) -> mikrio_core::PublicAccessBlock {
        mikrio_core::PublicAccessBlock {
            block_public_acls: self.block_public_acls,
            ignore_public_acls: self.ignore_public_acls,
            block_public_policy: self.block_public_policy,
            restrict_public_buckets: self.restrict_public_buckets,
        }
    }

    pub(super) fn from_core(value: mikrio_core::PublicAccessBlock) -> Self {
        Self {
            block_public_acls: value.block_public_acls,
            ignore_public_acls: value.ignore_public_acls,
            block_public_policy: value.block_public_policy,
            restrict_public_buckets: value.restrict_public_buckets,
        }
    }
}

/// Supported object ownership model. Mutable ACL authorization is disabled.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ObjectOwnership {
    /// Every object belongs to the bucket owner, with ACL authorization disabled.
    BucketOwnerEnforced,
}

/// Exact stored policy document; diagnostic formatting omits its contents.
#[derive(Clone, PartialEq, Eq)]
pub struct GetBucketPolicyOutput {
    pub(super) document: String,
}

impl GetBucketPolicyOutput {
    /// Original applied UTF-8 text, without normalization or merging.
    #[must_use]
    pub fn document(&self) -> &str {
        &self.document
    }
}

impl std::fmt::Debug for GetBucketPolicyOutput {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("GetBucketPolicyOutput")
            .finish_non_exhaustive()
    }
}

/// Policy classification, independent of the flags that currently restrict access.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct GetBucketPolicyStatusOutput {
    pub(super) is_public: bool,
}

impl GetBucketPolicyStatusOutput {
    /// Whether the stored policy contains a supported public grant.
    #[must_use]
    pub const fn is_public(&self) -> bool {
        self.is_public
    }
}

/// Present configuration. An absent configuration returns a typed operation error.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct GetPublicAccessBlockOutput {
    pub(super) configuration: PublicAccessBlockConfiguration,
}

impl GetPublicAccessBlockOutput {
    /// All four independently persisted flags.
    #[must_use]
    pub const fn configuration(&self) -> PublicAccessBlockConfiguration {
        self.configuration
    }
}

/// The fixed ownership model of a live bucket.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct GetBucketOwnershipControlsOutput {
    pub(super) object_ownership: ObjectOwnership,
}

impl GetBucketOwnershipControlsOutput {
    /// Bucket-owner-enforced ownership remains present for every live bucket.
    #[must_use]
    pub const fn object_ownership(&self) -> ObjectOwnership {
        self.object_ownership
    }
}

/// Canonical ACL owner, detached from the connection.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AclOwner {
    canonical_id: String,
}

impl AclOwner {
    /// Stable canonical identifier; no display-name or account-ID substitution.
    #[must_use]
    pub fn canonical_id(&self) -> &str {
        &self.canonical_id
    }
}

/// Permission described by the fixed owner-only ACL.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AclPermission {
    /// The canonical bucket owner has full control.
    FullControl,
}

/// One grant to a canonical user; ACL mutation remains unsupported.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AclGrant {
    grantee_canonical_id: String,
    permission: AclPermission,
}

impl AclGrant {
    /// Exact canonical user identifier of the grantee.
    #[must_use]
    pub fn grantee_canonical_id(&self) -> &str {
        &self.grantee_canonical_id
    }

    /// Full control for the bucket owner.
    #[must_use]
    pub const fn permission(&self) -> AclPermission {
        self.permission
    }
}

/// Detached canonical owner and its owner-only full-control grant.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GetBucketAclOutput {
    owner: AclOwner,
    grants: [AclGrant; 1],
}

impl GetBucketAclOutput {
    /// Stable canonical owner of the bucket and all of its objects.
    #[must_use]
    pub fn owner(&self) -> &AclOwner {
        &self.owner
    }

    /// Exactly one full-control grant to the canonical owner.
    #[must_use]
    pub fn grants(&self) -> &[AclGrant] {
        &self.grants
    }

    pub(super) fn from_core(owner: &mikrio_core::access::OwnerInfo) -> Self {
        let canonical_id = owner.canonical_id().to_owned();

        Self {
            owner: AclOwner {
                canonical_id: canonical_id.clone(),
            },
            grants: [AclGrant {
                grantee_canonical_id: canonical_id,
                permission: AclPermission::FullControl,
            }],
        }
    }
}

/// Detached canonical owner and its owner-only full-control grant.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GetObjectAclOutput {
    owner: AclOwner,
    grants: [AclGrant; 1],
}

impl GetObjectAclOutput {
    /// Stable canonical owner of the bucket and all of its objects.
    #[must_use]
    pub fn owner(&self) -> &AclOwner {
        &self.owner
    }

    /// Exactly one full-control grant to the canonical owner.
    #[must_use]
    pub fn grants(&self) -> &[AclGrant] {
        &self.grants
    }

    pub(super) fn from_core(owner: &mikrio_core::access::OwnerInfo) -> Self {
        let canonical_id = owner.canonical_id().to_owned();

        Self {
            owner: AclOwner {
                canonical_id: canonical_id.clone(),
            },
            grants: [AclGrant {
                grantee_canonical_id: canonical_id,
                permission: AclPermission::FullControl,
            }],
        }
    }
}

/// Successful put bucket policy acknowledgment.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub struct PutBucketPolicyOutput;

/// Successful delete bucket policy acknowledgment.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub struct DeleteBucketPolicyOutput;

/// Successful put public access block acknowledgment.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub struct PutPublicAccessBlockOutput;

/// Successful delete public access block acknowledgment.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub struct DeletePublicAccessBlockOutput;

/// Successful put bucket ownership controls acknowledgment.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub struct PutBucketOwnershipControlsOutput;
