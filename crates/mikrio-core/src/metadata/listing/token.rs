use rusqlite::Transaction;
use sha2::{Digest, Sha256};

use super::super::token::{TokenSigner, invalid_token};
use crate::OperationError;
use crate::object::ListObjectsRequest;

// Domain, version, store, immutable bucket, filter digest, cursor kind/length.
const PREFIX_LENGTH: usize = 4 + 1 + 16 + 16 + 32;
const MAX_PAYLOAD: usize = PREFIX_LENGTH + 3 + 1024;

pub(super) enum Cursor {
    Inclusive(Vec<u8>),
    Exclusive(Vec<u8>),
}

impl Cursor {
    pub(super) fn bytes(&self) -> &[u8] {
        match self {
            Self::Inclusive(bytes) | Self::Exclusive(bytes) => bytes,
        }
    }
}

pub(super) struct Scope {
    signer: TokenSigner,
    prefix: Vec<u8>,
}

impl Scope {
    pub(super) fn load(
        transaction: &Transaction<'_>,
        bucket_id: [u8; 16],
        request: &ListObjectsRequest,
    ) -> Result<Self, OperationError> {
        let signer = TokenSigner::load(transaction)?;
        let mut prefix = Vec::with_capacity(PREFIX_LENGTH);
        prefix.extend_from_slice(b"LOV2");
        prefix.push(1);
        prefix.extend_from_slice(&signer.store_id);
        prefix.extend_from_slice(&bucket_id);
        let mut filters = Sha256::new();

        for value in [&request.prefix, &request.delimiter] {
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

    pub(super) fn encode(&self, cursor: &Cursor) -> Result<String, OperationError> {
        if cursor.bytes().is_empty() || cursor.bytes().len() > 1024 {
            return Err(invalid_token());
        }

        let mut payload = self.prefix.clone();
        payload.push(u8::from(matches!(cursor, Cursor::Exclusive(_))));
        let length = u16::try_from(cursor.bytes().len()).map_err(|_| invalid_token())?;
        payload.extend_from_slice(&length.to_be_bytes());
        payload.extend_from_slice(cursor.bytes());
        self.signer.encode(payload)
    }

    pub(super) fn decode(&self, token: Option<&str>) -> Result<Option<Cursor>, OperationError> {
        let Some(token) = token else { return Ok(None) };
        let payload = self.signer.decode(token, MAX_PAYLOAD)?;

        if payload.len() <= PREFIX_LENGTH + 3 || !payload.starts_with(&self.prefix) {
            return Err(invalid_token());
        }

        let length = u16::from_be_bytes([payload[PREFIX_LENGTH + 1], payload[PREFIX_LENGTH + 2]]);
        let bytes = &payload[PREFIX_LENGTH + 3..];

        if bytes.len() != usize::from(length) {
            return Err(invalid_token());
        }

        match payload[PREFIX_LENGTH] {
            0 => Ok(Some(Cursor::Inclusive(bytes.to_vec()))),
            1 => Ok(Some(Cursor::Exclusive(bytes.to_vec()))),
            _ => Err(invalid_token()),
        }
    }
}
