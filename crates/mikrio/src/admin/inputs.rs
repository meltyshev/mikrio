//! Validated owned local administration inputs; construction performs no I/O.

use mikrio_core::access::{
    AccessKeyId, AdminPageSize, Policy, PolicyKind, PrincipalId, PrincipalName,
};

use crate::StorageError;

/// Validated owned values for [`crate::Store::create_principal`]. Construction performs no I/O.
#[derive(Debug, Clone)]
pub struct CreatePrincipalInput {
    pub(super) name: PrincipalName,
}

impl CreatePrincipalInput {
    /// Start an owned input builder with the required values.
    #[must_use]
    pub fn builder(name: impl Into<String>) -> CreatePrincipalInputBuilder {
        CreatePrincipalInputBuilder { name: name.into() }
    }

    /// Exact case-sensitive application identity name.
    #[must_use]
    pub fn name(&self) -> &str {
        self.name.as_str()
    }
}

/// Builder for [`CreatePrincipalInput`]. Validation occurs in [`Self::build`].
#[derive(Debug, Clone)]
pub struct CreatePrincipalInputBuilder {
    name: String,
}

impl CreatePrincipalInputBuilder {
    /// Validate values without opening or mutating storage.
    ///
    /// # Errors
    /// Returns `InvalidArgument` for an empty, oversized or control-containing name.
    pub fn build(self) -> Result<CreatePrincipalInput, StorageError> {
        let name = PrincipalName::new(self.name).map_err(StorageError::from_identity)?;

        Ok(CreatePrincipalInput { name })
    }
}

/// Validated owned values for [`crate::Store::get_principal`]. Construction performs no I/O.
#[derive(Debug, Clone)]
pub struct GetPrincipalInput {
    pub(super) principal_id: PrincipalId,
}

impl GetPrincipalInput {
    /// Start an owned input builder with the required values.
    #[must_use]
    pub fn builder(principal_id: impl Into<String>) -> GetPrincipalInputBuilder {
        GetPrincipalInputBuilder {
            principal_id: principal_id.into(),
        }
    }

    /// Canonical principal identifier, returned as an owned string.
    #[must_use]
    pub fn principal_id(&self) -> String {
        self.principal_id.to_string()
    }
}

/// Builder for [`GetPrincipalInput`]. Validation occurs in [`Self::build`].
#[derive(Debug, Clone)]
pub struct GetPrincipalInputBuilder {
    principal_id: String,
}

impl GetPrincipalInputBuilder {
    /// Validate values without opening or mutating storage.
    ///
    /// # Errors
    /// Returns `InvalidArgument` for a malformed principal identifier.
    pub fn build(self) -> Result<GetPrincipalInput, StorageError> {
        let principal_id =
            PrincipalId::parse(&self.principal_id).map_err(StorageError::from_identity)?;

        Ok(GetPrincipalInput { principal_id })
    }
}

/// Validated owned values for [`crate::Store::set_principal_enabled`]. Construction performs no I/O.
#[derive(Debug, Clone)]
pub struct SetPrincipalEnabledInput {
    pub(super) principal_id: PrincipalId,
    pub(super) is_enabled: bool,
}

impl SetPrincipalEnabledInput {
    /// Start an owned input builder with the required values.
    #[must_use]
    pub fn builder(
        principal_id: impl Into<String>,
        is_enabled: bool,
    ) -> SetPrincipalEnabledInputBuilder {
        SetPrincipalEnabledInputBuilder {
            principal_id: principal_id.into(),
            is_enabled,
        }
    }

    /// Canonical principal identifier, returned as an owned string.
    #[must_use]
    pub fn principal_id(&self) -> String {
        self.principal_id.to_string()
    }

    /// Whether the identity should accept authentication through its current keys.
    #[must_use]
    pub fn is_enabled(&self) -> bool {
        self.is_enabled
    }
}

