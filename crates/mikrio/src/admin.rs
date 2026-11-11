//! Trusted native administration through the shared bounded connection.

use crate::{StorageError, Store};

mod inputs;
mod values;

pub use inputs::{
    CreateAccessKeyInput, CreateAccessKeyInputBuilder, CreatePrincipalInput,
    CreatePrincipalInputBuilder, DeleteAccessKeyInput, DeleteAccessKeyInputBuilder,
    DeletePrincipalInput, DeletePrincipalInputBuilder, DeletePrincipalPolicyInput,
    DeletePrincipalPolicyInputBuilder, GetPrincipalInput, GetPrincipalInputBuilder,
    GetPrincipalPolicyInput, GetPrincipalPolicyInputBuilder, ListAccessKeysInput,
    ListAccessKeysInputBuilder, ListPrincipalsInput, ListPrincipalsInputBuilder,
    PutPrincipalPolicyInput, PutPrincipalPolicyInputBuilder, SetPrincipalEnabledInput,
    SetPrincipalEnabledInputBuilder,
};
pub use values::{
    AccessKeyInfo, CreateAccessKeyOutput, DeleteAccessKeyOutput, DeletePrincipalOutput,
    DeletePrincipalPolicyOutput, GetPrincipalPolicyOutput, ListAccessKeysOutput,
    ListPrincipalsOutput, OwnerInfo, PrincipalInfo, PutPrincipalPolicyOutput,
};

impl Store {
    /// Read immutable public owner identifiers. This grants no HTTP authority.
    ///
    /// # Errors
    /// Returns lifecycle, corruption or storage errors. The result is detached.
    pub async fn get_owner_info(&self) -> Result<OwnerInfo, StorageError> {
        self.connection
            .get_owner_info()
            .await
            .map(|value| OwnerInfo::from_core(&value))
            .map_err(StorageError::from_operation)
    }

    /// Create an enabled application identity with no policy or access keys.
    /// Dropping the future cancels work that has not entered its commit section.
    ///
    /// # Errors
    /// Preserves domain, lifecycle and storage errors; mutations report their outcome.
    pub async fn create_principal(
        &self,
        input: CreatePrincipalInput,
    ) -> Result<PrincipalInfo, StorageError> {
        self.connection
            .create_principal(input.name)
            .await
            .map(|value| PrincipalInfo::from_core(&value))
            .map_err(StorageError::from_operation)
    }

    /// Read detached information about an existing application identity.
    ///
    /// # Errors
    /// Preserves domain, lifecycle and storage errors.
    pub async fn get_principal(
        &self,
        input: GetPrincipalInput,
    ) -> Result<PrincipalInfo, StorageError> {
        self.connection
            .get_principal(input.principal_id)
            .await
            .map(|value| PrincipalInfo::from_core(&value))
            .map_err(StorageError::from_operation)
    }

    /// Enable or disable an identity while preserving its policy and access keys.
    /// Dropping the future cancels work that has not entered its commit section.
    ///
    /// # Errors
    /// Preserves domain, lifecycle and storage errors; mutations report their outcome.
    pub async fn set_principal_enabled(
        &self,
        input: SetPrincipalEnabledInput,
    ) -> Result<PrincipalInfo, StorageError> {
        self.connection
            .set_principal_enabled(input.principal_id, input.is_enabled)
            .await
            .map(|value| PrincipalInfo::from_core(&value))
            .map_err(StorageError::from_operation)
    }

    /// Delete an application identity and its attached policy atomically.
    /// Every key must be explicitly revoked first; deletion never cascades credentials.
    /// Dropping the future cancels work that has not entered its commit section.
    ///
    /// # Errors
    /// Preserves domain, lifecycle and storage errors; mutations report their outcome.
    pub async fn delete_principal(
        &self,
        input: DeletePrincipalInput,
    ) -> Result<DeletePrincipalOutput, StorageError> {
        self.connection
            .delete_principal(input.principal_id)
            .await
            .map_err(StorageError::from_operation)?;

        Ok(DeletePrincipalOutput)
    }

