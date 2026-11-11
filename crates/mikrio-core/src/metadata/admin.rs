//! Trusted identity administration uses the ordinary transaction settlement path.
//! Names are labels; only random principal IDs bind credentials and policies.

mod page;

use rusqlite::{OptionalExtension, Row, Transaction, params};

use super::{MetadataDatabase, MetadataError, decode_error, now};
use crate::access::{
    AccessKeyId, CreateAccessKeyOutput, GetPrincipalPolicyOutput, OwnerInfo, Policy, PolicyKind,
    PreparedAccessKey, PrincipalId, PrincipalInfo, PrincipalName,
};
use crate::{EpochMillis, OperationError, OperationFailure, StoreId};

const SELECT_PRINCIPAL: &str =
    "SELECT principal_id, name, is_enabled, created_at_ms FROM principals WHERE principal_id = ?1";

impl MetadataDatabase {
    pub(crate) fn get_owner_info(&mut self) -> Result<OwnerInfo, OperationError> {
        self.read(owner_info)
    }

    pub(crate) fn create_principal(
        &mut self,
        name: PrincipalName,
    ) -> Result<PrincipalInfo, OperationError> {
        let principal_id = PrincipalId::from_bytes(
            *StoreId::generate()
                .map_err(MetadataError::Entropy)?
                .as_bytes(),
        );

        let created_at = now()?;

        #[cfg(test)]
        let _protocol = crate::protocol_fault::Scope::enter("principal.create");

        self.write(|transaction| {
            if transaction
                .prepare_cached("SELECT 1 FROM principals WHERE name = ?1")?
                .exists([name.as_str()])?
            {
                return Err(OperationError::new(
                    OperationFailure::PrincipalAlreadyExists,
                ));
            }

            transaction.execute(
                "INSERT INTO principals (principal_id, name, is_enabled, created_at_ms)
                 VALUES (?1, ?2, 1, ?3)",
                params![
                    principal_id.as_bytes().as_slice(),
                    name.as_str(),
                    created_at.get()
                ],
            )?;

            Ok(PrincipalInfo {
                principal_id,
                name,
                is_enabled: true,
                created_at,
            })
        })
    }

    pub(crate) fn get_principal(
        &mut self,
        principal_id: PrincipalId,
    ) -> Result<PrincipalInfo, OperationError> {
        self.read(|transaction| principal(transaction, principal_id))
    }

    pub(crate) fn set_principal_enabled(
        &mut self,
        principal_id: PrincipalId,
        is_enabled: bool,
    ) -> Result<PrincipalInfo, OperationError> {
        #[cfg(test)]
        let _protocol = crate::protocol_fault::Scope::enter("principal.enable");

        self.write(|transaction| {
            let mut current = principal(transaction, principal_id)?;

            changed(transaction.execute(
                "UPDATE principals SET is_enabled = ?2 WHERE principal_id = ?1",
                params![principal_id.as_bytes().as_slice(), is_enabled],
            )?)?;

            current.is_enabled = is_enabled;
            Ok(current)
        })
    }

    pub(crate) fn delete_principal(
        &mut self,
        principal_id: PrincipalId,
    ) -> Result<(), OperationError> {
        #[cfg(test)]
        let _protocol = crate::protocol_fault::Scope::enter("principal.delete");

        self.write(|transaction| {
            principal(transaction, principal_id)?;

            if transaction
                .prepare_cached("SELECT 1 FROM access_keys WHERE principal_id = ?1 LIMIT 1")?
                .exists([principal_id.as_bytes().as_slice()])?
            {
                return Err(OperationError::new(
                    OperationFailure::PrincipalHasAccessKeys,
                ));
            }

            changed(transaction.execute(
                "DELETE FROM principals WHERE principal_id = ?1",
                [principal_id.as_bytes().as_slice()],
            )?)
        })
    }

    pub(crate) fn put_principal_policy(
        &mut self,
        principal_id: PrincipalId,
        document: &str,
    ) -> Result<(), OperationError> {
        Policy::parse(document, PolicyKind::Identity)
            .map_err(|error| OperationError::caused(OperationFailure::InvalidArgument, error))?;

        let generation = StoreId::generate().map_err(MetadataError::Entropy)?;

        #[cfg(test)]
        let _protocol = crate::protocol_fault::Scope::enter("principal.policy.put");

        self.write(|transaction| {
            principal(transaction, principal_id)?;

            changed(transaction.execute(
                "UPDATE principals SET policy_generation = ?2, policy_json = ?3 WHERE principal_id = ?1",
                params![
                    principal_id.as_bytes().as_slice(),
                    generation.as_bytes().as_slice(),
                    document
                ],
            )?)
        })
    }

    pub(crate) fn get_principal_policy(
        &mut self,
        principal_id: PrincipalId,
    ) -> Result<GetPrincipalPolicyOutput, OperationError> {
        self.read(|transaction| {
            principal(transaction, principal_id)?;

            let (generation, document): (Option<Vec<u8>>, Option<String>) = transaction
                .query_row(
                    "SELECT policy_generation, policy_json FROM principals WHERE principal_id = ?1",
                    [principal_id.as_bytes().as_slice()],
                    |row| Ok((row.get(0)?, row.get(1)?)),
                )
                .map_err(decode_error)?;

            match (generation, document) {
                (None, None) => Err(OperationError::new(OperationFailure::NoSuchPrincipalPolicy)),
                (Some(generation), Some(document)) if generation.len() == 16 => {
                    Policy::parse(&document, PolicyKind::Identity)
                        .map_err(|_| MetadataError::Corrupt("invalid stored principal policy"))?;

                    Ok(GetPrincipalPolicyOutput { document })
                }
                _ => Err(MetadataError::Corrupt("inconsistent principal policy").into()),
            }
        })
    }

