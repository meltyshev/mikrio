//! Detached operation failures and authoritative mutation outcomes.

use std::fmt;

use crate::{ConnectionError, EpochMillis, MetadataError, OpenFailure};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum OperationFailure {
    InvalidArgument,
    InvalidRequest,
    InvalidRange,
    InvalidPartNumber,
    NoSuchPrincipal,
    PrincipalAlreadyExists,
    PrincipalHasAccessKeys,
    NoSuchPrincipalPolicy,
    NoSuchBucket,
    NoSuchBucketPolicy,
    NoSuchCorsConfiguration,
    NoSuchPublicAccessBlockConfiguration,
    NoSuchKey,
    NoSuchUpload,
    OperationAborted,
    InvalidPart,
    InvalidPartOrder,
    EntityTooSmall,
    PreconditionFailed,
    NotModified,
    BodyReadFailed,
    BadDigest,
    IncompleteBody,
    EntityTooLarge,
    BucketAlreadyOwnedByYou,
    BucketNotEmpty,
    AccessDenied,
    StoreClosing,
    StoreClosed,
    StoreBusy,
    OperationCanceled,
    StoreCorrupt,
    IoError,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MutationOutcome {
    NotCommitted,
    Committed,
    Unknown,
}

/// Cache fields retained from the authorized conditional-read snapshot.
#[derive(Debug)]
pub struct CacheValidators {
    pub etag: String,
    pub cache_control: Option<String>,
    pub expires: Option<EpochMillis>,
}

#[derive(Debug)]
pub struct OperationError {
    failure: OperationFailure,
    outcome: Option<MutationOutcome>,
    cause: Option<Box<dyn std::error::Error + Send + Sync>>,
    cache_validators: Option<Box<CacheValidators>>,
    part_counts: Option<(u32, u32)>,
}

#[derive(Debug)]
struct SettlementError {
    operation: Box<OperationError>,
    rollback: rusqlite::Error,
}

impl fmt::Display for SettlementError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(formatter, "operation settlement failed: {}", self.rollback)
    }
}

impl std::error::Error for SettlementError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        Some(self.operation.as_ref())
    }
}

impl OperationError {
    pub(crate) const fn new(failure: OperationFailure) -> Self {
        Self {
            failure,
            outcome: None,
            cause: None,
            cache_validators: None,
            part_counts: None,
        }
    }

    pub(crate) fn caused(
        failure: OperationFailure,
        cause: impl std::error::Error + Send + Sync + 'static,
    ) -> Self {
        Self {
            failure,
            outcome: None,
            cause: Some(Box::new(cause)),
            cache_validators: None,
            part_counts: None,
        }
    }

    pub(crate) fn invalid_part_number(actual: u32, requested: u32) -> Self {
        let mut error = Self::new(OperationFailure::InvalidPartNumber);
        error.part_counts = Some((actual, requested));
        error
    }

    /// Actual and requested part counts, only from an authorized read snapshot.
    #[must_use]
    pub const fn part_counts(&self) -> Option<(u32, u32)> {
        self.part_counts
    }

    pub(crate) fn not_modified(selected: &crate::object::StoredObject) -> Self {
        let representation = selected.representation.fields();
        let mut error = Self::new(OperationFailure::NotModified);

        error.cache_validators = Some(Box::new(CacheValidators {
            etag: selected.etag.clone(),
            cache_control: representation.cache_control.clone(),
            expires: representation.expires,
        }));

        error
    }

    /// Only a successfully authorized cache-validation result carries these fields.
    #[must_use]
    pub fn cache_validators(&self) -> Option<&CacheValidators> {
        self.cache_validators.as_deref()
    }

    #[must_use]
    pub const fn failure(&self) -> OperationFailure {
        self.failure
    }

    #[must_use]
    pub const fn outcome(&self) -> Option<MutationOutcome> {
        self.outcome
    }

    pub(crate) fn mutation(mut self, outcome: MutationOutcome) -> Self {
        self.outcome = Some(outcome);
        self
    }

    pub(crate) fn uncommitted(self) -> Self {
        self.mutation(MutationOutcome::NotCommitted)
    }

    pub(crate) fn settlement(self, rollback: rusqlite::Error) -> Self {
        Self::caused(
            OperationFailure::IoError,
            SettlementError {
                operation: Box::new(self),
                rollback,
            },
        )
    }
}