/// Builder for [`SetPrincipalEnabledInput`]. Validation occurs in [`Self::build`].
#[derive(Debug, Clone)]
pub struct SetPrincipalEnabledInputBuilder {
    principal_id: String,
    is_enabled: bool,
}

impl SetPrincipalEnabledInputBuilder {
    /// Validate values without opening or mutating storage.
    ///
    /// # Errors
    /// Returns `InvalidArgument` for a malformed principal identifier.
    pub fn build(self) -> Result<SetPrincipalEnabledInput, StorageError> {
        let principal_id =
            PrincipalId::parse(&self.principal_id).map_err(StorageError::from_identity)?;

        Ok(SetPrincipalEnabledInput {
            principal_id,
            is_enabled: self.is_enabled,
        })
    }
}

/// Validated owned values for [`crate::Store::delete_principal`]. Construction performs no I/O.
#[derive(Debug, Clone)]
pub struct DeletePrincipalInput {
    pub(super) principal_id: PrincipalId,
}

impl DeletePrincipalInput {
    /// Start an owned input builder with the required values.
    #[must_use]
    pub fn builder(principal_id: impl Into<String>) -> DeletePrincipalInputBuilder {
        DeletePrincipalInputBuilder {
            principal_id: principal_id.into(),
        }
    }

    /// Canonical principal identifier, returned as an owned string.
    #[must_use]
    pub fn principal_id(&self) -> String {
        self.principal_id.to_string()
    }
}

/// Builder for [`DeletePrincipalInput`]. Validation occurs in [`Self::build`].
#[derive(Debug, Clone)]
pub struct DeletePrincipalInputBuilder {
    principal_id: String,
}

impl DeletePrincipalInputBuilder {
    /// Validate values without opening or mutating storage.
    ///
    /// # Errors
    /// Returns `InvalidArgument` for a malformed principal identifier.
    pub fn build(self) -> Result<DeletePrincipalInput, StorageError> {
        let principal_id =
            PrincipalId::parse(&self.principal_id).map_err(StorageError::from_identity)?;

        Ok(DeletePrincipalInput { principal_id })
    }
}

/// Validated owned values for [`crate::Store::put_principal_policy`]. Construction performs no I/O.
#[derive(Clone)]
pub struct PutPrincipalPolicyInput {
    pub(super) principal_id: PrincipalId,
    pub(super) document: String,
}

impl PutPrincipalPolicyInput {
    /// Start an owned input builder with the required values.
    #[must_use]
    pub fn builder(
        principal_id: impl Into<String>,
        document: impl Into<String>,
    ) -> PutPrincipalPolicyInputBuilder {
        PutPrincipalPolicyInputBuilder {
            principal_id: principal_id.into(),
            document: document.into(),
        }
    }

    /// Canonical principal identifier, returned as an owned string.
    #[must_use]
    pub fn principal_id(&self) -> String {
        self.principal_id.to_string()
    }

    /// Exact validated policy text, without normalization or merging.
    #[must_use]
    pub fn document(&self) -> &str {
        &self.document
    }
}

impl std::fmt::Debug for PutPrincipalPolicyInput {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("PutPrincipalPolicyInput")
            .field("principal_id", &self.principal_id)
            .field("document", &"[REDACTED]")
            .finish()
    }
}

/// Builder for [`PutPrincipalPolicyInput`]. Validation occurs in [`Self::build`].
#[derive(Clone)]
pub struct PutPrincipalPolicyInputBuilder {
    principal_id: String,
    document: String,
}

impl PutPrincipalPolicyInputBuilder {
    /// Validate values without opening or mutating storage.
    ///
    /// # Errors
    /// Returns `InvalidArgument` for a malformed principal identifier or invalid identity policy.
    pub fn build(self) -> Result<PutPrincipalPolicyInput, StorageError> {
        let principal_id =
            PrincipalId::parse(&self.principal_id).map_err(StorageError::from_identity)?;

        Policy::parse(&self.document, PolicyKind::Identity).map_err(StorageError::from_policy)?;

        Ok(PutPrincipalPolicyInput {
            principal_id,
            document: self.document,
        })
    }
}

