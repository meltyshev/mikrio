//! Embedded bucket and object storage with bounded streams and explicit close.
//!
//! Use the caller's Tokio runtime with its time driver enabled. Explicitly close
//! stores before destroying that runtime. Opening alone never starts a listener.
//!
//! ```no_run
//! use mikrio::{OpenOptions, StorageError, Store};
//!
//! # async fn example() -> Result<(), StorageError> {
//! let store = Store::open(OpenOptions::new("./storage").region("eu-west-1")).await?;
//! assert_eq!(store.info().region(), "eu-west-1");
//! store.close().await?;
//! # Ok(())
//! # }
//! ```

#![warn(missing_docs)]

use std::fmt;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use mikrio_core::{Connection, ConnectionError, OpenFailure};

mod access_configuration;
mod admin;
mod body_source;
mod bucket;
mod checksum;
mod copy_object;
mod cors;
mod delete_objects;
#[doc(hidden)]
pub mod embedding;
mod lifecycle;
#[cfg(feature = "http")]
mod listener;
mod listing;
mod multipart;
mod object;
mod object_attributes;
mod object_body;
mod object_tagging;
mod put_object;
mod read_object;
mod tag;
mod timestamp;

pub use access_configuration::{
    AclGrant, AclOwner, AclPermission, DeleteBucketPolicyInput, DeleteBucketPolicyInputBuilder,
    DeleteBucketPolicyOutput, DeletePublicAccessBlockInput, DeletePublicAccessBlockInputBuilder,
    DeletePublicAccessBlockOutput, GetBucketAclInput, GetBucketAclInputBuilder, GetBucketAclOutput,
    GetBucketOwnershipControlsInput, GetBucketOwnershipControlsInputBuilder,
    GetBucketOwnershipControlsOutput, GetBucketPolicyInput, GetBucketPolicyInputBuilder,
    GetBucketPolicyOutput, GetBucketPolicyStatusInput, GetBucketPolicyStatusInputBuilder,
    GetBucketPolicyStatusOutput, GetObjectAclInput, GetObjectAclInputBuilder, GetObjectAclOutput,
    GetPublicAccessBlockInput, GetPublicAccessBlockInputBuilder, GetPublicAccessBlockOutput,
    ObjectOwnership, PublicAccessBlockConfiguration, PutBucketOwnershipControlsInput,
    PutBucketOwnershipControlsInputBuilder, PutBucketOwnershipControlsOutput, PutBucketPolicyInput,
    PutBucketPolicyInputBuilder, PutBucketPolicyOutput, PutPublicAccessBlockInput,
    PutPublicAccessBlockInputBuilder, PutPublicAccessBlockOutput,
};
pub use admin::{
    AccessKeyInfo, CreateAccessKeyInput, CreateAccessKeyInputBuilder, CreateAccessKeyOutput,
    CreatePrincipalInput, CreatePrincipalInputBuilder, DeleteAccessKeyInput,
    DeleteAccessKeyInputBuilder, DeleteAccessKeyOutput, DeletePrincipalInput,
    DeletePrincipalInputBuilder, DeletePrincipalOutput, DeletePrincipalPolicyInput,
    DeletePrincipalPolicyInputBuilder, DeletePrincipalPolicyOutput, GetPrincipalInput,
    GetPrincipalInputBuilder, GetPrincipalPolicyInput, GetPrincipalPolicyInputBuilder,
    GetPrincipalPolicyOutput, ListAccessKeysInput, ListAccessKeysInputBuilder,
    ListAccessKeysOutput, ListPrincipalsInput, ListPrincipalsInputBuilder, ListPrincipalsOutput,
    OwnerInfo, PrincipalInfo, PutPrincipalPolicyInput, PutPrincipalPolicyInputBuilder,
    PutPrincipalPolicyOutput, SetPrincipalEnabledInput, SetPrincipalEnabledInputBuilder,
};
pub use body_source::BodySource;
pub use bucket::{
    CreateBucketInput, CreateBucketInputBuilder, CreateBucketOutput, DeleteBucketInput,
    DeleteBucketInputBuilder, DeleteBucketOutput, GetBucketLocationInput,
    GetBucketLocationInputBuilder, GetBucketLocationOutput, HeadBucketInput,
    HeadBucketInputBuilder, HeadBucketOutput,
};
pub use checksum::ChecksumAlgorithm;
pub use copy_object::{CopyDirective, CopyObjectInput, CopyObjectInputBuilder, CopyObjectOutput};
pub use cors::{
    CorsRule, CorsRuleBuilder, DeleteBucketCorsInput, DeleteBucketCorsInputBuilder,
    DeleteBucketCorsOutput, GetBucketCorsInput, GetBucketCorsInputBuilder, GetBucketCorsOutput,
    PutBucketCorsInput, PutBucketCorsInputBuilder, PutBucketCorsOutput,
};
pub use delete_objects::{
    DeleteError, DeleteObjectError, DeleteObjectProgress, DeleteObjectsInput,
    DeleteObjectsInputBuilder, DeleteObjectsOutput, DeleteObjectsProgress, DeletedObject,
    ObjectIdentifier, ObjectIdentifierBuilder,
};
pub use lifecycle::{
    AbortIncompleteMultipartUpload, DeleteBucketLifecycleInput, DeleteBucketLifecycleInputBuilder,
    DeleteBucketLifecycleOutput, GetBucketLifecycleConfigurationInput,
    GetBucketLifecycleConfigurationInputBuilder, GetBucketLifecycleConfigurationOutput,
    LifecycleExpiration, LifecycleFilter, LifecycleRule, LifecycleRuleBuilder, LifecycleStatus,
    PutBucketLifecycleConfigurationInput, PutBucketLifecycleConfigurationInputBuilder,
    PutBucketLifecycleConfigurationOutput,
};
#[cfg(feature = "http")]
pub use listener::{Credentials, ListenOptions, Server};
pub use listing::{
    BucketSummary, CommonPrefix, ListBucketsInput, ListBucketsInputBuilder, ListBucketsOutput,
    ListObjectsV2Input, ListObjectsV2InputBuilder, ListObjectsV2Output, ObjectSummary,
};
pub use multipart::*;
pub use object::{DeleteObjectInput, DeleteObjectInputBuilder, DeleteObjectOutput, ObjectRef};
pub use object_attributes::{
    Checksum, GetObjectAttributesInput, GetObjectAttributesInputBuilder, GetObjectAttributesOutput,
    ObjectAttribute, ObjectPart, ObjectParts,
};
pub use object_body::ObjectBody;
pub use object_tagging::{
    DeleteObjectTaggingInput, DeleteObjectTaggingInputBuilder, DeleteObjectTaggingOutput,
    GetObjectTaggingInput, GetObjectTaggingInputBuilder, GetObjectTaggingOutput,
    PutObjectTaggingInput, PutObjectTaggingInputBuilder, PutObjectTaggingOutput,
};
pub use put_object::{PutObjectInput, PutObjectInputBuilder, PutObjectOutput};
pub use read_object::{
    ChecksumMode, ChecksumType, GetObjectInput, GetObjectInputBuilder, GetObjectOutput,
    HeadObjectInput, HeadObjectInputBuilder, HeadObjectOutput,
};
pub use tag::Tag;
pub use timestamp::Timestamp;

