//! Fresh access snapshots and transaction-local authorization checks.
//! No policy/key/permission cache and no SQL escape to a binding or HTTP adapter.

use std::fmt;
use std::sync::atomic::Ordering;

use rusqlite::{OptionalExtension, Transaction};

use super::{MetadataDatabase, MetadataError};
use crate::access::{
    Action, Context, CredentialClass, HttpIdentity, Identity, IdentityError, OwnerInfo, Policy,
    PolicyError, PolicyKind, PrincipalId, Secret, SigningKey, Verdict,
};

/// Four independent stored flags; absent configuration is represented by Option.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[allow(
    clippy::struct_excessive_bools,
    reason = "the four independent S3 BPA flags are the selected protocol, not a lifecycle state"
)]
pub struct PublicAccessBlock {
    /// Reject public ACLs; ACL mutation remains unsupported regardless of this flag.
    pub block_public_acls: bool,
    /// Ignore public ACLs; disabling it cannot enable ACL authorization.
    pub ignore_public_acls: bool,
    /// Reject applying a public policy; does not invalidate an existing policy.
    pub block_public_policy: bool,
    /// Restrict an existing public policy to authorized same-account identities.
    pub restrict_public_buckets: bool,
}

impl PublicAccessBlock {
    /// Check a proposed bucket-policy installation separately from read access.
    /// A mutation must read these flags in its own write transaction.
    ///
    /// # Errors
    /// Denies a public policy when `BlockPublicPolicy` is enabled. Other flags
    /// cannot substitute for this check or turn ACL mutation into a supported API.
    pub fn check_policy_install(self, policy: &Policy) -> Result<(), AccessError> {
        if self.block_public_policy && policy.is_public() {
            return Err(AccessError::Denied);
        }

        Ok(())
    }
}

/// One already interpreted permission check. Authority is a separate opaque input.
pub struct AccessRequest<'a> {
    /// Bucket namespace selected by the operation, never parsed from a policy.
    pub bucket: &'a str,
    /// Immutable identity pinned at admission for a later mutation recheck.
    pub expected_bucket_id: Option<[u8; 16]>,
    /// Exact object key when this action checks an object resource.
    pub key: Option<&'a str>,
    /// Explicit action from the operation's permission table.
    pub action: Action,
    /// Operation-specific request facts. Current object/tag facts must be selected
    /// inside the same transaction by the implementing operation, never cached.
    pub context: &'a Context,
    /// Caller owner assertion; applies equally to bootstrap and application users.
    pub expected_owner: Option<&'a str>,
}

/// Bounded owned facts crossing into the metadata worker. It contains no grant
/// or trusted-native selector. Construct from a checked borrowed request.
pub struct OwnedAccessRequest {
    bucket: String,
    expected_bucket_id: Option<[u8; 16]>,
    key: Option<String>,
    action: Action,
    context: Context,
    expected_owner: Option<String>,
}

impl AccessRequest<'_> {
    /// Validate bounds before copying request facts for queue ownership.
    ///
    /// # Errors
    /// Rejects invalid names/lengths/assertions and unsupported resource kinds.
    pub fn into_owned(self) -> Result<OwnedAccessRequest, PolicyError> {
        self.validate()?;

        Ok(OwnedAccessRequest {
            bucket: self.bucket.into(),
            expected_bucket_id: self.expected_bucket_id,
            key: self.key.map(str::to_owned),
            action: self.action,
            context: self.context.clone(),
            expected_owner: self.expected_owner.map(str::to_owned),
        })
    }

    fn validate(&self) -> Result<(), PolicyError> {
        if self.bucket.is_empty() || self.bucket.len() > 63 {
            return Err(PolicyError::Invalid);
        }

        if self
            .key
            .is_some_and(|key| key.is_empty() || key.len() > 1024)
        {
            return Err(PolicyError::Invalid);
        }

        if self
            .expected_owner
            .is_some_and(|owner| crate::access::validate_account_id(owner).is_err())
        {
            return Err(PolicyError::Invalid);
        }

        if self.action.as_str() == "s3:ListAllMyBuckets"
            || self.action.object_resource() != self.key.is_some()
        {
            return Err(PolicyError::Unsupported);
        }

        Ok(())
    }
}

impl OwnedAccessRequest {
    /// Borrow facts while the native command retains the owned allocation.
    #[must_use]
    pub fn as_request(&self) -> AccessRequest<'_> {
        AccessRequest {
            bucket: &self.bucket,
            expected_bucket_id: self.expected_bucket_id,
            key: self.key.as_deref(),
            action: self.action,
            context: &self.context,
            expected_owner: self.expected_owner.as_deref(),
        }
    }
}