    /// Replace the identity's complete policy document atomically.
    /// Dropping the future cancels work that has not entered its commit section.
    ///
    /// # Errors
    /// Preserves domain, lifecycle and storage errors; mutations report their outcome.
    pub async fn put_principal_policy(
        &self,
        input: PutPrincipalPolicyInput,
    ) -> Result<PutPrincipalPolicyOutput, StorageError> {
        self.connection
            .put_principal_policy(input.principal_id, input.document)
            .await
            .map_err(StorageError::from_operation)?;

        Ok(PutPrincipalPolicyOutput)
    }

    /// Read the exact policy document attached to an existing identity.
    ///
    /// # Errors
    /// Preserves domain, lifecycle and storage errors.
    pub async fn get_principal_policy(
        &self,
        input: GetPrincipalPolicyInput,
    ) -> Result<GetPrincipalPolicyOutput, StorageError> {
        self.connection
            .get_principal_policy(input.principal_id)
            .await
            .map(GetPrincipalPolicyOutput::from_core)
            .map_err(StorageError::from_operation)
    }

    /// Remove the identity's policy; an already absent policy succeeds.
    /// Dropping the future cancels work that has not entered its commit section.
    ///
    /// # Errors
    /// Preserves domain, lifecycle and storage errors; mutations report their outcome.
    pub async fn delete_principal_policy(
        &self,
        input: DeletePrincipalPolicyInput,
    ) -> Result<DeletePrincipalPolicyOutput, StorageError> {
        self.connection
            .delete_principal_policy(input.principal_id)
            .await
            .map_err(StorageError::from_operation)?;

        Ok(DeletePrincipalPolicyOutput)
    }

    /// Create a persistent signing key and return its secret once.
    /// A disabled principal may prepare keys. Never retry creation automatically after an unknown outcome.
    /// Dropping the future cancels work that has not entered its commit section.
    ///
    /// # Errors
    /// Preserves domain, lifecycle and storage errors; mutations report their outcome.
    pub async fn create_access_key(
        &self,
        input: CreateAccessKeyInput,
    ) -> Result<CreateAccessKeyOutput, StorageError> {
        self.connection
            .create_access_key(input.principal_id)
            .await
            .map(CreateAccessKeyOutput::from_core)
            .map_err(StorageError::from_operation)
    }

    /// Revoke one key of the specified identity; an already absent key succeeds.
    /// Dropping the future cancels work that has not entered its commit section.
    ///
    /// # Errors
    /// Preserves domain, lifecycle and storage errors; mutations report their outcome.
    pub async fn delete_access_key(
        &self,
        input: DeleteAccessKeyInput,
    ) -> Result<DeleteAccessKeyOutput, StorageError> {
        self.connection
            .delete_access_key(input.principal_id, input.access_key_id)
            .await
            .map_err(StorageError::from_operation)?;

        Ok(DeleteAccessKeyOutput)
    }

    /// Read one bounded page of application identities in principal-ID order.
    /// Pages use live binary keyset order and do not promise a multi-request snapshot.
    ///
    /// # Errors
    /// Preserves domain, lifecycle and storage errors.
    pub async fn list_principals(
        &self,
        input: ListPrincipalsInput,
    ) -> Result<ListPrincipalsOutput, StorageError> {
        self.connection
            .list_principals(input.max_results, input.continuation_token)
            .await
            .map(ListPrincipalsOutput::from_core)
            .map_err(StorageError::from_operation)
    }

    /// Read one bounded page of an identity's keys without exposing secrets.
    /// Pages use live binary keyset order and do not promise a multi-request snapshot.
    ///
    /// # Errors
    /// Preserves domain, lifecycle and storage errors.
    pub async fn list_access_keys(
        &self,
        input: ListAccessKeysInput,
    ) -> Result<ListAccessKeysOutput, StorageError> {
        self.connection
            .list_access_keys(
                input.principal_id,
                input.max_results,
                input.continuation_token,
            )
            .await
            .map(ListAccessKeysOutput::from_core)
            .map_err(StorageError::from_operation)
    }
}
