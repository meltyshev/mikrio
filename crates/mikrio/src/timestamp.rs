use std::time::SystemTime;

use mikrio_core::EpochMillis;

use crate::StorageError;

/// An owned UTC instant with millisecond precision, from year 0001 through 9999.
/// Its range is independent of the host platform's [`SystemTime`] representation.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub struct Timestamp(EpochMillis);

impl Timestamp {
    /// Construct an instant from signed Unix epoch milliseconds.
    ///
    /// # Errors
    /// Returns `InvalidArgument` outside years 0001 through 9999.
    pub fn from_unix_millis(value: i64) -> Result<Self, StorageError> {
        EpochMillis::new(value)
            .map(Self)
            .map_err(StorageError::from_value)
    }

    /// Exact signed milliseconds since the Unix epoch.
    #[must_use]
    pub const fn as_unix_millis(self) -> i64 {
        self.0.get()
    }

    pub(crate) const fn into_core(self) -> EpochMillis {
        self.0
    }

    pub(crate) const fn from_core(value: EpochMillis) -> Self {
        Self(value)
    }
}

impl TryFrom<SystemTime> for Timestamp {
    type Error = StorageError;

    /// Round down to whole milliseconds, including for pre-epoch instants.
    ///
    /// # Errors
    /// Returns `InvalidArgument` outside years 0001 through 9999.
    fn try_from(value: SystemTime) -> Result<Self, Self::Error> {
        EpochMillis::from_system_time(value)
            .map(Self)
            .map_err(StorageError::from_value)
    }
}

impl TryFrom<Timestamp> for SystemTime {
    type Error = StorageError;

    /// Convert without clamping or changing the instant.
    ///
    /// # Errors
    /// Returns `InvalidArgument` if this platform cannot represent the instant.
    fn try_from(value: Timestamp) -> Result<Self, Self::Error> {
        value.0.to_system_time().map_err(StorageError::from_value)
    }
}