/// Values for opening a managed directory. Constructing options performs no I/O.
#[derive(Debug, Clone)]
pub struct OpenOptions {
    directory: PathBuf,
    region: Option<String>,
    create_if_missing: bool,
}

impl OpenOptions {
    /// Select a managed store directory, relative to cwd at the start of open.
    #[must_use]
    pub fn new(directory: impl AsRef<Path>) -> Self {
        Self {
            directory: directory.as_ref().to_owned(),
            region: None,
            create_if_missing: true,
        }
    }

    /// Set the signing region. New stores otherwise use `us-east-1`; reopening
    /// without this option uses the persisted region. Conflicts fail at open.
    #[must_use]
    pub fn region(mut self, region: impl Into<String>) -> Self {
        self.region = Some(region.into());
        self
    }

    /// Permit initialization of missing or empty storage (the default).
    /// When false, require a recognized store identity. Interrupted known
    /// initialization can still recover; this does not make access read-only.
    #[must_use]
    pub const fn create_if_missing(mut self, value: bool) -> Self {
        self.create_if_missing = value;
        self
    }

    /// Validate portable path and region values without accessing storage.
    ///
    /// # Errors
    /// Returns `InvalidArgument` for an empty/non-UTF-8 path or malformed region.
    /// Existing state, recovery and region agreement are checked only at open.
    pub fn validate(&self) -> Result<(), StorageError> {
        mikrio_core::OpenedStore::validate(&self.directory, self.region.as_deref())
            .map_err(|error| StorageError::from_core(error.into()))
    }
}