/// Authorization errors retain corruption and storage failure, rather than
/// converting unavailable security state into an empty policy or ordinary deny.
#[derive(Debug)]
pub enum AccessError {
    /// Native admission, close fence or worker settlement failed.
    Connection(crate::ConnectionError),
    /// Invalid supplied input or a policy outside the qualified subset.
    Invalid(PolicyError),
    /// Malformed identity/credential input, without including its supplied value.
    Identity(IdentityError),
    /// Current authority does not allow this access.
    Denied,
    /// An existing SQL transaction cannot be reused as a fresh admission snapshot.
    SnapshotActive,
    /// Persisted security data failed structural or semantic validation.
    Corrupt(&'static str),
    /// Reading or settling the real metadata transaction failed.
    Storage(MetadataError),
    /// Read-snapshot rollback failed after an earlier access error.
    Settlement {
        /// Original denial/input/storage error, preserved without formatting input.
        operation: Box<AccessError>,
        /// Failed rollback; subsequent work must not reuse the unsettled snapshot.
        rollback: rusqlite::Error,
    },
}

impl fmt::Display for AccessError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(match self {
            Self::Connection(_) => "access worker unavailable",
            Self::Invalid(_) => "invalid access input",
            Self::Identity(_) => "invalid identity input",
            Self::Denied => "access denied",
            Self::SnapshotActive => "fresh access snapshot requires a settled connection",
            Self::Corrupt(message) => message,
            Self::Storage(_) => "access metadata unavailable",
            Self::Settlement { .. } => "access snapshot settlement failed",
        })
    }
}

impl std::error::Error for AccessError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::Connection(error) => Some(error),
            Self::Invalid(error) => Some(error),
            Self::Identity(error) => Some(error),
            Self::Storage(error) => Some(error),
            Self::Settlement { operation, .. } => Some(operation.as_ref()),
            _ => None,
        }
    }
}

impl From<rusqlite::Error> for AccessError {
    fn from(error: rusqlite::Error) -> Self {
        match error {
            rusqlite::Error::InvalidColumnType(..)
            | rusqlite::Error::IntegralValueOutOfRange(..)
            | rusqlite::Error::FromSqlConversionFailure(..) => {
                Self::Corrupt("invalid stored access field type")
            }
            error => Self::Storage(error.into()),
        }
    }
}

impl From<PolicyError> for AccessError {
    fn from(error: PolicyError) -> Self {
        Self::Invalid(error)
    }
}

impl From<IdentityError> for AccessError {
    fn from(error: IdentityError) -> Self {
        Self::Identity(error)
    }
}

impl From<crate::ConnectionError> for AccessError {
    fn from(error: crate::ConnectionError) -> Self {
        Self::Connection(error)
    }
}

impl MetadataDatabase {
    /// Read the real initialized owner's immutable public IDs as detached values.
    ///
    /// # Errors
    /// Rejects an older active snapshot, missing/malformed owner state and failed
    /// SQL access or settlement. Does not generate/recover replacement identities.
    pub fn owner_info(&mut self) -> Result<OwnerInfo, AccessError> {
        if self.unusable || !self.connection.is_autocommit() {
            return Err(AccessError::SnapshotActive);
        }

        let tx = self.connection.transaction()?;

        let result = (|| {
            let (account, canonical) = tx
                .query_row(
                    "SELECT owner_account_id, canonical_owner_id
                     FROM store_metadata WHERE singleton_id = 1",
                    [],
                    |row| Ok((row.get(0)?, row.get(1)?)),
                )
                .optional()?
                .ok_or(AccessError::Corrupt("missing store owner identity"))?;

            OwnerInfo::new(account, canonical)
                .map_err(|_| AccessError::Corrupt("invalid stored owner identity"))
        })();

        settle(tx, result)
    }

    /// Read signing material from a fresh real snapshot in the application class.
    /// Signature verification is the private HTTP adapter's separate responsibility.
    ///
    /// # Errors
    /// Refuses other credential classes, disabled/missing identities, malformed
    /// stored data, an active older snapshot, and unavailable metadata. Never
    /// falls back to bootstrap or anonymous access after a lookup failure.
    pub fn resolve_application_key(&mut self, key: &str) -> Result<SigningKey, AccessError> {
        if CredentialClass::parse(key)? != CredentialClass::Application {
            return Err(AccessError::Denied);
        }

        if self.unusable || !self.connection.is_autocommit() {
            return Err(AccessError::SnapshotActive);
        }

        let tx = self.connection.transaction()?;
        let result = application_key(&tx, key);
        settle(tx, result)
    }