    pub(crate) fn delete_principal_policy(
        &mut self,
        principal_id: PrincipalId,
    ) -> Result<(), OperationError> {
        #[cfg(test)]
        let _protocol = crate::protocol_fault::Scope::enter("principal.policy.delete");

        self.write(|transaction| {
            principal(transaction, principal_id)?;

            changed(transaction.execute(
                "UPDATE principals SET policy_generation = NULL, policy_json = NULL WHERE principal_id = ?1",
                [principal_id.as_bytes().as_slice()],
            )?)
        })
    }

    pub(crate) fn create_access_key(
        &mut self,
        principal_id: PrincipalId,
    ) -> Result<CreateAccessKeyOutput, OperationError> {
        let prepared =
            PreparedAccessKey::generate(principal_id, now()?).map_err(MetadataError::Entropy)?;

        #[cfg(test)]
        let _protocol = crate::protocol_fault::Scope::enter("access_key.create");

        self.write(|transaction| {
            principal(transaction, principal_id)?;

            transaction.execute(
                "INSERT INTO access_keys (access_key_id, principal_id, secret_access_key, created_at_ms)
                 VALUES (?1, ?2, ?3, ?4)",
                params![
                    prepared.info.access_key_id.as_str(),
                    principal_id.as_bytes().as_slice(),
                    prepared.secret_access_key.expose(),
                    prepared.info.created_at.get()
                ],
            )?;

            Ok(())
        })?;

        // Only confirmed transaction success may disclose prepared secret material.
        Ok(CreateAccessKeyOutput {
            access_key: prepared.info,
            secret_access_key: prepared.secret_access_key,
        })
    }

    pub(crate) fn delete_access_key(
        &mut self,
        principal_id: PrincipalId,
        access_key_id: &AccessKeyId,
    ) -> Result<(), OperationError> {
        #[cfg(test)]
        let _protocol = crate::protocol_fault::Scope::enter("access_key.delete");

        self.write(|transaction| {
            principal(transaction, principal_id)?;

            let owner: Option<Vec<u8>> = transaction
                .query_row(
                    "SELECT principal_id FROM access_keys WHERE access_key_id = ?1",
                    [access_key_id.as_str()],
                    |row| row.get(0),
                )
                .optional()
                .map_err(decode_error)?;

            let Some(owner) = owner else {
                return Ok(());
            };

            let owner: [u8; 16] = owner
                .try_into()
                .map_err(|_| MetadataError::Corrupt("invalid access key owner"))?;

            if owner != *principal_id.as_bytes() {
                return Err(OperationError::new(OperationFailure::InvalidArgument));
            }

            changed(transaction.execute(
                "DELETE FROM access_keys WHERE access_key_id = ?1 AND principal_id = ?2",
                params![access_key_id.as_str(), principal_id.as_bytes().as_slice()],
            )?)
        })
    }
}

pub(super) fn owner_info(transaction: &Transaction<'_>) -> Result<OwnerInfo, OperationError> {
    let (account, canonical) = transaction
        .query_row(
            "SELECT owner_account_id, canonical_owner_id FROM store_metadata WHERE singleton_id = 1",
            [],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )
        .optional()
        .map_err(decode_error)?
        .ok_or(MetadataError::Corrupt("missing store owner identity"))?;

    OwnerInfo::new(account, canonical)
        .map_err(|_| MetadataError::Corrupt("invalid stored owner identity").into())
}

pub(super) fn principal(
    transaction: &Transaction<'_>,
    principal_id: PrincipalId,
) -> Result<PrincipalInfo, OperationError> {
    let mut statement = transaction.prepare_cached(SELECT_PRINCIPAL)?;

    let mut rows = statement.query([principal_id.as_bytes().as_slice()])?;

    let row = rows
        .next()?
        .ok_or_else(|| OperationError::new(OperationFailure::NoSuchPrincipal))?;

    decode_principal(row)
}

#[cfg(test)]
pub(in crate::metadata) fn principal_lookups(
    connection: &rusqlite::Connection,
) -> rusqlite::Result<i32> {
    let statement = connection.prepare_cached(SELECT_PRINCIPAL)?;
    Ok(statement.get_status(rusqlite::StatementStatus::Run))
}

fn decode_principal(row: &Row<'_>) -> Result<PrincipalInfo, OperationError> {
    let principal_id: Vec<u8> = row.get(0).map_err(decode_error)?;

    let principal_id = PrincipalId::from_bytes(
        principal_id
            .try_into()
            .map_err(|_| MetadataError::Corrupt("invalid principal identity"))?,
    );

    let name = PrincipalName::new(row.get(1).map_err(decode_error)?)
        .map_err(|_| MetadataError::Corrupt("invalid principal name"))?;

    let is_enabled = match row.get::<_, i64>(2).map_err(decode_error)? {
        0 => false,
        1 => true,
        _ => return Err(MetadataError::Corrupt("invalid principal enabled state").into()),
    };

    let created_at = EpochMillis::new(row.get(3).map_err(decode_error)?)
        .map_err(|_| MetadataError::Corrupt("invalid principal timestamp"))?;

    Ok(PrincipalInfo {
        principal_id,
        name,
        is_enabled,
        created_at,
    })
}

fn changed(count: usize) -> Result<(), OperationError> {
    if count != 1 {
        return Err(MetadataError::Corrupt("administration mutation lost its identity").into());
    }

    Ok(())
}

#[cfg(test)]
mod tests;