/// Detached immutable store facts, available after its connection closes.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StoreInfo {
    store_id: String,
    format_version: u32,
    directory: PathBuf,
    region: String,
}

impl StoreInfo {
    /// Stable random identity encoded as 32 lowercase hexadecimal characters.
    #[must_use]
    pub fn store_id(&self) -> &str {
        &self.store_id
    }

    /// Operational database format version, independent of marker/package versions.
    #[must_use]
    pub const fn format_version(&self) -> u32 {
        self.format_version
    }

    /// Absolute resolved managed directory.
    #[must_use]
    pub fn directory(&self) -> &Path {
        &self.directory
    }

    /// Persisted signing region.
    #[must_use]
    pub fn region(&self) -> &str {
        &self.region
    }
}

/// One independently opened logical connection. Clones alias its close state.
/// Drop is fallback cleanup; use [`Store::close`] to await/report shutdown.
#[derive(Clone)]
pub struct Store {
    connection: Connection,
    info: Arc<StoreInfo>,
}

impl Store {
    /// Initialize/reopen a managed directory without binding any network port.
    /// A canceled caller receives no handle; native initialization can still
    /// finish and leave a valid store for a later open.
    ///
    /// # Errors
    /// Rejects invalid inputs, unrelated/inconsistent storage, unsupported
    /// formats, conflicting regions and exhausted bounded contention. Requires
    /// a current Tokio runtime. Native failures retain their local cause.
    pub async fn open(options: OpenOptions) -> Result<Self, StorageError> {
        let mode = if options.create_if_missing {
            mikrio_core::OpenMode::CreateIfMissing
        } else {
            mikrio_core::OpenMode::ExistingOnly
        };

        let connection = Connection::open_with_mode(options.directory, options.region, mode)
            .await
            .map_err(StorageError::from_core)?;

        let facts = connection.info();

        let info = Arc::new(StoreInfo {
            store_id: facts.record.store_id.to_string(),
            format_version: facts.record.format_version,
            directory: facts.directory.clone(),
            region: facts.record.region.clone(),
        });

        Ok(Self { connection, info })
    }

    /// Read immutable information without I/O, including after shutdown.
    #[must_use]
    pub fn info(&self) -> &StoreInfo {
        &self.info
    }

    /// Idempotently close this connection and all its clones. Independently
    /// opened connections and the caller's runtime remain usable. Concurrent
    /// callers join one attempt; abandoning a wait does not stop native work.
    ///
    /// # Errors
    /// Returns [`ErrorCode::ShutdownFailed`] on failed shutdown, retaining the
    /// closing state so this borrowed handle can retry. No implicit timeout.
    pub async fn close(&self) -> Result<(), StorageError> {
        self.connection
            .close()
            .await
            .map_err(StorageError::from_core)
    }
}

