//! Internal, provisional storage primitives for mikrio.
//!
//! Includes a synchronous native initializer, but not the supported async Store
//! API. Platform probes do not qualify complete object publication.

pub mod access;
pub mod body;
mod connection;
pub mod cors;
pub mod diagnostics;
mod filesystem;
mod guards;
mod initialize;
pub mod integrity;
pub mod lifecycle;
mod marker;
mod metadata;
pub mod multipart;
pub mod object;
mod operation;
mod values;

#[cfg(windows)]
mod windows;

#[cfg(test)]
#[path = "../tests/support/directory.rs"]
mod test_directory;

#[cfg(test)]
#[path = "../tests/support/backup.rs"]
mod test_backup;

#[cfg(test)]
#[path = "../tests/support/process.rs"]
mod test_process;

#[cfg(test)]
#[path = "../tests/support/allocations.rs"]
mod test_allocations;

#[cfg(test)]
#[path = "../tests/support/metrics.rs"]
mod test_metrics;

#[cfg(test)]
mod runtime_probe;

#[cfg(test)]
mod protocol_fault;

pub use connection::{
    CommitSection, Connection, ConnectionError, ConnectionInfo, ListenerControl, ListenerLease,
    Operation, WorkProgress,
};
pub use filesystem::{DurableDirectory, sync_file};
pub use guards::{Guard, GuardManager, GuardMode, GuardSet, GuardTarget};
pub use initialize::{OpenError, OpenFailure, OpenMode, OpenedStore};
pub use marker::{IdentityMarker, MarkerError, MarkerState};
pub use metadata::{
    AccessError, AccessRequest, BootstrapValues, MetadataDatabase, MetadataError, MetadataOpen,
    OwnedAccessRequest, PublicAccessBlock, StoreRecord,
};
pub use operation::{CacheValidators, MutationOutcome, OperationError, OperationFailure};
pub use values::{BlobId, EpochMillis, StoreId, UploadId, ValueError};
