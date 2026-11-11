//! Store-local authenticated cursor envelopes. Callers own operation/scope bytes.

use base64_simd::URL_SAFE_NO_PAD;
use hmac::{Hmac, KeyInit, Mac};
use rusqlite::Transaction;
use sha2::Sha256;

use super::{MetadataError, decode_error};
use crate::{OperationError, OperationFailure};

const MAC_LENGTH: usize = 32;

pub(super) struct TokenSigner {
    pub store_id: [u8; 16],
    key: [u8; 32],
}

impl TokenSigner {
    pub(super) fn load(transaction: &Transaction<'_>) -> Result<Self, OperationError> {
        let (store_id, key): (Vec<u8>, Vec<u8>) = transaction
            .query_row(
                "SELECT store_id, continuation_token_key FROM store_metadata WHERE singleton_id = 1",
                [],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .map_err(decode_error)?;

        Ok(Self {
            store_id: store_id
                .try_into()
                .map_err(|_| MetadataError::Corrupt("invalid token store identity"))?,
            key: key
                .try_into()
                .map_err(|_| MetadataError::Corrupt("invalid token signing key"))?,
        })
    }

    pub(super) fn encode(&self, mut payload: Vec<u8>) -> Result<String, OperationError> {
        let mut mac = self.mac()?;
        mac.update(&payload);
        payload.extend_from_slice(&mac.finalize().into_bytes());
        Ok(URL_SAFE_NO_PAD.encode_to_string(payload))
    }

    pub(super) fn decode(
        &self,
        token: &str,
        max_payload: usize,
    ) -> Result<Vec<u8>, OperationError> {
        let max_bytes = max_payload
            .checked_add(MAC_LENGTH)
            .ok_or_else(invalid_token)?;

        let max_encoded = max_bytes
            .div_ceil(3)
            .checked_mul(4)
            .ok_or_else(invalid_token)?;

        if token.len() > max_encoded {
            return Err(invalid_token());
        }

        let mut bytes = URL_SAFE_NO_PAD
            .decode_to_vec(token)
            .map_err(|_| invalid_token())?;

        if bytes.len() <= MAC_LENGTH || bytes.len() > max_bytes {
            return Err(invalid_token());
        }

        let payload_length = bytes.len() - MAC_LENGTH;
        let (payload, signature) = bytes.split_at(payload_length);
        let mut mac = self.mac()?;
        mac.update(payload);
        mac.verify_slice(signature).map_err(|_| invalid_token())?;
        bytes.truncate(payload_length);
        Ok(bytes)
    }

    fn mac(&self) -> Result<Hmac<Sha256>, OperationError> {
        Hmac::<Sha256>::new_from_slice(&self.key)
            .map_err(|_| MetadataError::Corrupt("invalid token signing key").into())
    }
}

pub(super) fn invalid_token() -> OperationError {
    OperationError::new(OperationFailure::InvalidArgument)
}
