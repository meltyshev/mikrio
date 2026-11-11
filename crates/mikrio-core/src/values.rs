//! Validated disk encodings and OS-backed store identity generation.
//! Persisted representations are described in `docs/storage.md`.

use std::fmt;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

const MIN_EPOCH_MS: i64 = -62_135_596_800_000;
const MAX_EPOCH_MS: i64 = 253_402_300_799_999;

/// A malformed persisted value; callers must classify this as corruption on read.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ValueError {
    /// An instant is outside the common year 0001-9999 range.
    TimestampRange,
    /// A valid shared instant cannot be represented by the host's clock type.
    PlatformTimestampRange,
    /// An identifier has the wrong width, prefix, or lowercase hexadecimal shape.
    IdentifierEncoding,
    /// The encoded upload time disagrees with its separately persisted time.
    UploadTimestampMismatch,
}

impl fmt::Display for ValueError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(match self {
            Self::TimestampRange => "timestamp is outside the supported range",
            Self::PlatformTimestampRange => "timestamp is outside the platform clock range",
            Self::IdentifierEncoding => "invalid persisted identifier encoding",
            Self::UploadTimestampMismatch => "upload identifier and initiation time disagree",
        })
    }
}

impl std::error::Error for ValueError {}

/// A store's random identity, independent of blob IDs and filesystem identity.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct StoreId([u8; 16]);

impl StoreId {
    /// Wrap exactly 16 bytes from a validated database BLOB or entropy source.
    #[must_use]
    pub const fn from_bytes(bytes: [u8; 16]) -> Self {
        Self(bytes)
    }

    /// Generate a fresh identity using only operating-system entropy.
    ///
    /// # Errors
    /// Returns entropy failure; there is no time/PID/counter fallback.
    pub fn generate() -> Result<Self, getrandom::Error> {
        Self::from_entropy(getrandom::fill)
    }

    /// Parse the marker's canonical 32-character lowercase hexadecimal identity.
    ///
    /// # Errors
    /// Rejects invalid length, uppercase or non-hexadecimal characters.
    pub fn parse(value: &str) -> Result<Self, ValueError> {
        Ok(Self(decode_hex(value.as_bytes())?))
    }

    /// Bytes bound to the database singleton's BLOB identity field.
    #[must_use]
    pub const fn as_bytes(&self) -> &[u8; 16] {
        &self.0
    }

    fn from_entropy(
        fill: impl FnOnce(&mut [u8]) -> Result<(), getrandom::Error>,
    ) -> Result<Self, getrandom::Error> {
        let mut bytes = [0; 16];
        fill(&mut bytes)?;
        Ok(Self(bytes))
    }
}

impl fmt::Display for StoreId {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        for byte in self.0 {
            write!(formatter, "{byte:02x}")?;
        }

        Ok(())
    }
}

/// A checked signed Unix instant at the disk format's millisecond precision.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub struct EpochMillis(i64);

impl EpochMillis {
    /// Validate the shared API/disk timestamp range.
    ///
    /// # Errors
    /// Returns [`ValueError::TimestampRange`] outside years 0001 through 9999.
    pub fn new(value: i64) -> Result<Self, ValueError> {
        if !(MIN_EPOCH_MS..=MAX_EPOCH_MS).contains(&value) {
            return Err(ValueError::TimestampRange);
        }

        Ok(Self(value))
    }

    /// Return the exact persisted integer.
    #[must_use]
    pub const fn get(self) -> i64 {
        self.0
    }

    /// Round down to HTTP whole seconds, including instants before the epoch.
    /// The shared lower bound is already aligned to a whole second.
    #[must_use]
    pub const fn floor_to_second(self) -> Self {
        Self(self.0.div_euclid(1000) * 1000)
    }

    /// Normalize a host instant down to the preceding whole millisecond.
    ///
    /// # Errors
    /// Rejects instants outside the shared year 0001-9999 range.
    pub fn from_system_time(value: SystemTime) -> Result<Self, ValueError> {
        let milliseconds = match value.duration_since(UNIX_EPOCH) {
            Ok(elapsed) => {
                i64::try_from(elapsed.as_millis()).map_err(|_| ValueError::TimestampRange)?
            }
            Err(error) => {
                let elapsed = error.duration();

                let rounded = elapsed.as_millis()
                    + u128::from(!elapsed.subsec_nanos().is_multiple_of(1_000_000));

                -i64::try_from(rounded).map_err(|_| ValueError::TimestampRange)?
            }
        };

        Self::new(milliseconds)
    }

