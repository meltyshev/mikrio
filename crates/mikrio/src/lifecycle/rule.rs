//! Checked native retention values; wire-only legacy filters stay in the codec.

use mikrio_core::lifecycle as core;

use crate::StorageError;

/// Whether a configured rule currently authorizes retention.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum LifecycleStatus {
    /// Apply the configured actions to matching keys.
    Enabled,
    /// Preserve the configuration without applying its actions.
    Disabled,
}

impl LifecycleStatus {
    /// Stable AWS spelling shared by host bindings.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Enabled => "Enabled",
            Self::Disabled => "Disabled",
        }
    }
}

/// Modern prefix filter. Absence and an explicit empty prefix remain distinct.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct LifecycleFilter {
    prefix: Option<String>,
}

impl LifecycleFilter {
    /// Validate a literal optional prefix, without path normalization.
    ///
    /// # Errors
    /// Rejects prefixes exceeding 1,024 UTF-8 bytes or containing invalid XML text.
    pub fn new(prefix: Option<String>) -> Result<Self, StorageError> {
        let value = Self { prefix };

        core::LifecycleConfiguration::new(vec![core::LifecycleRule {
            id: None,
            is_enabled: false,
            filter: value.to_core(),
            expiration_days: Some(1),
            abort_incomplete_multipart_days: None,
        }])
        .map_err(super::invalid)?;

        Ok(value)
    }

    /// Optional literal prefix; `None` represents an empty modern Filter.
    #[must_use]
    pub fn prefix(&self) -> Option<&str> {
        self.prefix.as_deref()
    }

    fn to_core(&self) -> core::LifecycleFilter {
        self.prefix
            .clone()
            .map_or(core::LifecycleFilter::All, core::LifecycleFilter::Prefix)
    }
}

/// Expiration days from the object's persisted lifecycle age anchor.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct LifecycleExpiration {
    days: u32,
}

impl LifecycleExpiration {
    /// Check the positive signed-32-bit protocol range.
    ///
    /// # Errors
    /// Returns `InvalidArgument` for zero or days above `i32::MAX`.
    pub fn new(days: u32) -> Result<Self, StorageError> {
        validate_days(days)?;

        Ok(Self { days })
    }

    /// Configured number of days, before UTC rounding.
    #[must_use]
    pub const fn days(self) -> u32 {
        self.days
    }
}

/// Incomplete-upload abort days from the original initiation time.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct AbortIncompleteMultipartUpload {
    days_after_initiation: u32,
}

impl AbortIncompleteMultipartUpload {
    /// Check the positive signed-32-bit protocol range.
    ///
    /// # Errors
    /// Returns `InvalidArgument` for zero or days above `i32::MAX`.
    pub fn new(days_after_initiation: u32) -> Result<Self, StorageError> {
        validate_days(days_after_initiation)?;

        Ok(Self {
            days_after_initiation,
        })
    }

    /// Configured age from initiation, before UTC rounding.
    #[must_use]
    pub const fn days_after_initiation(self) -> u32 {
        self.days_after_initiation
    }
}

fn validate_days(days: u32) -> Result<(), StorageError> {
    if days == 0 || i32::try_from(days).is_err() {
        return Err(super::invalid(core::LifecycleError::Days));
    }

    Ok(())
}

/// One supported ordered rule. At least one action is required, even if disabled.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct LifecycleRule {
    id: Option<String>,
    status: LifecycleStatus,
    filter: LifecycleFilter,
    expiration: Option<LifecycleExpiration>,
    abort_incomplete_multipart_upload: Option<AbortIncompleteMultipartUpload>,
}

impl LifecycleRule {
    /// Start with the required rule status and modern filter.
    #[must_use]
    pub fn builder(status: LifecycleStatus, filter: LifecycleFilter) -> LifecycleRuleBuilder {
        LifecycleRuleBuilder {
            rule: Self {
                id: None,
                status,
                filter,
                expiration: None,
                abort_incomplete_multipart_upload: None,
            },
        }
    }

    /// Optional input ID; persisted results always have a generated or supplied ID.
    #[must_use]
    pub fn id(&self) -> Option<&str> {
        self.id.as_deref()
    }

    /// Whether this rule is active.
    #[must_use]
    pub const fn status(&self) -> LifecycleStatus {
        self.status
    }

    /// Modern prefix filter, preserving absent versus present-empty prefix.
    #[must_use]
    pub const fn filter(&self) -> &LifecycleFilter {
        &self.filter
    }

    /// Optional expiration action.
    #[must_use]
    pub const fn expiration(&self) -> Option<LifecycleExpiration> {
        self.expiration
    }

    /// Optional incomplete-upload abort action.
    #[must_use]
    pub const fn abort_incomplete_multipart_upload(
        &self,
    ) -> Option<AbortIncompleteMultipartUpload> {
        self.abort_incomplete_multipart_upload
    }

    pub(super) fn from_core(rule: core::LifecycleRule) -> Self {
        Self {
            id: rule.id,
            status: if rule.is_enabled {
                LifecycleStatus::Enabled
            } else {
                LifecycleStatus::Disabled
            },
            filter: LifecycleFilter {
                prefix: match rule.filter {
                    core::LifecycleFilter::All => None,
                    core::LifecycleFilter::Prefix(prefix)
                    | core::LifecycleFilter::LegacyPrefix(prefix) => Some(prefix),
                },
            },
            expiration: rule
                .expiration_days
                .map(|days| LifecycleExpiration { days }),
            abort_incomplete_multipart_upload: rule.abort_incomplete_multipart_days.map(
                |days_after_initiation| AbortIncompleteMultipartUpload {
                    days_after_initiation,
                },
            ),
        }
    }

    pub(super) fn into_core(self) -> core::LifecycleRule {
        core::LifecycleRule {
            id: self.id,
            is_enabled: self.status == LifecycleStatus::Enabled,
            filter: self.filter.to_core(),
            expiration_days: self.expiration.map(LifecycleExpiration::days),
            abort_incomplete_multipart_days: self
                .abort_incomplete_multipart_upload
                .map(AbortIncompleteMultipartUpload::days_after_initiation),
        }
    }
}

/// Builder for a validated rule. Whole-set validation occurs on PUT input.
#[derive(Clone, Debug)]
pub struct LifecycleRuleBuilder {
    rule: LifecycleRule,
}

impl LifecycleRuleBuilder {
    /// Set an optional ID; missing/empty IDs are generated at publication.
    #[must_use]
    pub fn id(mut self, id: impl Into<String>) -> Self {
        self.rule.id = Some(id.into());
        self
    }

    /// Set the object expiration action.
    #[must_use]
    pub fn expiration(mut self, action: LifecycleExpiration) -> Self {
        self.rule.expiration = Some(action);
        self
    }

    /// Set the incomplete-upload abort action.
    #[must_use]
    pub fn abort_incomplete_multipart_upload(
        mut self,
        action: AbortIncompleteMultipartUpload,
    ) -> Self {
        self.rule.abort_incomplete_multipart_upload = Some(action);
        self
    }

    /// Validate the complete rule without accessing storage.
    ///
    /// # Errors
    /// Rejects missing actions, invalid XML text and exceeded bounds.
    pub fn build(self) -> Result<LifecycleRule, StorageError> {
        core::LifecycleConfiguration::new(vec![self.rule.clone().into_core()])
            .map_err(super::invalid)?;

        Ok(self.rule)
    }
}