/// Stable machine-readable error vocabulary for storage, lifecycle and validation.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub enum ErrorCode {
    /// The current object does not exist in a live bucket.
    NoSuchKey,
    /// The requested multipart upload does not exist at this bucket and key.
    NoSuchUpload,
    /// A conflicting operation currently fences this resource.
    OperationAborted,
    /// A selected multipart part is missing or its acknowledged facts differ.
    InvalidPart,
    /// The request conflicts with the selected operation or its expected size.
    InvalidRequest,
    /// Selected multipart parts violate the negotiated ordering rules.
    InvalidPartOrder,
    /// A selected nonfinal multipart part is smaller than the minimum.
    EntityTooSmall,
    /// The upload source failed before successful terminal validation.
    BodyReadFailed,
    /// A supplied digest disagrees with the actual payload.
    BadDigest,
    /// Actual payload length differs from the declared length.
    IncompleteBody,
    /// Payload exceeds the operation's accepted size limit.
    EntityTooLarge,
    /// The requested live bucket does not exist.
    NoSuchBucket,
    /// This account already owns the bucket in a region requiring a conflict.
    BucketAlreadyOwnedByYou,
    /// Current objects prevent bucket retirement.
    BucketNotEmpty,
    /// The explicit owner assertion or authenticated permission does not match.
    AccessDenied,
    /// The requested application identity does not exist.
    NoSuchPrincipal,
    /// An identity already uses this exact name.
    PrincipalAlreadyExists,
    /// Revoke all keys explicitly before deleting their principal.
    PrincipalHasAccessKeys,
    /// The existing principal has no attached policy.
    NoSuchPrincipalPolicy,
    /// The bucket has no policy document.
    NoSuchBucketPolicy,
    /// The bucket has no CORS configuration.
    NoSuchCorsConfiguration,
    /// The live bucket has no lifecycle configuration.
    NoSuchLifecycleConfiguration,
    /// The bucket's public access block configuration is absent.
    NoSuchPublicAccessBlockConfiguration,
    /// Input does not satisfy the portable contract.
    InvalidArgument,
    /// A bucket name is outside the supported general-purpose namespace rules.
    InvalidBucketName,
    /// A read cache validator matched; no response body is created.
    NotModified,
    /// An operation precondition failed; no read body is created.
    PreconditionFailed,
    /// The requested byte range selects no bytes.
    InvalidRange,
    /// The selected completed-object part does not exist.
    InvalidPartNumber,
    /// An object response was explicitly closed or its read was canceled.
    BodyClosed,
    /// Collecting an object response would exceed the caller's explicit limit.
    BodyTooLarge,
    /// The owning operation canceled an active request or response read.
    OperationCanceled,
    /// Directory is unrelated or lacks positive initialization proof.
    InvalidStoreDirectory,
    /// Required persisted state is missing or inconsistent.
    StoreCorrupt,
    /// The marker or database operational format is unsupported.
    UnsupportedStoreVersion,
    /// The finite contention/admission budget was exhausted.
    StoreBusy,
    /// Explicit region differs from persisted configuration.
    RegionMismatch,
    /// Native storage or executor work failed.
    IoError,
    /// Connection is closing and no longer accepts work.
    StoreClosing,
    /// Connection has closed.
    StoreClosed,
    /// Shutdown failed and may be retried through the same handle.
    ShutdownFailed,
    /// This connection already owns a starting, active or closing listener.
    ListenerAlreadyRunning,
}

