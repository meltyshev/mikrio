use std::collections::BTreeMap;
use std::fmt;
use std::sync::LazyLock;

use regex::Regex;
use serde::de::{MapAccess, Visitor};
use serde::{Deserialize, Deserializer};

use super::ObjectValueError;

#[cfg(test)]
mod tests;

const MAX_ENTRIES: usize = 10;
const JSON_MAX_BYTES: usize = 32_768;

#[expect(
    clippy::expect_used,
    reason = "the fixed pattern is covered by tag fixtures"
)]
static TAG_CHARACTERS: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"\A[\p{L}\p{Z}\p{N}_.:/=+\-@]*\z").expect("valid static tag character pattern")
});

/// Bounded, case-sensitive object tags in ascending UTF-8 key order.
#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct ObjectTags(BTreeMap<String, String>);

impl ObjectTags {
    /// # Errors
    /// Rejects duplicate keys, reserved keys, invalid characters or UTF-16 limits.
    pub fn new(
        entries: impl IntoIterator<Item = (String, String)>,
    ) -> Result<Self, ObjectValueError> {
        let mut tags = BTreeMap::new();

        for (key, value) in entries {
            if tags.len() == MAX_ENTRIES {
                return Err(ObjectValueError::TagCount);
            }

            let reserved = key
                .get(..4)
                .is_some_and(|prefix| prefix.eq_ignore_ascii_case("aws:"));

            if key.is_empty() || reserved || !valid_text(&key, 128) {
                return Err(ObjectValueError::TagKey);
            }

            if !valid_text(&value, 256) {
                return Err(ObjectValueError::TagValue);
            }

            if tags.insert(key, value).is_some() {
                return Err(ObjectValueError::TagDuplicate);
            }
        }

        Ok(Self(tags))
    }

    #[must_use]
    pub const fn entries(&self) -> &BTreeMap<String, String> {
        &self.0
    }

    /// Protocol count, bounded by the ten-entry invariant of every constructor.
    #[must_use]
    pub fn count(&self) -> u32 {
        self.0.keys().map(|_| 1_u32).sum()
    }

    #[must_use]
    pub fn into_entries(self) -> BTreeMap<String, String> {
        self.0
    }

    /// # Errors
    /// Rejects encoding failure or a value outside the persisted JSON envelope.
    pub fn to_json(&self) -> Result<String, ObjectValueError> {
        let encoded = serde_json::to_string(&self.0).map_err(|_| ObjectValueError::TagEncoding)?;

        if encoded.len() > JSON_MAX_BYTES {
            return Err(ObjectValueError::TagEncoding);
        }

        Ok(encoded)
    }

    /// # Errors
    /// Rejects invalid JSON, duplicates and invalid tag values without normalizing.
    /// Callers reading storage must classify this failure as corruption.
    pub fn from_json(encoded: &str) -> Result<Self, ObjectValueError> {
        if encoded.len() > JSON_MAX_BYTES {
            return Err(ObjectValueError::TagEncoding);
        }

        serde_json::from_str(encoded).map_err(|_| ObjectValueError::TagEncoding)
    }
}

impl<'de> Deserialize<'de> for ObjectTags {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        struct TagsVisitor;

        impl<'de> Visitor<'de> for TagsVisitor {
            type Value = ObjectTags;

            fn expecting(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
                formatter.write_str("an object tag map")
            }

            fn visit_map<M: MapAccess<'de>>(self, mut map: M) -> Result<Self::Value, M::Error> {
                let mut entries = Vec::new();

                while let Some(entry) = map.next_entry::<String, String>()? {
                    if entries.len() == MAX_ENTRIES {
                        return Err(serde::de::Error::custom("object tags exceed ten entries"));
                    }

                    entries.push(entry);
                }

                ObjectTags::new(entries).map_err(serde::de::Error::custom)
            }
        }

        deserializer.deserialize_map(TagsVisitor)
    }
}

fn valid_text(value: &str, max_units: usize) -> bool {
    value.encode_utf16().take(max_units + 1).count() <= max_units && TAG_CHARACTERS.is_match(value)
}
