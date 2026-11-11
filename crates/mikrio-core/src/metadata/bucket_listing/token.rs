use rusqlite::Transaction;
use sha2::{Digest, Sha256};

use super::super::token::{TokenSigner, invalid_token};
use crate::OperationError;
use crate::object::{BucketName, ListBucketsRequest};

// Domain, version, store and digest of presence-preserving operation filters.
const PREFIX_LENGTH: usize = 4 + 1 + 16 + 32;
const MAX_PAYLOAD: usize = PREFIX_LENGTH + 63;

pub(super) struct Scope {
    signer: TokenSigner,
    prefix: Vec<u8>,
}

impl Scope {
    pub(super) fn load(
        transaction: &Transaction<'_>,
        request: &ListBucketsRequest,
    ) -> Result<Self, OperationError> {
        let signer = TokenSigner::load(transaction)?;
        let mut prefix = Vec::with_capacity(PREFIX_LENGTH);
        prefix.extend_from_slice(b"LBKT");
        prefix.push(1);
        prefix.extend_from_slice(&signer.store_id);
        let mut filters = Sha256::new();

        for value in [&request.prefix, &request.bucket_region] {
            filters.update([u8::from(value.is_some())]);

            if let Some(value) = value {
                let length = u64::try_from(value.len()).map_err(|_| invalid_token())?;
                filters.update(length.to_be_bytes());
                filters.update(value.as_bytes());
            }
        }

        prefix.extend_from_slice(&filters.finalize());
        Ok(Self { signer, prefix })
    }

    pub(super) fn encode(&self, name: &str) -> Result<String, OperationError> {
        BucketName::parse(name).map_err(|_| invalid_token())?;
        let mut payload = self.prefix.clone();
        payload.extend_from_slice(name.as_bytes());
        self.signer.encode(payload)
    }

    pub(super) fn decode(&self, token: Option<&str>) -> Result<Option<String>, OperationError> {
        let Some(token) = token else { return Ok(None) };
        let payload = self.signer.decode(token, MAX_PAYLOAD)?;

        if payload.len() <= PREFIX_LENGTH || !payload.starts_with(&self.prefix) {
            return Err(invalid_token());
        }

        let name = std::str::from_utf8(&payload[PREFIX_LENGTH..]).map_err(|_| invalid_token())?;
        BucketName::parse(name).map_err(|_| invalid_token())?;
        Ok(Some(name.to_owned()))
    }
}
