use crate::EpochMillis;

use super::{ObjectValueError, valid_entity_tag_bytes};

#[cfg(test)]
mod tests;

/// Read preconditions before shared syntax and timestamp normalization.
#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct ReadConditionsInput {
    pub if_match: Option<String>,
    pub if_none_match: Option<String>,
    pub if_modified_since: Option<EpochMillis>,
    pub if_unmodified_since: Option<EpochMillis>,
}

/// Valid condition syntax, retaining absence and exact entity-tag spelling.
/// Selection evaluates these against one authorized object snapshot.
#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct ReadConditions(ReadConditionsInput);

/// An authorized selection can fail a cache check or an update precondition.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ReadConditionFailure {
    NotModified,
    PreconditionFailed,
}

impl ReadConditions {
    /// Normalize conditional instants down to whole HTTP seconds.
    ///
    /// # Errors
    /// Rejects invalid entity-tag lists and wildcard/list mixtures. Weak tags
    /// remain distinguishable from strong tags; validation does not compare them.
    pub fn new(mut input: ReadConditionsInput) -> Result<Self, ObjectValueError> {
        for value in [&input.if_match, &input.if_none_match]
            .into_iter()
            .flatten()
        {
            validate_entity_tag_list(value)?;
        }

        input.if_modified_since = input.if_modified_since.map(EpochMillis::floor_to_second);
        input.if_unmodified_since = input.if_unmodified_since.map(EpochMillis::floor_to_second);
        Ok(Self(input))
    }

    #[must_use]
    pub const fn fields(&self) -> &ReadConditionsInput {
        &self.0
    }

    /// Evaluate a current authorized object's validators at HTTP second precision.
    /// Entity-tag conditions take precedence over their corresponding dates.
    ///
    /// # Errors
    /// Returns a distinct cache hit or failed precondition, without opening a body.
    pub fn evaluate(&self, etag: &str, modified: EpochMillis) -> Result<(), ReadConditionFailure> {
        let fields = &self.0;
        let modified = modified.floor_to_second();

        if let Some(value) = &fields.if_match {
            if !matches_entity_tag(value, etag, false) {
                return Err(ReadConditionFailure::PreconditionFailed);
            }
        } else if fields
            .if_unmodified_since
            .is_some_and(|date| modified > date)
        {
            return Err(ReadConditionFailure::PreconditionFailed);
        }

        if let Some(value) = &fields.if_none_match {
            if matches_entity_tag(value, etag, true) {
                return Err(ReadConditionFailure::NotModified);
            }
        } else if fields
            .if_modified_since
            .is_some_and(|date| modified <= date)
        {
            return Err(ReadConditionFailure::NotModified);
        }

        Ok(())
    }
}

fn matches_entity_tag(value: &str, etag: &str, weak_comparison: bool) -> bool {
    let mut remaining = value.trim_matches([' ', '\t']);

    if remaining == "*" {
        return true;
    }

    loop {
        remaining = remaining.trim_start_matches([' ', '\t', ',']);

        if remaining.is_empty() {
            return false;
        }

        let (weak, tag) = remaining
            .strip_prefix("W/")
            .map_or((false, remaining), |tag| (true, tag));

        let Some((opaque, tail)) = tag.strip_prefix('"').and_then(|tag| tag.split_once('"')) else {
            // Construction rejects this syntax; no malformed validator can match.
            return false;
        };

        if weak && !weak_comparison {
            remaining = tail;
            continue;
        }

        if etag
            .strip_prefix('"')
            .and_then(|value| value.strip_suffix('"'))
            == Some(opaque)
        {
            return true;
        }

        remaining = tail;
    }
}

fn validate_entity_tag_list(value: &str) -> Result<(), ObjectValueError> {
    let mut remaining = value.trim_matches([' ', '\t']);

    if remaining == "*" {
        return Ok(());
    }

    let mut has_tag = false;

    loop {
        // HTTP list recipients ignore empty members; a comma inside quotes is data.
        remaining = remaining.trim_start_matches([' ', '\t', ',']);

        if remaining.is_empty() {
            return if has_tag {
                Ok(())
            } else {
                Err(ObjectValueError::ReadCondition)
            };
        }

        let tag = remaining.strip_prefix("W/").unwrap_or(remaining);

        let opaque = tag
            .strip_prefix('"')
            .ok_or(ObjectValueError::ReadCondition)?;

        let (opaque, tail) = opaque
            .split_once('"')
            .ok_or(ObjectValueError::ReadCondition)?;

        if !valid_entity_tag_bytes(opaque) {
            return Err(ObjectValueError::ReadCondition);
        }

        has_tag = true;
        remaining = tail.trim_start_matches([' ', '\t']);

        if !remaining.is_empty() && !remaining.starts_with(',') {
            return Err(ObjectValueError::ReadCondition);
        }
    }
}