    /// Convert without narrowing the shared range to the host's clock range.
    ///
    /// # Errors
    /// Reports [`ValueError::PlatformTimestampRange`] when the platform cannot
    /// represent this instant; Windows cannot represent dates before 1601.
    pub fn to_system_time(self) -> Result<SystemTime, ValueError> {
        let duration = Duration::from_millis(self.0.unsigned_abs());

        let value = if self.0 < 0 {
            UNIX_EPOCH.checked_sub(duration)
        } else {
            UNIX_EPOCH.checked_add(duration)
        };

        value.ok_or(ValueError::PlatformTimestampRange)
    }
}

/// A blob's 16 bytes, independent of object keys and store identity.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct BlobId([u8; 16]);

impl BlobId {
    /// Wrap bytes supplied by the entropy/uniqueness owner; this does not generate an ID.
    #[must_use]
    pub const fn from_bytes(bytes: [u8; 16]) -> Self {
        Self(bytes)
    }

    /// The persisted first-byte shard mapping.
    #[must_use]
    pub const fn shard(self) -> u8 {
        self.0[0]
    }

    /// Decode an exact lowercase 32-digit hexadecimal identifier.
    ///
    /// # Errors
    /// Returns [`ValueError::IdentifierEncoding`] for a noncanonical identifier.
    pub fn parse(value: &str) -> Result<Self, ValueError> {
        Ok(Self(decode_hex(value.as_bytes())?))
    }

    /// Bytes used by the metadata ledger, independent of the path encoding.
    #[must_use]
    pub const fn as_bytes(&self) -> &[u8; 16] {
        &self.0
    }
}

impl fmt::Display for BlobId {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        for byte in self.0 {
            write!(formatter, "{byte:02x}")?;
        }

        Ok(())
    }
}

/// A versioned upload ID; ordering matches signed initiation time then entropy.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct UploadId {
    initiated_at: EpochMillis,
    entropy: [u8; 16],
}

impl UploadId {
    /// Combine validated initiation time and bytes from the entropy owner.
    #[must_use]
    pub const fn new(initiated_at: EpochMillis, entropy: [u8; 16]) -> Self {
        Self {
            initiated_at,
            entropy,
        }
    }

    /// The immutable time encoded in this upload's sortable identity.
    #[must_use]
    pub const fn initiated_at(&self) -> EpochMillis {
        self.initiated_at
    }

    /// Decode and validate the relationship to the persisted initiation time.
    ///
    /// # Errors
    /// Rejects malformed IDs, out-of-range times, and timestamp disagreement.
    pub fn parse(value: &str, initiated_at: EpochMillis) -> Result<Self, ValueError> {
        let decoded = Self::decode(value)?;

        if decoded.initiated_at != initiated_at {
            return Err(ValueError::UploadTimestampMismatch);
        }

        Ok(decoded)
    }

    /// Decode an opaque request identity without a persisted timestamp assertion.
    ///
    /// # Errors
    /// Rejects malformed identifiers and out-of-range initiation times.
    pub fn decode(value: &str) -> Result<Self, ValueError> {
        let bytes = value.as_bytes();

        if bytes.len() != 51 || !bytes.starts_with(b"u1-") {
            return Err(ValueError::IdentifierEncoding);
        }

        let mut timestamp_bytes: [u8; 8] = decode_hex(&bytes[3..19])?;
        timestamp_bytes[0] ^= 0x80;
        let decoded_time = EpochMillis::new(i64::from_be_bytes(timestamp_bytes))?;
        Ok(Self::new(decoded_time, decode_hex(&bytes[19..])?))
    }
}

impl fmt::Display for UploadId {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        let mut timestamp_bytes = self.initiated_at.get().to_be_bytes();
        timestamp_bytes[0] ^= 0x80;
        formatter.write_str("u1-")?;

        for byte in timestamp_bytes.into_iter().chain(self.entropy) {
            write!(formatter, "{byte:02x}")?;
        }

        Ok(())
    }
}

pub(crate) fn decode_hex<const N: usize>(bytes: &[u8]) -> Result<[u8; N], ValueError> {
    if bytes.len() / 2 != N || !bytes.len().is_multiple_of(2) {
        return Err(ValueError::IdentifierEncoding);
    }

    let mut decoded = [0; N];

    for (destination, pair) in decoded.iter_mut().zip(bytes.as_chunks::<2>().0) {
        *destination = (hex_digit(pair[0])? << 4) | hex_digit(pair[1])?;
    }

    Ok(decoded)
}