impl ErrorCode {
    /// Stable spelling shared with host-language errors.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::NoSuchPrincipal => "NoSuchPrincipal",
            Self::PrincipalAlreadyExists => "PrincipalAlreadyExists",
            Self::PrincipalHasAccessKeys => "PrincipalHasAccessKeys",
            Self::NoSuchPrincipalPolicy => "NoSuchPrincipalPolicy",
            Self::NoSuchBucketPolicy => "NoSuchBucketPolicy",
            Self::NoSuchCorsConfiguration => "NoSuchCORSConfiguration",
            Self::NoSuchLifecycleConfiguration => "NoSuchLifecycleConfiguration",
            Self::NoSuchPublicAccessBlockConfiguration => "NoSuchPublicAccessBlockConfiguration",
            Self::NoSuchUpload => "NoSuchUpload",
            Self::OperationAborted => "OperationAborted",
            Self::InvalidPart => "InvalidPart",
            Self::InvalidRequest => "InvalidRequest",
            Self::InvalidPartOrder => "InvalidPartOrder",
            Self::EntityTooSmall => "EntityTooSmall",
            Self::NoSuchKey => "NoSuchKey",
            Self::BodyReadFailed => "BodyReadFailed",
            Self::BadDigest => "BadDigest",
            Self::IncompleteBody => "IncompleteBody",
            Self::EntityTooLarge => "EntityTooLarge",
            Self::NoSuchBucket => "NoSuchBucket",
            Self::BucketAlreadyOwnedByYou => "BucketAlreadyOwnedByYou",
            Self::BucketNotEmpty => "BucketNotEmpty",
            Self::AccessDenied => "AccessDenied",
            Self::InvalidArgument => "InvalidArgument",
            Self::InvalidBucketName => "InvalidBucketName",
            Self::NotModified => "NotModified",
            Self::InvalidPartNumber => "InvalidPartNumber",
            Self::InvalidRange => "InvalidRange",
            Self::PreconditionFailed => "PreconditionFailed",
            Self::BodyClosed => "BodyClosed",
            Self::BodyTooLarge => "BodyTooLarge",
            Self::OperationCanceled => "OperationCanceled",
            Self::InvalidStoreDirectory => "InvalidStoreDirectory",
            Self::StoreCorrupt => "StoreCorrupt",
            Self::UnsupportedStoreVersion => "UnsupportedStoreVersion",
            Self::StoreBusy => "StoreBusy",
            Self::RegionMismatch => "RegionMismatch",
            Self::IoError => "IoError",
            Self::StoreClosing => "StoreClosing",
            Self::StoreClosed => "StoreClosed",
            Self::ShutdownFailed => "ShutdownFailed",
            Self::ListenerAlreadyRunning => "ListenerAlreadyRunning",
        }
    }
}

/// Failure with a stable code and trusted local error chaining. Message text is
/// descriptive, never an interface for matching errors. Lifecycle and value
/// errors make no claim about object mutation outcomes.
#[derive(Debug)]
pub struct StorageError {
    code: ErrorCode,
    cause: ErrorCause,
    mutation_outcome: Option<MutationOutcome>,
    partial_result: Option<DeleteObjectsProgress>,
}

/// Authoritative logical outcome of a failed storage mutation.
/// Absence on an error outside an operation never implies rollback.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub enum MutationOutcome {
    /// This attempt did not commit and cannot subsequently publish.
    NotCommitted,
    /// The logical mutation committed, even if a later request superseded it.
    Committed,
    /// Available authoritative evidence cannot determine whether it committed.
    Unknown,
    /// A batch has both committed and uncommitted items, with none uncertain.
    Partial,
}

impl MutationOutcome {
    /// Stable spelling shared with host-language errors.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::NotCommitted => "NotCommitted",
            Self::Committed => "Committed",
            Self::Unknown => "Unknown",
            Self::Partial => "Partial",
        }
    }
}

#[derive(Debug)]
enum ErrorCause {
    Operation(mikrio_core::OperationError),
    Connection(ConnectionError),
    Value(mikrio_core::ValueError),
    Object(mikrio_core::object::ObjectValueError),
    Identity(mikrio_core::access::IdentityError),
    Policy(mikrio_core::access::PolicyError),
    Cors(mikrio_core::cors::CorsError),
    Lifecycle(mikrio_core::lifecycle::LifecycleError),
    Body(mikrio_core::body::BodyError),
    IntegrityInput(mikrio_core::integrity::IntegrityError),
    External(std::io::Error),
}

impl StorageError {
    /// Stable classification, independent of the human-readable message.
    #[must_use]
    pub const fn code(&self) -> ErrorCode {
        self.code
    }

    /// Known logical outcome when this error belongs to a storage mutation.
    /// Standalone value construction and lifecycle errors return `None`, which
    /// must not be interpreted as `NotCommitted`.
    #[must_use]
    pub const fn mutation_outcome(&self) -> Option<&MutationOutcome> {
        self.mutation_outcome.as_ref()
    }

