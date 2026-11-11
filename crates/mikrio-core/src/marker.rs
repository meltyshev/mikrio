//! Strict, bounded bootstrap encoding; operational configuration lives in SQL.

use std::fmt;
use std::io::{self, Read};

use serde::{Deserialize, Serialize};

use crate::values::{StoreId, ValueError};

const MARKER_LIMIT: u64 = 4096;

/// Positively identified bootstrap state; never inferred from filenames alone.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum MarkerState {
    /// Identity is established; required artifacts still need validation.
    Initializing,
    /// The database and filesystem layout have been validated.
    Ready,
}

/// Validated store identity and bootstrap state, separate from format version.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct IdentityMarker {
    store_id: StoreId,
    state: MarkerState,
}

#[derive(Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct EncodedMarker {
    format: String,
    marker_version: u32,
    store_id: String,
    state: MarkerState,
}

#[derive(Deserialize)]
struct MarkerVersion {
    format: String,
    marker_version: u32,
}

/// Invalid or unreadable identity marker. No malformed input becomes a default.
#[derive(Debug)]
pub enum MarkerError {
    /// Reading the marker failed.
    Io(io::Error),
    /// Invalid fields, duplicate/unknown fields, malformed JSON or excessive size.
    Invalid,
    /// An explicitly unsupported marker encoding version.
    UnsupportedVersion,
}

impl fmt::Display for MarkerError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(match self {
            Self::Io(_) => "identity marker could not be read",
            Self::Invalid => "invalid identity marker",
            Self::UnsupportedVersion => "unsupported marker encoding version",
        })
    }
}

impl std::error::Error for MarkerError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::Io(error) => Some(error),
            Self::Invalid | Self::UnsupportedVersion => None,
        }
    }
}

impl From<ValueError> for MarkerError {
    fn from(_: ValueError) -> Self {
        Self::Invalid
    }
}

impl IdentityMarker {
    /// Construct the initial marker from an already generated store identity.
    #[must_use]
    pub const fn initializing(store_id: StoreId) -> Self {
        Self {
            store_id,
            state: MarkerState::Initializing,
        }
    }

    /// Read at most 4 KiB plus one rejection byte and validate all fields.
    ///
    /// # Errors
    /// Rejects oversized/malformed/duplicate/unknown fields, wrong magic,
    /// unsupported encoding versions and noncanonical identifiers.
    pub fn read(reader: impl Read) -> Result<Self, MarkerError> {
        let mut bytes = Vec::new();

        reader
            .take(MARKER_LIMIT + 1)
            .read_to_end(&mut bytes)
            .map_err(MarkerError::Io)?;

        if u64::try_from(bytes.len()).map_err(|_| MarkerError::Invalid)? > MARKER_LIMIT {
            return Err(MarkerError::Invalid);
        }

        let version: MarkerVersion =
            serde_json::from_slice(&bytes).map_err(|_| MarkerError::Invalid)?;

        if version.format != "mikrio" {
            return Err(MarkerError::Invalid);
        }

        if version.marker_version != 1 {
            return Err(MarkerError::UnsupportedVersion);
        }

        let encoded: EncodedMarker =
            serde_json::from_slice(&bytes).map_err(|_| MarkerError::Invalid)?;

        Ok(Self {
            store_id: StoreId::parse(&encoded.store_id)?,
            state: encoded.state,
        })
    }

    /// Encode the exact private version-1 marker without operational fields.
    ///
    /// # Errors
    /// Reports a serialization failure rather than returning a partial marker.
    pub fn encode(&self) -> Result<Vec<u8>, MarkerError> {
        serde_json::to_vec(&EncodedMarker {
            format: "mikrio".into(),
            marker_version: 1,
            store_id: self.store_id.to_string(),
            state: self.state,
        })
        .map_err(|_| MarkerError::Invalid)
    }

    /// Identity that must match the committed singleton database record.
    #[must_use]
    pub const fn store_id(&self) -> StoreId {
        self.store_id
    }

    /// Bootstrap state; Ready does not replace database/layout validation.
    #[must_use]
    pub const fn state(&self) -> MarkerState {
        self.state
    }

    /// Prepare the ready marker after its caller has validated committed state.
    #[must_use]
    pub const fn ready(&self) -> Self {
        Self {
            store_id: self.store_id,
            state: MarkerState::Ready,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const MARKER: &[u8] = br#"{"format":"mikrio","marker_version":1,"store_id":"00112233445566778899aabbccddeeff","state":"initializing"}"#;

    #[test]
    fn marker_matches_independent_bytes_and_preserves_identity_on_ready() {
        let marker = IdentityMarker::read(MARKER).unwrap();

        assert_eq!(
            marker.store_id().to_string(),
            "00112233445566778899aabbccddeeff"
        );

        assert_eq!(marker.state(), MarkerState::Initializing);
        assert_eq!(marker.encode().unwrap(), MARKER);
        let ready = marker.ready();
        assert_eq!(ready.store_id(), marker.store_id());
        assert_eq!(ready.state(), MarkerState::Ready);
    }

    #[test]
    fn marker_rejects_duplicates_unknown_fields_trailing_data_and_invalid_ids() {
        let text = std::str::from_utf8(MARKER).unwrap();

        for invalid in [
            text.replace("\"format\":", "\"format\":\"mikrio\",\"format\":"),
            text.replace("\"format\":", "\"unknown\":1,\"format\":"),
            text.replace("001122", "A01122"),
            text.replace("initializing", "Initializing"),
            text.replace("mikrio", "other"),
            format!("{text}{{}}"),
        ] {
            assert!(
                matches!(
                    IdentityMarker::read(invalid.as_bytes()),
                    Err(MarkerError::Invalid)
                ),
                "{invalid}"
            );
        }

        assert!(matches!(
            IdentityMarker::read(
                text.replace("\"marker_version\":1", "\"marker_version\":2")
                    .as_bytes()
            ),
            Err(MarkerError::UnsupportedVersion)
        ));
    }

    #[test]
    fn marker_size_bound_accepts_limit_and_stops_oversized_reads() {
        let mut bytes = MARKER.to_vec();
        bytes.resize(4096, b' ');
        assert!(IdentityMarker::read(bytes.as_slice()).is_ok());
        bytes.push(b' ');

        assert!(matches!(
            IdentityMarker::read(bytes.as_slice()),
            Err(MarkerError::Invalid)
        ));

        let mut infinite = io::repeat(b' ');

        assert!(matches!(
            IdentityMarker::read(&mut infinite),
            Err(MarkerError::Invalid)
        ));
    }

    #[test]
    fn future_marker_fields_are_not_misclassified_as_current_format_corruption() {
        let future =
            br#"{"format":"mikrio","marker_version":2,"new_field":true,"state":"new-state"}"#;

        assert!(matches!(
            IdentityMarker::read(future.as_slice()),
            Err(MarkerError::UnsupportedVersion)
        ));
    }
}