impl std::fmt::Debug for PutPrincipalPolicyInputBuilder {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("PutPrincipalPolicyInputBuilder")
            .field("principal_id", &self.principal_id)
            .field("document", &"[REDACTED]")
            .finish()
    }
}

/// Validated owned values for [`crate::Store::get_principal_policy`]. Construction performs no I/O.
#[derive(Debug, Clone)]
pub struct GetPrincipalPolicyInput {
    pub(super) principal_id: PrincipalId,
}

impl GetPrincipalPolicyInput {
    /// Start an owned input builder with the required values.
    #[must_use]
    pub fn builder(principal_id: impl Into<String>) -> GetPrincipalPolicyInputBuilder {
        GetPrincipalPolicyInputBuilder {
            principal_id: principal_id.into(),
        }
    }

    /// Canonical principal identifier, returned as an owned string.
    #[must_use]
    pub fn principal_id(&self) -> String {
        self.principal_id.to_string()
    }
}

/// Builder for [`GetPrincipalPolicyInput`]. Validation occurs in [`Self::build`].
#[derive(Debug, Clone)]
pub struct GetPrincipalPolicyInputBuilder {
    principal_id: String,
}

impl GetPrincipalPolicyInputBuilder {
    /// Validate values without opening or mutating storage.
    ///
    /// # Errors
    /// Returns `InvalidArgument` for a malformed principal identifier.
    pub fn build(self) -> Result<GetPrincipalPolicyInput, StorageError> {
        let principal_id =
            PrincipalId::parse(&self.principal_id).map_err(StorageError::from_identity)?;

        Ok(GetPrincipalPolicyInput { principal_id })
    }
}

/// Validated owned values for [`crate::Store::delete_principal_policy`]. Construction performs no I/O.
#[derive(Debug, Clone)]
pub struct DeletePrincipalPolicyInput {
    pub(super) principal_id: PrincipalId,
}

impl DeletePrincipalPolicyInput {
    /// Start an owned input builder with the required values.
    #[must_use]
    pub fn builder(principal_id: impl Into<String>) -> DeletePrincipalPolicyInputBuilder {
        DeletePrincipalPolicyInputBuilder {
            principal_id: principal_id.into(),
        }
    }

    /// Canonical principal identifier, returned as an owned string.
    #[must_use]
    pub fn principal_id(&self) -> String {
        self.principal_id.to_string()
    }
}

/// Builder for [`DeletePrincipalPolicyInput`]. Validation occurs in [`Self::build`].
#[derive(Debug, Clone)]
pub struct DeletePrincipalPolicyInputBuilder {
    principal_id: String,
}

impl DeletePrincipalPolicyInputBuilder {
    /// Validate values without opening or mutating storage.
    ///
    /// # Errors
    /// Returns `InvalidArgument` for a malformed principal identifier.
    pub fn build(self) -> Result<DeletePrincipalPolicyInput, StorageError> {
        let principal_id =
            PrincipalId::parse(&self.principal_id).map_err(StorageError::from_identity)?;

        Ok(DeletePrincipalPolicyInput { principal_id })
    }
}

/// Validated owned values for [`crate::Store::create_access_key`]. Construction performs no I/O.
#[derive(Debug, Clone)]
pub struct CreateAccessKeyInput {
    pub(super) principal_id: PrincipalId,
}

impl CreateAccessKeyInput {
    /// Start an owned input builder with the required values.
    #[must_use]
    pub fn builder(principal_id: impl Into<String>) -> CreateAccessKeyInputBuilder {
        CreateAccessKeyInputBuilder {
            principal_id: principal_id.into(),
        }
    }

    /// Canonical principal identifier, returned as an owned string.
    #[must_use]
    pub fn principal_id(&self) -> String {
        self.principal_id.to_string()
    }
}