    fn cause(&self) -> &(dyn std::error::Error + 'static) {
        match &self.cause {
            ErrorCause::Operation(error) => error,
            ErrorCause::Connection(error) => error,
            ErrorCause::Value(error) => error,
            ErrorCause::Object(error) => error,
            ErrorCause::Identity(error) => error,
            ErrorCause::Policy(error) => error,
            ErrorCause::Cors(error) => error,
            ErrorCause::Lifecycle(error) => error,
            ErrorCause::Body(error) => error,
            ErrorCause::IntegrityInput(error) => error,
            ErrorCause::External(error) => error,
        }
    }

    fn from_value(cause: mikrio_core::ValueError) -> Self {
        Self {
            code: ErrorCode::InvalidArgument,
            cause: ErrorCause::Value(cause),
            mutation_outcome: None,
            partial_result: None,
        }
    }

    fn from_object(cause: mikrio_core::object::ObjectValueError) -> Self {
        let code = match cause {
            mikrio_core::object::ObjectValueError::BucketName => ErrorCode::InvalidBucketName,
            _ => ErrorCode::InvalidArgument,
        };

        Self {
            code,
            cause: ErrorCause::Object(cause),
            mutation_outcome: None,
            partial_result: None,
        }
    }

    fn from_policy(cause: mikrio_core::access::PolicyError) -> Self {
        Self {
            code: ErrorCode::InvalidArgument,
            cause: ErrorCause::Policy(cause),
            mutation_outcome: None,
            partial_result: None,
        }
    }

    fn from_identity(cause: mikrio_core::access::IdentityError) -> Self {
        Self {
            code: ErrorCode::InvalidArgument,
            cause: ErrorCause::Identity(cause),
            mutation_outcome: None,
            partial_result: None,
        }
    }

    fn from_integrity_input(cause: mikrio_core::integrity::IntegrityError) -> Self {
        Self {
            code: ErrorCode::InvalidArgument,
            cause: ErrorCause::IntegrityInput(cause),
            mutation_outcome: None,
            partial_result: None,
        }
    }

    fn from_body(cause: mikrio_core::body::BodyError) -> Self {
        use mikrio_core::body::BodyFailure;

        let code = match cause.failure() {
            BodyFailure::InvalidArgument => ErrorCode::InvalidArgument,
            BodyFailure::Closed => ErrorCode::BodyClosed,
            BodyFailure::Canceled => ErrorCode::OperationCanceled,
            BodyFailure::TooLarge => ErrorCode::BodyTooLarge,
            BodyFailure::Io => ErrorCode::IoError,
        };

        Self {
            code,
            cause: ErrorCause::Body(cause),
            mutation_outcome: None,
            partial_result: None,
        }
    }
}

impl fmt::Display for StorageError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(formatter, "{}: {}", self.code.as_str(), self.cause())
    }
}

impl std::error::Error for StorageError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        Some(self.cause())
    }
}

impl StorageError {
    fn from_operation(cause: mikrio_core::OperationError) -> Self {
        use mikrio_core::MutationOutcome as Outcome;

        let code = ErrorCode::from_operation(cause.failure());

        let mutation_outcome = cause.outcome().map(|outcome| match outcome {
            Outcome::NotCommitted => MutationOutcome::NotCommitted,
            Outcome::Committed => MutationOutcome::Committed,
            Outcome::Unknown => MutationOutcome::Unknown,
        });

        Self {
            code,
            cause: ErrorCause::Operation(cause),
            mutation_outcome,
            partial_result: None,
        }
    }

