//! Checked facade rule values with explicit conversion to the internal contract.

use crate::StorageError;

/// One ordered bucket CORS rule. Construct with [`Self::builder`].
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CorsRule {
    inner: mikrio_core::cors::CorsRule,
}

impl CorsRule {
    /// Start a rule with its nonempty origin and uppercase method lists.
    #[must_use]
    pub fn builder(allowed_origins: Vec<String>, allowed_methods: Vec<String>) -> CorsRuleBuilder {
        CorsRuleBuilder {
            inner: mikrio_core::cors::CorsRule {
                id: None,
                allowed_origins,
                allowed_methods,
                allowed_headers: None,
                expose_headers: None,
                max_age_seconds: None,
            },
        }
    }

    /// Optional rule identifier, limited to 255 Unicode scalar values.
    #[must_use]
    pub fn id(&self) -> Option<&str> {
        self.inner.id.as_deref()
    }

    /// Exact origin patterns, each containing at most one wildcard.
    #[must_use]
    pub fn allowed_origins(&self) -> &[String] {
        &self.inner.allowed_origins
    }

    /// Uppercase GET, PUT, HEAD, POST or DELETE methods in supplied order.
    #[must_use]
    pub fn allowed_methods(&self) -> &[String] {
        &self.inner.allowed_methods
    }

    /// Optional preflight header patterns; an empty list permits no requested headers.
    #[must_use]
    pub fn allowed_headers(&self) -> Option<&[String]> {
        self.inner.allowed_headers.as_deref()
    }

    /// Optional response header names to expose, without wildcards.
    #[must_use]
    pub fn expose_headers(&self) -> Option<&[String]> {
        self.inner.expose_headers.as_deref()
    }

    /// Optional preflight cache lifetime in seconds, including zero and negative values.
    #[must_use]
    pub fn max_age_seconds(&self) -> Option<i32> {
        self.inner.max_age_seconds
    }

    pub(super) fn from_core(inner: mikrio_core::cors::CorsRule) -> Self {
        Self { inner }
    }

    pub(super) fn into_core(self) -> mikrio_core::cors::CorsRule {
        self.inner
    }
}

/// Builder for one [`CorsRule`]; configuration-wide limits are checked on PUT input.
#[derive(Clone, Debug)]
pub struct CorsRuleBuilder {
    inner: mikrio_core::cors::CorsRule,
}

impl CorsRuleBuilder {
    /// Set an identifier that must be unique within its configuration.
    #[must_use]
    pub fn id(mut self, id: impl Into<String>) -> Self {
        self.inner.id = Some(id.into());
        self
    }

    /// Set allowed request header patterns, retaining an explicitly empty list.
    #[must_use]
    pub fn allowed_headers(mut self, headers: Vec<String>) -> Self {
        self.inner.allowed_headers = Some(headers);
        self
    }

    /// Set response headers exposed to browser code, retaining an empty list.
    #[must_use]
    pub fn expose_headers(mut self, headers: Vec<String>) -> Self {
        self.inner.expose_headers = Some(headers);
        self
    }

    /// Set the preflight cache lifetime, as a signed 32-bit number of seconds.
    #[must_use]
    pub fn max_age_seconds(mut self, seconds: i32) -> Self {
        self.inner.max_age_seconds = Some(seconds);
        self
    }

    /// Validate and own a rule without accessing storage.
    ///
    /// # Errors
    /// Returns `InvalidArgument` for invalid fields or exceeded limits.
    pub fn build(self) -> Result<CorsRule, StorageError> {
        let validated =
            mikrio_core::cors::CorsConfiguration::new(vec![self.inner]).map_err(super::invalid)?;

        let inner = validated
            .into_rules()
            .pop()
            .ok_or_else(|| super::invalid(mikrio_core::cors::CorsError))?;

        Ok(CorsRule { inner })
    }
}
