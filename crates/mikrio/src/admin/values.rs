//! Detached administration results. Secret and policy contents stay out of Debug.

use mikrio_core::access;

use crate::Timestamp;

/// Immutable public owner identifiers, detached from the store connection.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct OwnerInfo {
    account_id: String,
    canonical_id: String,
}

impl OwnerInfo {
    /// Exact twelve-digit local account identifier.
    #[must_use]
    pub fn account_id(&self) -> &str {
        &self.account_id
    }

    /// Exact lowercase canonical owner identifier.
    #[must_use]
    pub fn canonical_id(&self) -> &str {
        &self.canonical_id
    }

    pub(super) fn from_core(value: &access::OwnerInfo) -> Self {
        Self {
            account_id: value.account_id().into(),
            canonical_id: value.canonical_id().into(),
        }
    }
}

/// Stable application identity; the human name is never an authorization identity.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PrincipalInfo {
    principal_id: String,
    name: String,
    is_enabled: bool,
    created_at: Timestamp,
}

impl PrincipalInfo {
    /// Canonical 32-character lowercase identity, independent of the name.
    #[must_use]
    pub fn principal_id(&self) -> &str {
        &self.principal_id
    }

    /// Exact case-sensitive human label.
    #[must_use]
    pub fn name(&self) -> &str {
        &self.name
    }

    /// Whether current persistent credentials may authenticate.
    #[must_use]
    pub fn is_enabled(&self) -> bool {
        self.is_enabled
    }

    /// Original creation time, unchanged by policy or enabled-state updates.
    #[must_use]
    pub fn created_at(&self) -> Timestamp {
        self.created_at
    }

    pub(super) fn from_core(value: &access::PrincipalInfo) -> Self {
        Self {
            principal_id: value.principal_id.to_string(),
            name: value.name.as_str().into(),
            is_enabled: value.is_enabled,
            created_at: Timestamp::from_core(value.created_at),
        }
    }
}

/// Nonsecret credential inventory; this value cannot recover a signing secret.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct AccessKeyInfo {
    access_key_id: String,
    principal_id: String,
    created_at: Timestamp,
}

impl AccessKeyInfo {
    /// Persistent application credential identifier.
    #[must_use]
    pub fn access_key_id(&self) -> &str {
        &self.access_key_id
    }

    /// Stable identity owning this key.
    #[must_use]
    pub fn principal_id(&self) -> &str {
        &self.principal_id
    }

    /// Key creation time.
    #[must_use]
    pub fn created_at(&self) -> Timestamp {
        self.created_at
    }

    pub(super) fn from_core(value: &access::AccessKeyInfo) -> Self {
        Self {
            access_key_id: value.access_key_id.to_string(),
            principal_id: value.principal_id.to_string(),
            created_at: Timestamp::from_core(value.created_at),
        }
    }
}

/// Confirmed key creation with explicit one-time secret disclosure.
/// Inventory and lost-response reconciliation cannot recover this secret.
#[derive(Debug)]
pub struct CreateAccessKeyOutput {
    access_key: AccessKeyInfo,
    secret_access_key: access::Secret,
}

impl CreateAccessKeyOutput {
    /// Detached nonsecret credential metadata.
    #[must_use]
    pub fn access_key(&self) -> &AccessKeyInfo {
        &self.access_key
    }

    /// Explicitly disclose the signing secret. Debug never includes this value.
    #[must_use]
    pub fn secret_access_key(&self) -> &str {
        self.secret_access_key.expose()
    }

    pub(super) fn from_core(value: access::CreateAccessKeyOutput) -> Self {
        Self {
            access_key: AccessKeyInfo::from_core(&value.access_key),
            secret_access_key: value.secret_access_key,
        }
    }
}

/// One bounded live-keyset page of principals, detached from a SQL snapshot.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ListPrincipalsOutput {
    principals: Vec<PrincipalInfo>,
    next_continuation_token: Option<String>,
}

impl ListPrincipalsOutput {
    /// Ascending binary principal-ID order.
    #[must_use]
    pub fn principals(&self) -> &[PrincipalInfo] {
        &self.principals
    }

    /// Opaque store/operation-scoped cursor; absent at the end of the list.
    #[must_use]
    pub fn next_continuation_token(&self) -> Option<&str> {
        self.next_continuation_token.as_deref()
    }

    pub(super) fn from_core(value: access::ListPrincipalsOutput) -> Self {
        Self {
            principals: value
                .principals
                .iter()
                .map(PrincipalInfo::from_core)
                .collect(),
            next_continuation_token: value.next_continuation_token,
        }
    }
}

/// One bounded, nonsecret live-keyset page of one principal's credentials.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ListAccessKeysOutput {
    access_keys: Vec<AccessKeyInfo>,
    next_continuation_token: Option<String>,
}

impl ListAccessKeysOutput {
    /// Ascending binary access-key-ID order.
    #[must_use]
    pub fn access_keys(&self) -> &[AccessKeyInfo] {
        &self.access_keys
    }

    /// Opaque cursor bound to this store, operation and principal.
    #[must_use]
    pub fn next_continuation_token(&self) -> Option<&str> {
        self.next_continuation_token.as_deref()
    }

    pub(super) fn from_core(value: access::ListAccessKeysOutput) -> Self {
        Self {
            access_keys: value
                .access_keys
                .iter()
                .map(AccessKeyInfo::from_core)
                .collect(),
            next_continuation_token: value.next_continuation_token,
        }
    }
}

/// Exact validated policy text; diagnostic formatting omits its contents.
pub struct GetPrincipalPolicyOutput {
    document: String,
}

impl GetPrincipalPolicyOutput {
    /// Original applied UTF-8 document, without normalization or merging.
    #[must_use]
    pub fn document(&self) -> &str {
        &self.document
    }

    pub(super) fn from_core(value: access::GetPrincipalPolicyOutput) -> Self {
        Self {
            document: value.document,
        }
    }
}

impl std::fmt::Debug for GetPrincipalPolicyOutput {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str("GetPrincipalPolicyOutput { document: [REDACTED] }")
    }
}

/// Successful whole-policy replacement.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[non_exhaustive]
pub struct PutPrincipalPolicyOutput;

/// Successful policy deletion, including an already absent policy.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[non_exhaustive]
pub struct DeletePrincipalPolicyOutput;

/// Successful key revocation, including an already absent key.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[non_exhaustive]
pub struct DeleteAccessKeyOutput;

/// Successful principal deletion after explicit revocation of all keys.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[non_exhaustive]
pub struct DeletePrincipalOutput;