impl OperationFailure {
    /// Safe descriptive text, independent of trusted local error causes.
    #[must_use]
    pub const fn message(self) -> &'static str {
        match self {
            OperationFailure::InvalidPartNumber => "requested object part does not exist",
            OperationFailure::InvalidRange => "requested range is not satisfiable",
            OperationFailure::InvalidArgument => "invalid operation argument",
            OperationFailure::InvalidRequest => "request conflicts with the selected operation",
            OperationFailure::NoSuchPrincipal => "principal does not exist",
            OperationFailure::PrincipalAlreadyExists => "principal name already exists",
            OperationFailure::PrincipalHasAccessKeys => "principal still has access keys",
            OperationFailure::NoSuchPrincipalPolicy => "principal has no policy",
            OperationFailure::NoSuchBucket => "bucket does not exist",
            OperationFailure::NoSuchBucketPolicy => "bucket has no policy",
            OperationFailure::NoSuchCorsConfiguration => "bucket has no CORS configuration",
            OperationFailure::NoSuchPublicAccessBlockConfiguration => {
                "bucket has no public access block configuration"
            }
            OperationFailure::NoSuchUpload => "multipart upload does not exist",
            OperationFailure::OperationAborted => "a conflicting operation is in progress",
            OperationFailure::InvalidPart => "selected multipart part does not match",
            OperationFailure::InvalidPartOrder => "invalid multipart part order",
            OperationFailure::EntityTooSmall => "nonfinal multipart part is too small",
            OperationFailure::NoSuchKey => "object does not exist",
            OperationFailure::PreconditionFailed => "object precondition failed",
            OperationFailure::NotModified => "object has not been modified",
            OperationFailure::BodyReadFailed => "upload source failed",
            OperationFailure::BadDigest => "payload digest mismatch",
            OperationFailure::IncompleteBody => "payload length mismatch",
            OperationFailure::EntityTooLarge => "payload exceeds the operation size limit",
            OperationFailure::BucketAlreadyOwnedByYou => "bucket already exists in this region",
            OperationFailure::BucketNotEmpty => "bucket contains objects",
            OperationFailure::AccessDenied => "operation is not permitted",
            OperationFailure::StoreClosing => "store connection is closing",
            OperationFailure::StoreClosed => "store connection is closed",
            OperationFailure::StoreBusy => "operation admission is busy",
            OperationFailure::OperationCanceled => "operation was canceled before commit",
            OperationFailure::StoreCorrupt => "stored operation state is inconsistent",
            OperationFailure::IoError => "native operation failed",
        }
    }
}

impl fmt::Display for OperationError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(self.failure.message())
    }
}

impl std::error::Error for OperationError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        self.cause
            .as_ref()
            .map(|cause| -> &(dyn std::error::Error + 'static) { cause.as_ref() })
    }
}

impl From<MetadataError> for OperationError {
    fn from(error: MetadataError) -> Self {
        let failure = match &error {
            MetadataError::Corrupt(_) => OperationFailure::StoreCorrupt,
            MetadataError::Sqlite(error)
                if matches!(
                    error.sqlite_error_code(),
                    Some(rusqlite::ErrorCode::DatabaseBusy | rusqlite::ErrorCode::DatabaseLocked)
                ) =>
            {
                OperationFailure::StoreBusy
            }
            MetadataError::Sqlite(error)
                if matches!(
                    error.sqlite_error_code(),
                    Some(rusqlite::ErrorCode::DatabaseCorrupt | rusqlite::ErrorCode::NotADatabase)
                ) =>
            {
                OperationFailure::StoreCorrupt
            }
            _ => OperationFailure::IoError,
        };

        Self::caused(failure, error)
    }
}

impl From<rusqlite::Error> for OperationError {
    fn from(error: rusqlite::Error) -> Self {
        MetadataError::from(error).into()
    }
}

impl From<std::io::Error> for OperationError {
    fn from(error: std::io::Error) -> Self {
        Self::caused(OperationFailure::IoError, error)
    }
}

impl From<crate::integrity::IntegrityError> for OperationError {
    fn from(error: crate::integrity::IntegrityError) -> Self {
        use crate::integrity::IntegrityError;

        let failure = match error {
            IntegrityError::TooLarge => OperationFailure::EntityTooLarge,
            IntegrityError::LengthMismatch => OperationFailure::IncompleteBody,
            _ => OperationFailure::BadDigest,
        };

        Self::caused(failure, error)
    }
}

impl From<crate::AccessError> for OperationError {
    fn from(error: crate::AccessError) -> Self {
        let failure = match &error {
            crate::AccessError::Denied => OperationFailure::AccessDenied,
            crate::AccessError::Corrupt(_) => OperationFailure::StoreCorrupt,
            _ => OperationFailure::IoError,
        };

        Self::caused(failure, error)
    }
}

impl From<ConnectionError> for OperationError {
    fn from(error: ConnectionError) -> Self {
        let failure = match &error {
            ConnectionError::StoreClosing => OperationFailure::StoreClosing,
            ConnectionError::StoreClosed => OperationFailure::StoreClosed,
            ConnectionError::Open(error) if error.failure() == OpenFailure::StoreBusy => {
                OperationFailure::StoreBusy
            }
            _ => OperationFailure::IoError,
        };

        Self::caused(failure, error)
    }
}
