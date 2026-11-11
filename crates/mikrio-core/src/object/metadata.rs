use std::collections::BTreeMap;
use std::fmt;

use serde::de::{MapAccess, Visitor};
use serde::{Deserialize, Deserializer};

use super::{METADATA_MAX_BYTES, METADATA_MAX_ENTRIES, ObjectValueError};

const JSON_MAX_BYTES: usize = 65_536;

/// Canonical user metadata, validated before encoding and after decoding.
#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct UserMetadata(BTreeMap<String, String>);

impl UserMetadata {
    /// # Errors
    /// Rejects invalid keys, control characters, duplicate folded keys or overflow.
    pub fn new(
        entries: impl IntoIterator<Item = (String, String)>,
    ) -> Result<Self, ObjectValueError> {
        let mut metadata = BTreeMap::new();
        let mut remaining = METADATA_MAX_BYTES;

        for (key, value) in entries {
            if metadata.len() == METADATA_MAX_ENTRIES {
                return Err(ObjectValueError::MetadataCount);
            }

            if key.is_empty() || !key.bytes().all(is_token_byte) {
                return Err(ObjectValueError::MetadataKey);
            }

            if value.chars().any(char::is_control) {
                return Err(ObjectValueError::MetadataValue);
            }

            remaining = remaining
                .checked_sub(key.len())
                .and_then(|remaining| remaining.checked_sub(value.len()))
                .ok_or(ObjectValueError::MetadataSize)?;

            if metadata.insert(key.to_ascii_lowercase(), value).is_some() {
                return Err(ObjectValueError::MetadataDuplicate);
            }
        }

        Ok(Self(metadata))
    }

    #[must_use]
    pub const fn entries(&self) -> &BTreeMap<String, String> {
        &self.0
    }

    #[must_use]
    pub fn into_entries(self) -> BTreeMap<String, String> {
        self.0
    }

    /// # Errors
    /// Rejects encoding failure or a value outside the persisted JSON envelope.
    pub fn to_json(&self) -> Result<String, ObjectValueError> {
        let encoded =
            serde_json::to_string(&self.0).map_err(|_| ObjectValueError::MetadataEncoding)?;

        if encoded.len() > JSON_MAX_BYTES {
            return Err(ObjectValueError::MetadataEncoding);
        }

        Ok(encoded)
    }

    /// # Errors
    /// Rejects invalid JSON, duplicate/noncanonical keys and invalid metadata.
    /// Callers reading storage must classify this failure as corruption.
    pub fn from_json(encoded: &str) -> Result<Self, ObjectValueError> {
        if encoded.len() > JSON_MAX_BYTES {
            return Err(ObjectValueError::MetadataEncoding);
        }

        serde_json::from_str(encoded).map_err(|_| ObjectValueError::MetadataEncoding)
    }
}

impl<'de> Deserialize<'de> for UserMetadata {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        struct MetadataVisitor;

        impl<'de> Visitor<'de> for MetadataVisitor {
            type Value = UserMetadata;

            fn expecting(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
                formatter.write_str("a canonical metadata object")
            }

            fn visit_map<M: MapAccess<'de>>(self, mut map: M) -> Result<Self::Value, M::Error> {
                let mut entries = BTreeMap::new();
                let mut remaining = METADATA_MAX_BYTES;

                while let Some((key, value)) = map.next_entry::<String, String>()? {
                    if entries.len() == METADATA_MAX_ENTRIES {
                        return Err(serde::de::Error::custom("metadata exceeds its entry limit"));
                    }

                    if key.bytes().any(|byte| byte.is_ascii_uppercase()) {
                        return Err(serde::de::Error::custom("noncanonical metadata key"));
                    }

                    remaining = remaining
                        .checked_sub(key.len())
                        .and_then(|remaining| remaining.checked_sub(value.len()))
                        .ok_or_else(|| {
                            serde::de::Error::custom("metadata exceeds its byte limit")
                        })?;

                    if entries.insert(key, value).is_some() {
                        return Err(serde::de::Error::custom("duplicate metadata member"));
                    }
                }

                UserMetadata::new(entries).map_err(serde::de::Error::custom)
            }
        }

        deserializer.deserialize_map(MetadataVisitor)
    }
}

fn is_token_byte(byte: u8) -> bool {
    byte.is_ascii_alphanumeric() || b"!#$%&'*+-.^_`|~".contains(&byte)
}
