//! Validated retention configurations and age arithmetic, independent of cleanup authority.

use std::collections::BTreeSet;
use std::fmt;

use crate::EpochMillis;

const MILLIS_PER_DAY: i64 = 86_400_000;
pub(crate) const MAX_RULES: usize = 1_000;

/// Preserve the received filter representation as well as its matching prefix.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum LifecycleFilter {
    All,
    Prefix(String),
    LegacyPrefix(String),
}

impl LifecycleFilter {
    #[must_use]
    pub fn prefix(&self) -> &str {
        match self {
            Self::All => "",
            Self::Prefix(prefix) | Self::LegacyPrefix(prefix) => prefix,
        }
    }
}

/// Owned supported rule fields. Missing or empty IDs are assigned before storage.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct LifecycleRule {
    pub id: Option<String>,
    pub is_enabled: bool,
    pub filter: LifecycleFilter,
    pub expiration_days: Option<u32>,
    pub abort_incomplete_multipart_days: Option<u32>,
}

/// A completely validated ordered rule set; absence is represented separately.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct LifecycleConfiguration(Vec<LifecycleRule>);

/// Invalid configuration, day count or retention date. No candidate is held.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum LifecycleError {
    Configuration,
    Days,
    Time,
}

impl fmt::Display for LifecycleError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(match self {
            Self::Configuration => "invalid or oversized lifecycle configuration",
            Self::Days => "lifecycle days must be a positive protocol integer",
            Self::Time => "lifecycle eligibility time is outside the supported range",
        })
    }
}

impl std::error::Error for LifecycleError {}

impl LifecycleConfiguration {
    /// Validate the whole supported rule set before worker admission.
    ///
    /// # Errors
    /// Rejects empty/oversized sets, duplicate nonempty IDs, invalid XML text,
    /// oversized prefixes or IDs, missing actions and invalid day integers.
    pub fn new(rules: Vec<LifecycleRule>) -> Result<Self, LifecycleError> {
        if rules.is_empty() || rules.len() > MAX_RULES {
            return Err(LifecycleError::Configuration);
        }

        let mut ids = BTreeSet::new();

        for rule in &rules {
            if let Some(id) = &rule.id {
                if id.chars().take(256).count() > 255 || !xml_text(id) {
                    return Err(LifecycleError::Configuration);
                }

                if !id.is_empty() && !ids.insert(id) {
                    return Err(LifecycleError::Configuration);
                }
            }

            let prefix = rule.filter.prefix();

            if prefix.len() > 1_024 || !xml_text(prefix) {
                return Err(LifecycleError::Configuration);
            }

            if rule.expiration_days.is_none() && rule.abort_incomplete_multipart_days.is_none() {
                return Err(LifecycleError::Configuration);
            }

            for days in [rule.expiration_days, rule.abort_incomplete_multipart_days]
                .into_iter()
                .flatten()
            {
                validate_days(days)?;
            }
        }

        Ok(Self(rules))
    }

    #[must_use]
    pub fn rules(&self) -> &[LifecycleRule] {
        &self.0
    }

    #[must_use]
    pub fn into_rules(self) -> Vec<LifecycleRule> {
        self.0
    }
}

/// Compute object expiration from its persisted age anchor, rounded to the
/// following UTC midnight. The cleanup clock never becomes the object's age.
/// This supplies no authority to delete a mapping or reclaim protected bytes.
///
/// # Errors
/// Rejects zero or out-of-range protocol integers and arithmetic/date overflow.
/// A caller must leave the candidate untouched when evaluation fails.
pub fn expiration_at(anchor: EpochMillis, days: u32) -> Result<EpochMillis, LifecycleError> {
    validate_days(days)?;

    let duration = i64::from(days)
        .checked_mul(MILLIS_PER_DAY)
        .ok_or(LifecycleError::Time)?;

    let age = anchor
        .get()
        .checked_add(duration)
        .ok_or(LifecycleError::Time)?;

    let midnight = age
        .div_euclid(MILLIS_PER_DAY)
        .checked_add(1)
        .and_then(|day| day.checked_mul(MILLIS_PER_DAY))
        .ok_or(LifecycleError::Time)?;

    EpochMillis::new(midnight).map_err(|_| LifecycleError::Time)
}

/// Incomplete-upload eligibility at the strictly following UTC midnight,
/// anchored at initiation, under the local compatibility contract.
/// This arithmetic alone does not authorize retirement.
///
/// # Errors
/// Rejects invalid days and arithmetic/date overflow without authorizing abort.
pub fn abort_at(initiated: EpochMillis, days: u32) -> Result<EpochMillis, LifecycleError> {
    expiration_at(initiated, days)
}

fn validate_days(days: u32) -> Result<(), LifecycleError> {
    if days == 0 || i32::try_from(days).is_err() {
        return Err(LifecycleError::Days);
    }

    Ok(())
}

fn xml_text(value: &str) -> bool {
    value.chars().all(|character| {
        matches!(character, '\t' | '\n' | '\r' | '\u{20}'..='\u{d7ff}'
            | '\u{e000}'..='\u{fffd}' | '\u{10000}'..='\u{10ffff}')
    })
}

#[cfg(test)]
mod tests;
