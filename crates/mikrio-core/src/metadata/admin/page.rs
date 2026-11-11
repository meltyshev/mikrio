//! Bounded, authenticated live-keyset cursors. Tokens carry no credential material.

use rusqlite::{Transaction, params};

use super::{decode_principal, principal};
use crate::access::{
    AccessKeyId, AccessKeyInfo, AdminPageSize, ListAccessKeysOutput, ListPrincipalsOutput,
    PrincipalId,
};
use crate::metadata::token::{TokenSigner, invalid_token};
use crate::metadata::{MetadataDatabase, MetadataError, decode_error};
use crate::{EpochMillis, OperationError};

// Version, operation, store ID, principal scope, cursor length, bounded cursor, MAC.
const PREFIX_LENGTH: usize = 35;
const MAX_CURSOR: usize = 128;

impl MetadataDatabase {
    pub(crate) fn list_principals(
        &mut self,
        max_results: AdminPageSize,
        continuation_token: Option<&str>,
    ) -> Result<ListPrincipalsOutput, OperationError> {
        self.read(|transaction| {
            let scope = Scope::load(transaction, None)?;
            let cursor = scope.decode(continuation_token)?;

            if !cursor.is_empty() && cursor.len() != 16 {
                return Err(invalid_token());
            }

            let mut statement = transaction.prepare_cached(
                "SELECT principal_id, name, is_enabled, created_at_ms FROM principals
                 WHERE principal_id > ?1 ORDER BY principal_id LIMIT ?2",
            )?;

            let limit = usize::try_from(max_results.get()).map_err(|_| invalid_token())?;
            let mut rows = statement.query(params![cursor, max_results.get() + 1])?;
            let mut principals: Vec<crate::access::PrincipalInfo> = Vec::with_capacity(limit);

            while let Some(row) = rows.next()? {
                if principals.len() == limit {
                    // The lookahead row is validated even though it is not returned.
                    decode_principal(row)?;
                    let last = principals.last().ok_or_else(invalid_token)?;
                    let token = scope.encode(last.principal_id.as_bytes())?;

                    return Ok(ListPrincipalsOutput {
                        principals,
                        next_continuation_token: Some(token),
                    });
                }

                principals.push(decode_principal(row)?);
            }

            Ok(ListPrincipalsOutput {
                principals,
                next_continuation_token: None,
            })
        })
    }

    pub(crate) fn list_access_keys(
        &mut self,
        principal_id: PrincipalId,
        max_results: AdminPageSize,
        continuation_token: Option<&str>,
    ) -> Result<ListAccessKeysOutput, OperationError> {
        self.read(|transaction| {
            principal(transaction, principal_id)?;
            let scope = Scope::load(transaction, Some(principal_id))?;
            let cursor = scope.decode(continuation_token)?;
            let cursor = std::str::from_utf8(&cursor).map_err(|_| invalid_token())?;

            if !cursor.is_empty() {
                AccessKeyId::parse(cursor.into()).map_err(|_| invalid_token())?;
            }

            let mut statement = transaction.prepare_cached(
                "SELECT access_key_id, principal_id, created_at_ms FROM access_keys
                 WHERE principal_id = ?1 AND access_key_id > ?2 ORDER BY access_key_id LIMIT ?3",
            )?;

            let limit = usize::try_from(max_results.get()).map_err(|_| invalid_token())?;

            let mut rows = statement.query(params![
                principal_id.as_bytes().as_slice(),
                cursor,
                max_results.get() + 1
            ])?;

            let mut access_keys: Vec<AccessKeyInfo> = Vec::with_capacity(limit);

            while let Some(row) = rows.next()? {
                let access_key_id = AccessKeyId::parse(row.get(0).map_err(decode_error)?)
                    .map_err(|_| MetadataError::Corrupt("invalid stored access key identity"))?;

                let owner: Vec<u8> = row.get(1).map_err(decode_error)?;

                if owner.as_slice() != principal_id.as_bytes() {
                    return Err(MetadataError::Corrupt("invalid stored access key owner").into());
                }

                let created_at = EpochMillis::new(row.get(2).map_err(decode_error)?)
                    .map_err(|_| MetadataError::Corrupt("invalid stored access key timestamp"))?;

                if access_keys.len() == limit {
                    let last = access_keys.last().ok_or_else(invalid_token)?;
                    let token = scope.encode(last.access_key_id.as_str().as_bytes())?;

                    return Ok(ListAccessKeysOutput {
                        access_keys,
                        next_continuation_token: Some(token),
                    });
                }

                access_keys.push(AccessKeyInfo {
                    access_key_id,
                    principal_id,
                    created_at,
                });
            }

            Ok(ListAccessKeysOutput {
                access_keys,
                next_continuation_token: None,
            })
        })
    }
}

struct Scope {
    signer: TokenSigner,
    principal_id: Option<PrincipalId>,
}

impl Scope {
    fn load(
        transaction: &Transaction<'_>,
        principal_id: Option<PrincipalId>,
    ) -> Result<Self, OperationError> {
        Ok(Self {
            signer: TokenSigner::load(transaction)?,
            principal_id,
        })
    }

    fn prefix(&self) -> Vec<u8> {
        let mut bytes = Vec::with_capacity(PREFIX_LENGTH + MAX_CURSOR);
        bytes.extend_from_slice(&[1, if self.principal_id.is_some() { 2 } else { 1 }]);
        bytes.extend_from_slice(&self.signer.store_id);

        bytes.extend_from_slice(
            self.principal_id
                .as_ref()
                .map_or(&[0; 16], |principal| principal.as_bytes().as_slice()),
        );

        bytes
    }

    fn encode(&self, cursor: &[u8]) -> Result<String, OperationError> {
        if cursor.is_empty() || cursor.len() > MAX_CURSOR {
            return Err(invalid_token());
        }

        let mut bytes = self.prefix();
        bytes.push(u8::try_from(cursor.len()).map_err(|_| invalid_token())?);
        bytes.extend_from_slice(cursor);
        self.signer.encode(bytes)
    }

    fn decode(&self, token: Option<&str>) -> Result<Vec<u8>, OperationError> {
        let Some(token) = token else {
            return Ok(Vec::new());
        };

        let payload = self.signer.decode(token, PREFIX_LENGTH + MAX_CURSOR)?;

        if payload.len() <= PREFIX_LENGTH {
            return Err(invalid_token());
        }

        if payload[..PREFIX_LENGTH - 1] != self.prefix()
            || usize::from(payload[PREFIX_LENGTH - 1]) != payload.len() - PREFIX_LENGTH
            || payload.len() - PREFIX_LENGTH > MAX_CURSOR
        {
            return Err(invalid_token());
        }

        Ok(payload[PREFIX_LENGTH..].to_vec())
    }
}
