use std::collections::BTreeSet;

use serde_json::Value;

use super::{Context, PolicyError, pattern};

enum Operator {
    Equals,
    NotEquals,
    Like,
    NotLike,
    Bool,
    Null,
}

enum Quantifier {
    Single,
    All,
    Any,
}

pub(super) struct Condition {
    key: String,
    values: Vec<String>,
    operator: Operator,
    quantifier: Quantifier,
    if_exists: bool,
}

impl Condition {
    pub(super) fn parse(value: &Value) -> Result<Vec<Self>, PolicyError> {
        let operators = value.as_object().ok_or(PolicyError::Invalid)?;

        if operators.is_empty() {
            return Err(PolicyError::Invalid);
        }

        let mut conditions = Vec::new();

        for (operator_name, entries) in operators {
            let entries = entries.as_object().ok_or(PolicyError::Invalid)?;

            if entries.is_empty() {
                return Err(PolicyError::Invalid);
            }

            let mut keys = BTreeSet::new();

            for (condition_key, values) in entries {
                let condition = Self::new(operator_name, condition_key, values)?;

                if !keys.insert(condition.key.clone()) {
                    return Err(PolicyError::Invalid);
                }

                conditions.push(condition);
            }
        }

        Ok(conditions)
    }

    pub(super) fn matches(&self, context: &Context) -> bool {
        let actual = context.0.get(&self.key);

        if matches!(self.operator, Operator::Null) {
            return self
                .values
                .iter()
                .any(|value| (value == "true") == actual.is_none());
        }

        let negated = matches!(self.operator, Operator::NotEquals | Operator::NotLike);

        let Some(actual) = actual else {
            return match self.quantifier {
                Quantifier::All => true,
                Quantifier::Any => false,
                Quantifier::Single => self.if_exists || negated,
            };
        };

        let comparison = |actual: &String| {
            let matched = self.values.iter().any(|expected| match self.operator {
                Operator::Like | Operator::NotLike => pattern::matches(expected, actual),
                _ => expected == actual,
            });

            matched != negated
        };

        match self.quantifier {
            Quantifier::Single | Quantifier::Any => actual.iter().any(comparison),
            Quantifier::All => actual.iter().all(comparison),
        }
    }

    fn new(operator_name: &str, condition_key: &str, value: &Value) -> Result<Self, PolicyError> {
        let (quantifier, operator_name) =
            if let Some(name) = operator_name.strip_prefix("ForAllValues:") {
                (Quantifier::All, name)
            } else if let Some(name) = operator_name.strip_prefix("ForAnyValue:") {
                (Quantifier::Any, name)
            } else {
                (Quantifier::Single, operator_name)
            };

        let (operator_name, if_exists) = operator_name
            .strip_suffix("IfExists")
            .map_or((operator_name, false), |name| (name, true));

        let operator = match operator_name {
            "StringEquals" => Operator::Equals,
            "StringNotEquals" => Operator::NotEquals,
            "StringLike" => Operator::Like,
            "StringNotLike" => Operator::NotLike,
            "Bool" => Operator::Bool,
            "Null" if !if_exists => Operator::Null,
            _ => return Err(PolicyError::Unsupported),
        };

        let key = key(condition_key)?;
        let is_set = key == "s3:requestobjecttagkeys";

        if !matches!(quantifier, Quantifier::Single)
            && (!is_set || if_exists || matches!(operator, Operator::Bool | Operator::Null))
        {
            return Err(PolicyError::Unsupported);
        }

        if is_set && matches!(quantifier, Quantifier::Single) && !matches!(operator, Operator::Null)
        {
            return Err(PolicyError::Unsupported);
        }

        let is_boolean = key == "s3:objectcreationoperation";

        if !matches!(operator, Operator::Null) && (is_boolean != matches!(operator, Operator::Bool))
        {
            return Err(PolicyError::Unsupported);
        }

        let values = condition_values(value, matches!(operator, Operator::Bool | Operator::Null))?;

        let values_are_valid = match operator {
            Operator::Bool | Operator::Null => values
                .iter()
                .all(|value| matches!(value.as_str(), "true" | "false")),
            _ => true,
        };

        if !values_are_valid {
            return Err(PolicyError::Invalid);
        }

        if values.iter().any(|value| value.contains("${")) {
            return Err(PolicyError::Unsupported);
        }

        Ok(Self {
            key,
            values,
            operator,
            quantifier,
            if_exists,
        })
    }
}

pub(super) fn key(value: &str) -> Result<String, PolicyError> {
    // Bound before allocating/case folding even for a forged adapter context.
    if value.len() > "s3:existingobjecttag/".len() + 128 {
        return Err(PolicyError::Unsupported);
    }

    let normalized = value.to_ascii_lowercase();

    if matches!(
        normalized.as_str(),
        "s3:prefix"
            | "s3:delimiter"
            | "s3:if-match"
            | "s3:if-none-match"
            | "s3:objectcreationoperation"
            | "s3:requestobjecttagkeys"
    ) {
        return Ok(normalized);
    }

    for prefix in ["s3:existingobjecttag/", "s3:requestobjecttag/"] {
        let Some(suffix) = normalized.strip_prefix(prefix) else {
            continue;
        };

        // ASCII case aliases share one condition key. Unicode suffix matching
        // is unsupported; stored Unicode tags are never normalized or discarded.
        if suffix.is_empty()
            || !suffix.is_ascii()
            || suffix.encode_utf16().count() > 128
            || suffix.contains(['*', '?', '$'])
        {
            return Err(PolicyError::Unsupported);
        }

        return Ok(normalized);
    }

    Err(PolicyError::Unsupported)
}

fn condition_values(value: &Value, accepts_booleans: bool) -> Result<Vec<String>, PolicyError> {
    match value {
        Value::String(value) => Ok(vec![value.clone()]),
        Value::Bool(value) if accepts_booleans => Ok(vec![value.to_string()]),
        Value::Array(values) if !values.is_empty() => values
            .iter()
            .map(|value| match value {
                Value::String(value) => Ok(value.clone()),
                Value::Bool(value) if accepts_booleans => Ok(value.to_string()),
                _ => Err(PolicyError::Invalid),
            })
            .collect(),
        _ => Err(PolicyError::Invalid),
    }
}