/// Builder for [`CreateAccessKeyInput`]. Validation occurs in [`Self::build`].
#[derive(Debug, Clone)]
pub struct CreateAccessKeyInputBuilder {
    principal_id: String,
}

impl CreateAccessKeyInputBuilder {
    /// Validate values without opening or mutating storage.
    ///
    /// # Errors
    /// Returns `InvalidArgument` for a malformed principal identifier.
    pub fn build(self) -> Result<CreateAccessKeyInput, StorageError> {
        let principal_id =
            PrincipalId::parse(&self.principal_id).map_err(StorageError::from_identity)?;

        Ok(CreateAccessKeyInput { principal_id })
    }
}

/// Validated owned values for [`crate::Store::delete_access_key`]. Construction performs no I/O.
#[derive(Debug, Clone)]
pub struct DeleteAccessKeyInput {
    pub(super) principal_id: PrincipalId,
    pub(super) access_key_id: AccessKeyId,
}

impl DeleteAccessKeyInput {
    /// Start an owned input builder with the required values.
    #[must_use]
    pub fn builder(
        principal_id: impl Into<String>,
        access_key_id: impl Into<String>,
    ) -> DeleteAccessKeyInputBuilder {
        DeleteAccessKeyInputBuilder {
            principal_id: principal_id.into(),
            access_key_id: access_key_id.into(),
        }
    }

    /// Canonical principal identifier, returned as an owned string.
    #[must_use]
    pub fn principal_id(&self) -> String {
        self.principal_id.to_string()
    }

    /// Exact persistent application-key identifier.
    #[must_use]
    pub fn access_key_id(&self) -> &str {
        self.access_key_id.as_str()
    }
}

/// Builder for [`DeleteAccessKeyInput`]. Validation occurs in [`Self::build`].
#[derive(Debug, Clone)]
pub struct DeleteAccessKeyInputBuilder {
    principal_id: String,
    access_key_id: String,
}

impl DeleteAccessKeyInputBuilder {
    /// Validate values without opening or mutating storage.
    ///
    /// # Errors
    /// Returns `InvalidArgument` for a malformed principal or access-key identifier.
    pub fn build(self) -> Result<DeleteAccessKeyInput, StorageError> {
        let principal_id =
            PrincipalId::parse(&self.principal_id).map_err(StorageError::from_identity)?;

        let access_key_id =
            AccessKeyId::parse(self.access_key_id).map_err(StorageError::from_identity)?;

        Ok(DeleteAccessKeyInput {
            principal_id,
            access_key_id,
        })
    }
}

/// Validated owned values for [`crate::Store::list_principals`]. Construction performs no I/O.
#[derive(Debug, Clone)]
pub struct ListPrincipalsInput {
    pub(super) max_results: AdminPageSize,
    pub(super) continuation_token: Option<String>,
}

impl ListPrincipalsInput {
    /// Start an owned input builder with the required values.
    #[must_use]
    pub fn builder() -> ListPrincipalsInputBuilder {
        ListPrincipalsInputBuilder {
            max_results: 100,
            continuation_token: None,
        }
    }

    /// Effective bound, from one through one thousand; defaults to one hundred.
    #[must_use]
    pub fn max_results(&self) -> u32 {
        self.max_results.get()
    }

    /// Optional opaque cursor. Its scope is checked against the opened store.
    #[must_use]
    pub fn continuation_token(&self) -> Option<&str> {
        self.continuation_token.as_deref()
    }
}

/// Builder for [`ListPrincipalsInput`]. Validation occurs in [`Self::build`].
#[derive(Debug, Clone)]
pub struct ListPrincipalsInputBuilder {
    max_results: u32,
    continuation_token: Option<String>,
}

impl ListPrincipalsInputBuilder {
    /// Select a page size from 1 through 1000; defaults to 100.
    #[must_use]
    pub fn max_results(mut self, value: u32) -> Self {
        self.max_results = value;
        self
    }