    fn from_core(cause: ConnectionError) -> Self {
        let code = match &cause {
            ConnectionError::Open(error) => match error.failure() {
                OpenFailure::InvalidArgument => ErrorCode::InvalidArgument,
                OpenFailure::InvalidStoreDirectory => ErrorCode::InvalidStoreDirectory,
                OpenFailure::StoreCorrupt => ErrorCode::StoreCorrupt,
                OpenFailure::UnsupportedStoreVersion => ErrorCode::UnsupportedStoreVersion,
                OpenFailure::StoreBusy => ErrorCode::StoreBusy,
                OpenFailure::RegionMismatch => ErrorCode::RegionMismatch,
                OpenFailure::IoError => ErrorCode::IoError,
            },
            ConnectionError::StoreClosing => ErrorCode::StoreClosing,
            ConnectionError::StoreClosed => ErrorCode::StoreClosed,
            ConnectionError::ShutdownFailed(_) => ErrorCode::ShutdownFailed,
            ConnectionError::ListenerAlreadyRunning => ErrorCode::ListenerAlreadyRunning,
            ConnectionError::ListenerIo(error)
                if error.kind() == std::io::ErrorKind::InvalidInput =>
            {
                ErrorCode::InvalidArgument
            }
            ConnectionError::ListenerIo(_) => ErrorCode::IoError,
        };

        Self {
            code,
            cause: ErrorCause::Connection(cause),
            mutation_outcome: None,
            partial_result: None,
        }
    }
}

impl ErrorCode {
    fn from_operation(failure: mikrio_core::OperationFailure) -> Self {
        use mikrio_core::OperationFailure as Failure;

        match failure {
            Failure::InvalidArgument => ErrorCode::InvalidArgument,
            Failure::NoSuchPrincipal => ErrorCode::NoSuchPrincipal,
            Failure::PrincipalAlreadyExists => ErrorCode::PrincipalAlreadyExists,
            Failure::PrincipalHasAccessKeys => ErrorCode::PrincipalHasAccessKeys,
            Failure::NoSuchPrincipalPolicy => ErrorCode::NoSuchPrincipalPolicy,
            Failure::NoSuchBucketPolicy => ErrorCode::NoSuchBucketPolicy,
            Failure::NoSuchCorsConfiguration => ErrorCode::NoSuchCorsConfiguration,
            Failure::NoSuchPublicAccessBlockConfiguration => {
                ErrorCode::NoSuchPublicAccessBlockConfiguration
            }
            Failure::NoSuchUpload => ErrorCode::NoSuchUpload,
            Failure::OperationAborted => ErrorCode::OperationAborted,
            Failure::InvalidPart => ErrorCode::InvalidPart,
            Failure::InvalidRequest => ErrorCode::InvalidRequest,
            Failure::InvalidPartOrder => ErrorCode::InvalidPartOrder,
            Failure::EntityTooSmall => ErrorCode::EntityTooSmall,
            Failure::NoSuchKey => ErrorCode::NoSuchKey,
            Failure::InvalidPartNumber => ErrorCode::InvalidPartNumber,
            Failure::InvalidRange => ErrorCode::InvalidRange,
            Failure::PreconditionFailed => ErrorCode::PreconditionFailed,
            Failure::NotModified => ErrorCode::NotModified,
            Failure::BodyReadFailed => ErrorCode::BodyReadFailed,
            Failure::BadDigest => ErrorCode::BadDigest,
            Failure::IncompleteBody => ErrorCode::IncompleteBody,
            Failure::EntityTooLarge => ErrorCode::EntityTooLarge,
            Failure::NoSuchBucket => ErrorCode::NoSuchBucket,
            Failure::BucketAlreadyOwnedByYou => ErrorCode::BucketAlreadyOwnedByYou,
            Failure::BucketNotEmpty => ErrorCode::BucketNotEmpty,
            Failure::AccessDenied => ErrorCode::AccessDenied,
            Failure::StoreClosing => ErrorCode::StoreClosing,
            Failure::StoreClosed => ErrorCode::StoreClosed,
            Failure::StoreBusy => ErrorCode::StoreBusy,
            Failure::OperationCanceled => ErrorCode::OperationCanceled,
            Failure::StoreCorrupt => ErrorCode::StoreCorrupt,
            Failure::IoError => ErrorCode::IoError,
        }
    }
}
