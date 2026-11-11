//! Duplicate-preserving validation before policy projection into typed records.

use std::fmt;

use serde::de::{DeserializeSeed, Error, MapAccess, SeqAccess, Visitor};
use serde_json::{Map, Value};

use super::PolicyError;

const MAX_DEPTH: usize = 12;

pub(super) fn parse(document: &str) -> Result<Value, PolicyError> {
    if document.len() > 20 * 1024 {
        return Err(PolicyError::TooLarge);
    }

    let mut decoder = serde_json::Deserializer::from_str(document);

    let value = Node(0)
        .deserialize(&mut decoder)
        .map_err(|_| PolicyError::Invalid)?;

    decoder.end().map_err(|_| PolicyError::Invalid)?;
    Ok(value)
}

struct Node(usize);

impl<'de> DeserializeSeed<'de> for Node {
    type Value = Value;

    fn deserialize<D: serde::Deserializer<'de>>(self, decoder: D) -> Result<Value, D::Error> {
        if self.0 > MAX_DEPTH {
            return Err(D::Error::custom("policy nesting limit"));
        }

        decoder.deserialize_any(self)
    }
}

// Numbers are outside the string/bool policy grammar. The default numeric
// visitors reject them before typed projection or disclosure in an error.
impl<'de> Visitor<'de> for Node {
    type Value = Value;

    fn expecting(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("a bounded policy value")
    }

    fn visit_str<E: Error>(self, value: &str) -> Result<Value, E> {
        Ok(Value::String(value.into()))
    }

    fn visit_bool<E: Error>(self, value: bool) -> Result<Value, E> {
        Ok(Value::Bool(value))
    }

    fn visit_unit<E: Error>(self) -> Result<Value, E> {
        Ok(Value::Null)
    }

    fn visit_seq<A: SeqAccess<'de>>(self, mut sequence: A) -> Result<Value, A::Error> {
        let mut values = Vec::new();

        while let Some(value) = sequence.next_element_seed(Node(self.0 + 1))? {
            values.push(value);
        }

        Ok(Value::Array(values))
    }

    fn visit_map<A: MapAccess<'de>>(self, mut map: A) -> Result<Value, A::Error> {
        let mut values = Map::new();

        while let Some(key) = map.next_key::<String>()? {
            if values.contains_key(&key) {
                return Err(A::Error::custom("duplicate policy member"));
            }

            values.insert(key, map.next_value_seed(Node(self.0 + 1))?);
        }

        Ok(Value::Object(values))
    }
}