fn hex_digit(byte: u8) -> Result<u8, ValueError> {
    match byte {
        b'0'..=b'9' => Ok(byte - b'0'),
        b'a'..=b'f' => Ok(byte - b'a' + 10),
        _ => Err(ValueError::IdentifierEncoding),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn store_identity_entropy_failure_never_produces_an_id() {
        let result = StoreId::from_entropy(|bytes| {
            bytes.fill(7);
            Err(getrandom::Error::UNSUPPORTED)
        });

        assert_eq!(result, Err(getrandom::Error::UNSUPPORTED));
    }

    #[test]
    fn timestamp_bounds_are_checked_without_truncation() {
        for valid in [MIN_EPOCH_MS, -1, 0, MAX_EPOCH_MS] {
            assert_eq!(EpochMillis::new(valid).unwrap().get(), valid);
        }

        for invalid in [i64::MIN, MIN_EPOCH_MS - 1, MAX_EPOCH_MS + 1, i64::MAX] {
            assert_eq!(
                EpochMillis::new(invalid),
                Err(ValueError::TimestampRange),
                "timestamp={invalid}"
            );
        }
    }

    #[test]
    fn system_time_rounding_preserves_pre_epoch_order() {
        // Windows SystemTime has 100 ns ticks; every input is representable.
        for (nanoseconds, expected) in [
            (-1_000_100, -2),
            (-1_000_000, -1),
            (-100, -1),
            (0, 0),
            (100, 0),
            (999_900, 0),
            (1_000_000, 1),
        ] {
            let nanoseconds: i64 = nanoseconds;
            let duration = Duration::from_nanos(nanoseconds.unsigned_abs());

            let instant = if nanoseconds < 0 {
                UNIX_EPOCH.checked_sub(duration)
            } else {
                UNIX_EPOCH.checked_add(duration)
            }
            .unwrap();

            let normalized = EpochMillis::from_system_time(instant).unwrap();
            assert_eq!(normalized.get(), expected, "nanoseconds={nanoseconds}");

            assert!(
                normalized.to_system_time().unwrap() <= instant,
                "nanoseconds={nanoseconds}"
            );
        }
    }

    #[test]
    fn clock_conversion_rejects_values_outside_the_shared_range() {
        let after_maximum = UNIX_EPOCH
            .checked_add(Duration::from_hours(70_389_528))
            .unwrap();

        assert_eq!(
            EpochMillis::from_system_time(after_maximum),
            Err(ValueError::TimestampRange)
        );

        let earliest = EpochMillis::new(MIN_EPOCH_MS).unwrap();

        #[cfg(windows)]
        assert_eq!(
            earliest.to_system_time(),
            Err(ValueError::PlatformTimestampRange)
        );

        #[cfg(unix)]
        assert_eq!(
            EpochMillis::from_system_time(earliest.to_system_time().unwrap()),
            Ok(earliest)
        );
    }

    #[test]
    fn blob_encoding_preserves_shard_and_rejects_noncanonical_names() {
        let blob = BlobId::from_bytes([0xab; 16]);
        assert_eq!(blob.shard(), 0xab);
        assert_eq!(blob.to_string(), "abababababababababababababababab");
        assert_eq!(BlobId::parse(&blob.to_string()).unwrap(), blob);

        for invalid in [
            "",
            "../blob",
            "abab",
            "ABABABABABABABABABABABABABABABAB",
            "é000000000000000000000000000000",
        ] {
            assert_eq!(
                BlobId::parse(invalid),
                Err(ValueError::IdentifierEncoding),
                "blob_id={invalid:?}"
            );
        }
    }

    #[test]
    fn upload_id_matches_independent_signed_order_vectors() {
        let cases = [
            (-1, "u1-7fffffffffffffff000102030405060708090a0b0c0d0e0f"),
            (0, "u1-8000000000000000000102030405060708090a0b0c0d0e0f"),
            (1, "u1-8000000000000001000102030405060708090a0b0c0d0e0f"),
        ];

        let entropy = [0, 1, 2, 3, 4, 5, 6, 7, 8, 9, 10, 11, 12, 13, 14, 15];

        for (time, literal) in cases {
            let time = EpochMillis::new(time).unwrap();
            let upload = UploadId::new(time, entropy);
            assert_eq!(upload.to_string(), literal);
            assert_eq!(UploadId::parse(literal, time).unwrap(), upload);
        }

        assert!(cases[0].1 < cases[1].1 && cases[1].1 < cases[2].1);
    }

    #[test]
    fn upload_id_rejects_invalid_shape_time_and_disagreement() {
        let time = EpochMillis::new(0).unwrap();

        for invalid in [
            "",
            "u2-8000000000000000000102030405060708090a0b0c0d0e0f",
            "u1-8000000000000000000102030405060708090A0b0c0d0e0f",
        ] {
            assert_eq!(
                UploadId::parse(invalid, time),
                Err(ValueError::IdentifierEncoding)
            );
        }

        assert_eq!(
            UploadId::parse("u1-8000000000000001000102030405060708090a0b0c0d0e0f", time),
            Err(ValueError::UploadTimestampMismatch)
        );

        assert_eq!(
            UploadId::parse("u1-ffffffffffffffff000102030405060708090a0b0c0d0e0f", time),
            Err(ValueError::TimestampRange)
        );
    }
}
