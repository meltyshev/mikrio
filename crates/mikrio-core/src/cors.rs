//! Bounded whole-value CORS configurations. Transport matching remains in HTTP.

use std::collections::BTreeSet;
use std::fmt;

use serde::{Deserialize, Serialize};

const MAX_RULES: usize = 100;
const MAX_DOCUMENT_BYTES: usize = 64 * 1024;
pub(crate) const MAX_STORED_BYTES: usize = 256 * 1024;

/// Owned rule fields, in their original order and case.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CorsRule {
    pub id: Option<String>,
    pub allowed_origins: Vec<String>,
    pub allowed_methods: Vec<String>,
    pub allowed_headers: Option<Vec<String>>,
    pub expose_headers: Option<Vec<String>>,
    pub max_age_seconds: Option<i32>,
}

/// A completely validated, ordered configuration; absence is represented separately.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CorsConfiguration(Vec<CorsRule>);

/// Invalid CORS fields or an exceeded configuration bound. No input is retained.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct CorsError;

impl fmt::Display for CorsError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("invalid or oversized CORS configuration")
    }
}

impl std::error::Error for CorsError {}

impl CorsConfiguration {
    /// Validate every rule and the canonical XML byte budget before admission.
    ///
    /// # Errors
    /// Rejects invalid fields, duplicate IDs, or a configuration outside its bounds.
    pub fn new(rules: Vec<CorsRule>) -> Result<Self, CorsError> {
        if rules.is_empty() || rules.len() > MAX_RULES {
            return Err(CorsError);
        }

        let mut ids = BTreeSet::new();
        let mut size = b"<CORSConfiguration xmlns=\"http://s3.amazonaws.com/doc/2006-03-01/\"></CORSConfiguration>".len();

        for rule in &rules {
            size += b"<CORSRule></CORSRule>".len();

            if let Some(id) = &rule.id {
                if id.chars().take(256).count() > 255 || !ids.insert(id) {
                    return Err(CorsError);
                }

                element_size(&mut size, "ID", id)?;
            }

            if rule.allowed_origins.is_empty() || rule.allowed_methods.is_empty() {
                return Err(CorsError);
            }

            for origin in &rule.allowed_origins {
                if !origin.bytes().all(|byte| byte.is_ascii_graphic())
                    || origin.is_empty()
                    || origin.bytes().filter(|&byte| byte == b'*').count() > 1
                {
                    return Err(CorsError);
                }

                element_size(&mut size, "AllowedOrigin", origin)?;
            }

            for method in &rule.allowed_methods {
                if !matches!(method.as_str(), "GET" | "PUT" | "HEAD" | "POST" | "DELETE") {
                    return Err(CorsError);
                }

                element_size(&mut size, "AllowedMethod", method)?;
            }

            for header in rule.allowed_headers.iter().flatten() {
                if !header_name(header) || header.bytes().filter(|&byte| byte == b'*').count() > 1 {
                    return Err(CorsError);
                }

                element_size(&mut size, "AllowedHeader", header)?;
            }

            for header in rule.expose_headers.iter().flatten() {
                if !header_name(header) || header.contains('*') {
                    return Err(CorsError);
                }

                element_size(&mut size, "ExposeHeader", header)?;
            }

            if let Some(seconds) = rule.max_age_seconds {
                element_size(&mut size, "MaxAgeSeconds", &seconds.to_string())?;
            }
        }

        Ok(Self(rules))
    }

    #[must_use]
    pub fn rules(&self) -> &[CorsRule] {
        &self.0
    }

    #[must_use]
    pub fn into_rules(self) -> Vec<CorsRule> {
        self.0
    }

    pub(crate) fn encode(&self) -> Result<String, CorsError> {
        let encoded = serde_json::to_string(&self.0).map_err(|_| CorsError)?;

        if encoded.len() > MAX_STORED_BYTES {
            return Err(CorsError);
        }

        Ok(encoded)
    }

    pub(crate) fn decode(encoded: &str) -> Result<Self, CorsError> {
        if encoded.len() > MAX_STORED_BYTES {
            return Err(CorsError);
        }

        Self::new(serde_json::from_str(encoded).map_err(|_| CorsError)?)
    }
}

/// HTTP token grammar, shared by configuration and requested-header validation.
#[must_use]
pub fn header_name(value: &str) -> bool {
    !value.is_empty()
        && value.bytes().all(|byte| {
            byte.is_ascii_alphanumeric()
                || matches!(
                    byte,
                    b'!' | b'#'
                        | b'$'
                        | b'%'
                        | b'&'
                        | b'\''
                        | b'*'
                        | b'+'
                        | b'-'
                        | b'.'
                        | b'^'
                        | b'_'
                        | b'`'
                        | b'|'
                        | b'~'
                )
        })
}

fn element_size(size: &mut usize, name: &str, value: &str) -> Result<(), CorsError> {
    if value.len() > MAX_DOCUMENT_BYTES {
        return Err(CorsError);
    }

    // XML 1.0 text cannot represent other control characters. Count escaping
    // without allocating another document, including CR's preserving entity.
    let mut bytes = 2 * name.len() + 5;

    for character in value.chars() {
        bytes += match character {
            '\'' | '"' => 6,
            '&' | '\r' => 5,
            '<' | '>' => 4,
            '\t' | '\n' => 1,
            '\u{20}'..='\u{d7ff}' | '\u{e000}'..='\u{fffd}' | '\u{10000}'..='\u{10ffff}' => {
                character.len_utf8()
            }
            _ => return Err(CorsError),
        };
    }

    *size = size.checked_add(bytes).ok_or(CorsError)?;

    if *size > MAX_DOCUMENT_BYTES {
        return Err(CorsError);
    }

    Ok(())
}

#[cfg(test)]
mod tests;