    /// Authorize a new request from one short, current SQL snapshot.
    /// No transaction survives payload transfer. This result is admission only;
    /// mutations must call `authorize` inside their publication transaction.
    ///
    /// # Errors
    /// Preserves explicit denial, invalid input, corrupt policy/key/BPA state,
    /// stale-snapshot misuse and unavailable metadata as distinct failures.
    pub fn authorize_http(
        &mut self,
        identity: &HttpIdentity,
        request: &AccessRequest<'_>,
    ) -> Result<(), AccessError> {
        self.authorize_http_all(identity, std::slice::from_ref(request))
    }

    /// Require every permission in the same fresh snapshot. The operation owns
    /// this explicit list (for example conditional PUT needs destination PUT and
    /// GET); decoded headers never supply action names or native authority.
    /// Four checks cover the scoped COPY source/destination/tag/condition case.
    ///
    /// # Errors
    /// Rejects empty/oversized check lists and preserves the same denial,
    /// corruption, snapshot and storage errors as [`Self::authorize_http`].
    pub fn authorize_http_all(
        &mut self,
        identity: &HttpIdentity,
        requests: &[AccessRequest<'_>],
    ) -> Result<(), AccessError> {
        if requests.is_empty() || requests.len() > 4 {
            return Err(AccessError::Invalid(PolicyError::Invalid));
        }

        if self.unusable || !self.connection.is_autocommit() {
            return Err(AccessError::SnapshotActive);
        }

        let tx = self.connection.transaction()?;

        let result = requests
            .iter()
            .try_for_each(|request| authorize(&tx, identity, request));

        settle(tx, result)
    }
}

/// Transaction-local access recheck, after BEGIN IMMEDIATE and current resource/
/// tag selection, before publication changes. The result is not a reusable grant.
pub(super) fn authorize(
    tx: &Transaction<'_>,
    identity: &HttpIdentity,
    request: &AccessRequest<'_>,
) -> Result<(), AccessError> {
    authorize_with_initiator(tx, identity, request, None)
}

/// The historical initiator is a default grant for these two actions only.
/// Current credentials, stored policy validation and explicit denies still apply.
pub(super) fn authorize_multipart(
    tx: &Transaction<'_>,
    identity: &HttpIdentity,
    request: &AccessRequest<'_>,
    initiator: Option<&str>,
) -> Result<(), AccessError> {
    if !matches!(
        request.action.as_str(),
        "s3:ListMultipartUploadParts" | "s3:AbortMultipartUpload"
    ) {
        return Err(AccessError::Invalid(PolicyError::Unsupported));
    }

    authorize_with_initiator(tx, identity, request, initiator)
}

fn authorize_with_initiator(
    tx: &Transaction<'_>,
    identity: &HttpIdentity,
    request: &AccessRequest<'_>,
    initiator: Option<&str>,
) -> Result<(), AccessError> {
    request.validate()?;

    let owner: String = tx
        .prepare_cached("SELECT owner_account_id FROM store_metadata WHERE singleton_id = 1")?
        .query_row([], |row| row.get(0))
        .optional()?
        .ok_or(AccessError::Corrupt("missing store account identity"))?;

    if owner.len() != 12 || !owner.bytes().all(|byte| byte.is_ascii_digit()) {
        return Err(AccessError::Corrupt("invalid stored account identity"));
    }

    if request
        .expected_owner
        .is_some_and(|expected| expected != owner)
    {
        return Err(AccessError::Denied);
    }

    let (bucket_id, policy_generation, policy_json, bpa) = tx
        .prepare_cached(
            "SELECT bucket_id, policy_generation, policy_json, block_public_acls,
             ignore_public_acls, block_public_policy, restrict_public_buckets
             FROM buckets WHERE name = ?1",
        )?
        .query_row([request.bucket], |row| {
            Ok((
                row.get::<_, Vec<u8>>(0)?,
                row.get::<_, Option<Vec<u8>>>(1)?,
                row.get::<_, Option<String>>(2)?,
                [
                    row.get::<_, Option<i64>>(3)?,
                    row.get(4)?,
                    row.get(5)?,
                    row.get(6)?,
                ],
            ))
        })
        .optional()?
        .ok_or(AccessError::Denied)?;

    let bucket_id = identity_bytes(bucket_id)?;

    if request
        .expected_bucket_id
        .is_some_and(|expected| expected != bucket_id)
    {
        return Err(AccessError::Denied);
    }

    let bucket = policy(
        policy_generation,
        policy_json,
        PolicyKind::Bucket {
            name: request.bucket,
            owner_account_id: &owner,
        },
    )?;

    let bpa = public_access_block(bpa)?;

    let resource = request.key.map_or_else(
        || format!("arn:aws:s3:::{}", request.bucket),
        |key| format!("arn:aws:s3:::{}/{key}", request.bucket),
    );

    let action = request.action;
    let (principal, attached, bootstrap) = subject(tx, identity, &owner)?;

    // Recovery skips bucket-policy denial only for these three owner actions.
    // Owner assertions and corrupt-policy/BPA validation have already run.
    let can_recover_policy = bootstrap
        && matches!(
            action.as_str(),
            "s3:GetBucketPolicy" | "s3:PutBucketPolicy" | "s3:DeleteBucketPolicy"
        );

    if can_recover_policy {
        return Ok(());
    }

    let evaluate =
        |policy: &Policy| policy.evaluate(action, &resource, principal.as_deref(), request.context);

    let attached = attached
        .as_ref()
        .map(evaluate)
        .transpose()?
        .unwrap_or(Verdict::ImplicitDeny);

    let bucket_verdict = bucket
        .as_ref()
        .map(evaluate)
        .transpose()?
        .unwrap_or(Verdict::ImplicitDeny);

    if attached == Verdict::ExplicitDeny || bucket_verdict == Verdict::ExplicitDeny {
        return Err(AccessError::Denied);
    }

    if matches!(identity.0, Identity::Anonymous)
        && bucket.as_ref().is_some_and(Policy::is_public)
        && bpa.is_some_and(|flags| flags.restrict_public_buckets)
    {
        return Err(AccessError::Denied);
    }

    let initiated = principal
        .as_deref()
        .is_some_and(|principal| Some(principal) == initiator);

    if bootstrap || initiated || attached == Verdict::Allow || bucket_verdict == Verdict::Allow {
        return Ok(());
    }

    Err(AccessError::Denied)
}

/// An absent bucket has no bucket policy or BPA. Authorize the prospective ARN
/// from current identity state inside the same creation transaction.
pub(super) fn authorize_creation(
    tx: &Transaction<'_>,
    identity: &HttpIdentity,
    bucket: &str,
) -> Result<(), AccessError> {
    let owner: String = tx
        .query_row(
            "SELECT owner_account_id FROM store_metadata WHERE singleton_id = 1",
            [],
            |row| row.get(0),
        )
        .optional()?
        .ok_or(AccessError::Corrupt("missing store account identity"))?;

    crate::access::validate_account_id(&owner)
        .map_err(|_| AccessError::Corrupt("invalid stored account identity"))?;

    let (principal, attached, bootstrap) = subject(tx, identity, &owner)?;

    let verdict = attached
        .as_ref()
        .map(|policy| {
            policy.evaluate(
                Action::parse("s3:CreateBucket")?,
                &format!("arn:aws:s3:::{bucket}"),
                principal.as_deref(),
                &Context::default(),
            )
        })
        .transpose()?
        .unwrap_or(Verdict::ImplicitDeny);

    if verdict == Verdict::Allow || bootstrap {
        return Ok(());
    }

    Err(AccessError::Denied)
}

/// Listing is an account operation. No per-bucket policy or prefix filtering
/// can substitute for the current identity's `ListAllMyBuckets` permission.
pub(super) fn authorize_bucket_listing(
    tx: &Transaction<'_>,
    identity: &HttpIdentity,
    owner: &str,
) -> Result<(), AccessError> {
    let (principal, attached, bootstrap) = subject(tx, identity, owner)?;

    let verdict = attached
        .as_ref()
        .map(|policy| {
            policy.evaluate(
                Action::parse("s3:ListAllMyBuckets")?,
                "*",
                principal.as_deref(),
                &Context::default(),
            )
        })
        .transpose()?
        .unwrap_or(Verdict::ImplicitDeny);

    if verdict == Verdict::ExplicitDeny {
        return Err(AccessError::Denied);
    }

    if bootstrap || verdict == Verdict::Allow {
        return Ok(());
    }

    Err(AccessError::Denied)
}

fn subject(
    tx: &Transaction<'_>,
    identity: &HttpIdentity,
    owner: &str,
) -> Result<(Option<String>, Option<Policy>, bool), AccessError> {
    Ok(match &identity.0 {
        Identity::Anonymous => (None, None, false),
        Identity::Bootstrap(active) => {
            if !active.load(Ordering::Acquire) {
                return Err(AccessError::Denied);
            }

            (None, None, true)
        }
        Identity::Application { key, principal } => {
            let (current_principal, secret, is_enabled, policy_generation, policy_json) = tx
                .prepare_cached(
                    "SELECT access_keys.principal_id, access_keys.secret_access_key,
                     principals.is_enabled, principals.policy_generation, principals.policy_json
                     FROM access_keys LEFT JOIN principals
                     ON principals.principal_id = access_keys.principal_id
                     WHERE access_keys.access_key_id = ?1",
                )?
                .query_row([key], |row| {
                    Ok((
                        row.get::<_, Vec<u8>>(0)?,
                        row.get::<_, String>(1)?,
                        row.get::<_, Option<i64>>(2)?,
                        row.get::<_, Option<Vec<u8>>>(3)?,
                        row.get::<_, Option<String>>(4)?,
                    ))
                })
                .optional()?
                .ok_or(AccessError::Denied)?;

            let is_enabled =
                enabled(is_enabled.ok_or(AccessError::Corrupt("orphan signing key"))?)?;

            // Prior signature verification cannot validate a credential row that
            // became corrupt before current-state selection or publication.
            Secret::new(secret)
                .map_err(|_| AccessError::Corrupt("invalid stored signing secret"))?;

            if identity_bytes(current_principal)? != *principal.as_bytes() || !is_enabled {
                return Err(AccessError::Denied);
            }

            (
                Some(principal.arn(owner)?),
                policy(policy_generation, policy_json, PolicyKind::Identity)?,
                false,
            )
        }
    })
}

fn application_key(tx: &Transaction<'_>, key: &str) -> Result<SigningKey, AccessError> {
    let (principal_id, secret, is_enabled) = tx
        .prepare_cached(
            "SELECT access_keys.principal_id, access_keys.secret_access_key, principals.is_enabled
             FROM access_keys LEFT JOIN principals ON principals.principal_id = access_keys.principal_id
             WHERE access_keys.access_key_id = ?1",
        )?
        .query_row([key], |row| {
            Ok((
                row.get::<_, Vec<u8>>(0)?,
                row.get::<_, String>(1)?,
                row.get::<_, Option<i64>>(2)?,
            ))
        })
        .optional()?
        .ok_or(AccessError::Denied)?;

    if !enabled(is_enabled.ok_or(AccessError::Corrupt("orphan signing key"))?)? {
        return Err(AccessError::Denied);
    }

    let principal = PrincipalId::from_bytes(identity_bytes(principal_id)?);

    let secret =
        Secret::new(secret).map_err(|_| AccessError::Corrupt("invalid stored signing secret"))?;

    Ok(SigningKey {
        secret,
        identity: Identity::Application {
            key: key.into(),
            principal,
        },
    })
}

fn settle<T>(tx: Transaction<'_>, result: Result<T, AccessError>) -> Result<T, AccessError> {
    match result {
        Ok(value) => {
            tx.commit()?;
            Ok(value)
        }
        Err(operation) => match tx.rollback() {
            Ok(()) => Err(operation),
            Err(rollback) => Err(AccessError::Settlement {
                operation: Box::new(operation),
                rollback,
            }),
        },
    }
}

fn enabled(value: i64) -> Result<bool, AccessError> {
    match value {
        0 => Ok(false),
        1 => Ok(true),
        _ => Err(AccessError::Corrupt("invalid principal enabled state")),
    }
}

fn identity_bytes(value: Vec<u8>) -> Result<[u8; 16], AccessError> {
    value
        .try_into()
        .map_err(|_| AccessError::Corrupt("invalid stored access identity"))
}

pub(super) fn policy(
    generation: Option<Vec<u8>>,
    document: Option<String>,
    kind: PolicyKind<'_>,
) -> Result<Option<Policy>, AccessError> {
    match (generation, document) {
        (None, None) => Ok(None),
        (Some(generation), Some(document)) if generation.len() == 16 => {
            Policy::parse(&document, kind)
                .map(Some)
                .map_err(|_| AccessError::Corrupt("invalid stored access policy"))
        }
        _ => Err(AccessError::Corrupt(
            "inconsistent policy generation and document",
        )),
    }
}

pub(super) fn public_access_block(
    values: [Option<i64>; 4],
) -> Result<Option<PublicAccessBlock>, AccessError> {
    if values == [None; 4] {
        return Ok(None);
    }

    let mut flags = [false; 4];

    for (flag, value) in flags.iter_mut().zip(values) {
        *flag = match value {
            Some(0) => false,
            Some(1) => true,
            _ => return Err(AccessError::Corrupt("invalid public access block state")),
        };
    }

    Ok(Some(PublicAccessBlock {
        block_public_acls: flags[0],
        ignore_public_acls: flags[1],
        block_public_policy: flags[2],
        restrict_public_buckets: flags[3],
    }))
}

#[cfg(test)]
mod tests;