    /// Continue a live-keyset page with an opaque, store-scoped cursor.
    #[must_use]
    pub fn continuation_token(mut self, value: impl Into<String>) -> Self {
        self.continuation_token = Some(value.into());
        self
    }

    /// Validate values without opening or mutating storage.
    ///
    /// # Errors
    /// Returns `InvalidArgument` for a page size outside 1-1000 or a supplied cursor
    /// outside 1-260 bytes. Cursor contents are checked when executing the request.
    pub fn build(self) -> Result<ListPrincipalsInput, StorageError> {
        let max_results =
            AdminPageSize::new(self.max_results).map_err(StorageError::from_identity)?;

        if self
            .continuation_token
            .as_ref()
            .is_some_and(|token| token.is_empty() || token.len() > 260)
        {
            return Err(StorageError::from_policy(
                mikrio_core::access::PolicyError::Invalid,
            ));
        }

        Ok(ListPrincipalsInput {
            max_results,
            continuation_token: self.continuation_token,
        })
    }
}

/// Validated owned values for [`crate::Store::list_access_keys`]. Construction performs no I/O.
#[derive(Debug, Clone)]
pub struct ListAccessKeysInput {
    pub(super) principal_id: PrincipalId,
    pub(super) max_results: AdminPageSize,
    pub(super) continuation_token: Option<String>,
}

impl ListAccessKeysInput {
    /// Start an owned input builder with the required values.
    #[must_use]
    pub fn builder(principal_id: impl Into<String>) -> ListAccessKeysInputBuilder {
        ListAccessKeysInputBuilder {
            principal_id: principal_id.into(),
            max_results: 100,
            continuation_token: None,
        }
    }

    /// Canonical principal identifier, returned as an owned string.
    #[must_use]
    pub fn principal_id(&self) -> String {
        self.principal_id.to_string()
    }

    /// Effective bound, from one through one thousand; defaults to one hundred.
    #[must_use]
    pub fn max_results(&self) -> u32 {
        self.max_results.get()
    }

    /// Optional opaque cursor. Its scope is checked against the opened store.
    #[must_use]
    pub fn continuation_token(&self) -> Option<&str> {
        self.continuation_token.as_deref()
    }
}

/// Builder for [`ListAccessKeysInput`]. Validation occurs in [`Self::build`].
#[derive(Debug, Clone)]
pub struct ListAccessKeysInputBuilder {
    principal_id: String,
    max_results: u32,
    continuation_token: Option<String>,
}

impl ListAccessKeysInputBuilder {
    /// Select a page size from 1 through 1000; defaults to 100.
    #[must_use]
    pub fn max_results(mut self, value: u32) -> Self {
        self.max_results = value;
        self
    }

    /// Continue a live-keyset page with an opaque, store-scoped cursor.
    #[must_use]
    pub fn continuation_token(mut self, value: impl Into<String>) -> Self {
        self.continuation_token = Some(value.into());
        self
    }

    /// Validate values without opening or mutating storage.
    ///
    /// # Errors
    /// Returns `InvalidArgument` for a malformed principal identifier, a page size
    /// outside 1-1000 or a supplied cursor outside 1-260 bytes. Cursor contents
    /// are checked when executing the request.
    pub fn build(self) -> Result<ListAccessKeysInput, StorageError> {
        let principal_id =
            PrincipalId::parse(&self.principal_id).map_err(StorageError::from_identity)?;

        let max_results =
            AdminPageSize::new(self.max_results).map_err(StorageError::from_identity)?;

        if self
            .continuation_token
            .as_ref()
            .is_some_and(|token| token.is_empty() || token.len() > 260)
        {
            return Err(StorageError::from_policy(
                mikrio_core::access::PolicyError::Invalid,
            ));
        }

        Ok(ListAccessKeysInput {
            principal_id,
            max_results,
            continuation_token: self.continuation_token,
        })
    }
}
