//! Store initialization and recovery on the native worker.

use std::fmt;
use std::fs::{self, File};
use std::io::{self, Write};
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

use crate::metadata::migration::{REGISTRY, Registry};
use crate::{
    BootstrapValues, DurableDirectory, EpochMillis, Guard, GuardManager, GuardMode, GuardTarget,
    IdentityMarker, MarkerError, MarkerState, MetadataDatabase, MetadataError, MetadataOpen,
    StoreId, StoreRecord,
};

#[cfg(test)]
pub(crate) mod fault;

/// Opening failure classification, translated explicitly by the public facade.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum OpenFailure {
    /// Invalid portable input, rejected before filesystem effects.
    InvalidArgument,
    /// Unrelated or unidentifiable incomplete directory; preserved for inspection.
    InvalidStoreDirectory,
    /// Required state is absent or inconsistent.
    StoreCorrupt,
    /// An unsupported marker or operational format.
    UnsupportedStoreVersion,
    /// Another owner currently excludes this acquisition attempt.
    StoreBusy,
    /// Explicit region differs from the committed store value.
    RegionMismatch,
    /// Native filesystem, entropy or SQLite work failed.
    IoError,
}

/// Whether opening may establish a new managed identity.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum OpenMode {
    /// Initialize missing or empty storage, or recover an existing identity.
    CreateIfMissing,
    /// Require a recognized identity; recovery and maintenance remain enabled.
    ExistingOnly,
}

/// Failed opening with a stable class and preserved local cause.
#[derive(Debug)]
pub struct OpenError {
    failure: OpenFailure,
    cause: Option<Box<dyn std::error::Error + Send + Sync>>,
    incomplete_guard: bool,
}

impl OpenError {
    pub(crate) fn new(failure: OpenFailure) -> Self {
        Self {
            failure,
            cause: None,
            incomplete_guard: false,
        }
    }

    pub(crate) fn caused(
        failure: OpenFailure,
        error: impl std::error::Error + Send + Sync + 'static,
    ) -> Self {
        Self {
            failure,
            cause: Some(Box::new(error)),
            incomplete_guard: false,
        }
    }

    fn incomplete_guard() -> Self {
        Self {
            failure: OpenFailure::InvalidStoreDirectory,
            cause: None,
            incomplete_guard: true,
        }
    }

    pub(crate) const fn can_recheck_initialization(&self) -> bool {
        self.incomplete_guard
    }

    /// Machine-readable classification; messages are not a matching interface.
    #[must_use]
    pub const fn failure(&self) -> OpenFailure {
        self.failure
    }
}

impl fmt::Display for OpenError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(match self.failure {
            OpenFailure::InvalidArgument => "invalid store open input",
            OpenFailure::InvalidStoreDirectory => "unrelated or incomplete store directory",
            OpenFailure::StoreCorrupt => "store state is missing or inconsistent",
            OpenFailure::UnsupportedStoreVersion => "unsupported store format",
            OpenFailure::StoreBusy => "store-use guard is busy",
            OpenFailure::RegionMismatch => "explicit region differs from the store region",
            OpenFailure::IoError => "native store open failed",
        })
    }
}

impl std::error::Error for OpenError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        self.cause
            .as_ref()
            .map(|cause| -> &(dyn std::error::Error + 'static) { cause.as_ref() })
    }
}

impl From<io::Error> for OpenError {
    fn from(error: io::Error) -> Self {
        Self::caused(OpenFailure::IoError, error)
    }
}

impl From<MetadataError> for OpenError {
    fn from(error: MetadataError) -> Self {
        let failure = match &error {
            MetadataError::Corrupt(_) => OpenFailure::StoreCorrupt,
            MetadataError::UnsupportedVersion(_) => OpenFailure::UnsupportedStoreVersion,
            MetadataError::InvalidRegion => OpenFailure::InvalidArgument,
            MetadataError::Sqlite(error)
                if matches!(
                    error.sqlite_error_code(),
                    Some(rusqlite::ErrorCode::DatabaseBusy | rusqlite::ErrorCode::DatabaseLocked)
                ) =>
            {
                OpenFailure::StoreBusy
            }
            MetadataError::Sqlite(error)
                if matches!(
                    error.sqlite_error_code(),
                    Some(rusqlite::ErrorCode::DatabaseCorrupt | rusqlite::ErrorCode::NotADatabase)
                ) =>
            {
                OpenFailure::StoreCorrupt
            }
            _ => OpenFailure::IoError,
        };

        Self::caused(failure, error)
    }
}

impl From<MarkerError> for OpenError {
    fn from(error: MarkerError) -> Self {
        let failure = match &error {
            MarkerError::UnsupportedVersion => OpenFailure::UnsupportedStoreVersion,
            MarkerError::Invalid => OpenFailure::StoreCorrupt,
            MarkerError::Io(_) => OpenFailure::IoError,
        };

        Self::caused(failure, error)
    }
}

/// Fully initialized native resource with shared store-use protection.
///
/// The database is declared before its guard so fallback destruction closes
/// SQLite before releasing protection. This is not the public async Store.
pub struct OpenedStore {
    database: MetadataDatabase,
    guard: Guard,
    directory: PathBuf,
    record: StoreRecord,
}

impl OpenedStore {
    /// Recognize, initialize or reopen a local store.
    ///
    /// Performs synchronous I/O. The native worker owns this entire call and
    /// any returned resource even if its caller stops awaiting the result.
    /// Busy is one nonblocking attempt; async bounded retry belongs to admission.
    ///
    /// # Errors
    /// Preserves unrelated/corrupt/incomplete data, refuses unsupported formats
    /// and conflicting regions, and returns native failures with their cause.
    pub fn open(directory: &Path, region: Option<&str>) -> Result<Self, OpenError> {
        Self::open_registered(directory, region, &REGISTRY)
    }

    /// Open under an explicit creation policy using the ordinary recovery protocol.
    ///
    /// # Errors
    /// Returns the same failures as [`Self::open`]. Existing-only opening refuses
    /// missing and empty directories without creating roots, guards or identities.
    pub fn open_with_mode(
        directory: &Path,
        region: Option<&str>,
        mode: OpenMode,
    ) -> Result<Self, OpenError> {
        Self::open_registered_with_mode(directory, region, &REGISTRY, mode)
    }

    fn open_registered(
        directory: &Path,
        region: Option<&str>,
        registry: &'static Registry,
    ) -> Result<Self, OpenError> {
        Self::open_registered_with_mode(directory, region, registry, OpenMode::CreateIfMissing)
    }

    fn open_registered_with_mode(
        directory: &Path,
        region: Option<&str>,
        registry: &'static Registry,
        mode: OpenMode,
    ) -> Result<Self, OpenError> {
        let directory = Self::prepare_with_mode(directory, region, mode)?;
        let root = DurableDirectory::open(&directory)?;
        let initial = inspect(&directory)?;

        if mode == OpenMode::ExistingOnly && matches!(initial, Layout::Empty) {
            return Err(OpenError::new(OpenFailure::InvalidStoreDirectory));
        }

        let manager = GuardManager::open(&directory)?;

        Self::open_prepared(directory, region, &root, &initial, &manager, registry)
    }

    /// Validate the portable path and optional region without filesystem access.
    ///
    /// # Errors
    /// Returns `InvalidArgument` for an empty/non-UTF-8 path or malformed region.
    pub fn validate(directory: &Path, region: Option<&str>) -> Result<(), OpenError> {
        if directory.as_os_str().is_empty()
            || directory.to_str().is_none()
            || region.is_some_and(|region| !crate::metadata::valid_region(region))
        {
            return Err(OpenError::new(OpenFailure::InvalidArgument));
        }

        Ok(())
    }

    pub(crate) fn absolute(directory: &Path, region: Option<&str>) -> Result<PathBuf, OpenError> {
        Self::validate(directory, region)?;
        let directory = std::path::absolute(directory)?;

        if directory.to_str().is_none() {
            return Err(OpenError::new(OpenFailure::InvalidArgument));
        }

        Ok(directory)
    }

    #[cfg(test)]
    pub(crate) fn prepare(directory: &Path, region: Option<&str>) -> Result<PathBuf, OpenError> {
        Self::prepare_with_mode(directory, region, OpenMode::CreateIfMissing)
    }

    pub(crate) fn prepare_with_mode(
        directory: &Path,
        region: Option<&str>,
        mode: OpenMode,
    ) -> Result<PathBuf, OpenError> {
        let directory = Self::absolute(directory, region)?;
        crate::metadata::validate_shared_runtime()?;

        if !directory.try_exists()? {
            if mode == OpenMode::ExistingOnly {
                return Err(OpenError::new(OpenFailure::InvalidStoreDirectory));
            }

            create_root(&directory)?;
        }

        let directory = directory.canonicalize()?;

        if directory.to_str().is_none() {
            return Err(OpenError::new(OpenFailure::InvalidArgument));
        }

        if !directory.is_dir() {
            return Err(OpenError::new(OpenFailure::InvalidStoreDirectory));
        }

        Ok(directory)
    }

    fn open_prepared(
        directory: PathBuf,
        region: Option<&str>,
        root: &DurableDirectory,
        initial: &Layout,
        manager: &GuardManager,
        registry: &'static Registry,
    ) -> Result<Self, OpenError> {
        if matches!(initial, Layout::GuardOnly) {
            // Do not race the guard creator for exclusive use before its initial
            // marker exists. It alone retains positive creation provenance.
            return Err(OpenError::incomplete_guard());
        }

        if matches!(initial, Layout::Recognized(marker) if marker.state() == MarkerState::Ready) {
            let guard = acquire(manager, GuardMode::Shared)?;
            return open_or_upgrade(directory, region, guard, manager, registry);
        }

        let created_guard = if matches!(initial, Layout::Empty) {
            #[cfg(test)]
            fault::hit("initialize.guard.create.before")?;

            match root.create_file(".mikrio.lock") {
                Ok(file) => {
                    #[cfg(test)]
                    fault::hit("initialize.guard.create.after")?;

                    crate::sync_file(&file)?;

                    #[cfg(test)]
                    fault::hit("initialize.guard.file_sync.after")?;

                    root.sync()?;

                    #[cfg(test)]
                    fault::hit("initialize.guard.directory_sync.after")?;

                    true
                }
                Err(error) if error.kind() == io::ErrorKind::AlreadyExists => false,
                Err(error) => return Err(error.into()),
            }
        } else {
            false
        };

        if matches!(initial, Layout::Empty) && !created_guard {
            return Err(OpenError::incomplete_guard());
        }

        let guard = acquire(manager, GuardMode::Exclusive)?;

        let marker = match inspect(&directory)? {
            Layout::GuardOnly if created_guard => {
                let store_id = StoreId::generate()
                    .map_err(|error| OpenError::caused(OpenFailure::IoError, error))?;

                let marker = IdentityMarker::initializing(store_id);
                publish_marker(&directory, root, &marker, MarkerPublication::Initial)?;
                marker
            }
            Layout::Recognized(marker) => {
                validate_temporary_markers(&directory, &marker)?;
                marker
            }
            Layout::GuardOnly => return Err(OpenError::incomplete_guard()),
            Layout::Empty => {
                return Err(OpenError::new(OpenFailure::InvalidStoreDirectory));
            }
        };

        if marker.state() == MarkerState::Initializing {
            initialize(&directory, root, &marker, region, registry)?;
        }

        // No in-place upgrade/downgrade. The next owner can change state during
        // this gap, so the shared path repeats marker/database/layout validation.
        drop(guard);
        let guard = acquire(manager, GuardMode::Shared)?;
        open_or_upgrade(directory, region, guard, manager, registry)
    }

    pub(crate) fn recheck(&self, region: Option<&str>) -> Result<(), OpenError> {
        let manager = GuardManager::open(&self.directory)?;
        let _guard = acquire(&manager, GuardMode::Shared)?;

        let Layout::Recognized(marker) = inspect(&self.directory)? else {
            return Err(OpenError::new(OpenFailure::StoreCorrupt));
        };

        if marker.state() != MarkerState::Ready {
            return Err(OpenError::new(OpenFailure::StoreCorrupt));
        }

        validate_temporary_markers(&self.directory, &marker)?;
        validate_layout(&self.directory)?;
        validate_file(&self.directory.join("metadata.sqlite3"))?;
        let record = self.database.read_store()?;
        validate_record(&record, &marker, region)?;

        if record != self.record {
            return Err(OpenError::new(OpenFailure::StoreCorrupt));
        }

        Ok(())
    }

    /// Validated immutable database facts; readable for the native owner's life.
    #[must_use]
    pub const fn record(&self) -> &StoreRecord {
        &self.record
    }

    /// Absolute resolved directory, independent of later working-directory changes.
    #[must_use]
    pub fn directory(&self) -> &Path {
        &self.directory
    }

    /// Close SQLite before dropping store-use protection.
    ///
    /// # Errors
    /// Returns the complete still-owned resource if SQLite close fails, so a
    /// connection state machine can remain Closing and retry safely.
    pub fn close(self) -> Result<(), (Box<Self>, MetadataError)> {
        let Self {
            database,
            guard,
            directory,
            record,
        } = self;

        match database.close() {
            Ok(()) => {
                drop(guard);
                Ok(())
            }
            Err((database, error)) => Err((
                Box::new(Self {
                    database: *database,
                    guard,
                    directory,
                    record,
                }),
                error,
            )),
        }
    }

    #[cfg(test)]
    pub(crate) fn test_checkpoint_authorizer(
        &mut self,
        denied: std::sync::Arc<std::sync::atomic::AtomicBool>,
        hits: std::sync::Arc<std::sync::atomic::AtomicUsize>,
    ) -> rusqlite::Result<()> {
        self.database.test_checkpoint_authorizer(denied, hits)
    }

    pub(crate) fn database_mut(&mut self) -> &mut MetadataDatabase {
        &mut self.database
    }
}

enum Layout {
    Empty,
    GuardOnly,
    Recognized(IdentityMarker),
}

fn inspect(directory: &Path) -> Result<Layout, OpenError> {
    let mut count = 0_u8;
    let mut has_guard = false;
    let mut has_marker = false;
    let mut only_provisional_names = true;

    for entry in fs::read_dir(directory)? {
        let entry = entry?;

        let name = entry
            .file_name()
            .into_string()
            .map_err(|_| OpenError::new(OpenFailure::InvalidStoreDirectory))?;

        let is_layout_entry = matches!(
            name.as_str(),
            "store.json"
                | ".mikrio.lock"
                | "metadata.sqlite3"
                | "metadata.sqlite3-wal"
                | "metadata.sqlite3-shm"
                | "metadata.sqlite3-journal"
                | "objects"
                | "staging"
                | "locks"
        );

        if !is_layout_entry && !temporary_name(&name) {
            return Err(OpenError::caused(
                OpenFailure::InvalidStoreDirectory,
                io::Error::new(
                    io::ErrorKind::InvalidData,
                    format!("unrecognized store root entry: {name}"),
                ),
            ));
        }

        count = count.saturating_add(1);
        has_guard |= name == ".mikrio.lock";
        has_marker |= name == "store.json";
        only_provisional_names &= name == ".mikrio.lock" || temporary_name(&name);
    }

    if count == 0 {
        return Ok(Layout::Empty);
    }

    if count == 1 && has_guard {
        return Ok(Layout::GuardOnly);
    }

    if !has_marker {
        if has_guard && only_provisional_names {
            return Err(OpenError::incomplete_guard());
        }

        return Err(OpenError::new(OpenFailure::InvalidStoreDirectory));
    }

    let marker = read_marker(&directory.join("store.json"))?;
    Ok(Layout::Recognized(marker))
}

// Temporary contents are inspected only with shared/exclusive protection. An
// initializer can be writing or renaming them during unprotected inspection.
fn validate_temporary_markers(directory: &Path, marker: &IdentityMarker) -> Result<(), OpenError> {
    for entry in fs::read_dir(directory)? {
        let entry = entry?;

        if entry.file_name().to_str().is_some_and(temporary_name)
            && read_marker(&entry.path())?.store_id() != marker.store_id()
        {
            return Err(OpenError::new(OpenFailure::StoreCorrupt));
        }
    }

    Ok(())
}

fn temporary_name(name: &str) -> bool {
    name.strip_prefix(".store-")
        .and_then(|name| name.strip_suffix(".tmp"))
        .is_some_and(|identity| StoreId::parse(identity).is_ok())
}

fn read_marker(path: &Path) -> Result<IdentityMarker, OpenError> {
    validate_file(path)?;
    Ok(IdentityMarker::read(File::open(path)?)?)
}

fn acquire(manager: &GuardManager, mode: GuardMode) -> Result<Guard, OpenError> {
    manager
        .try_acquire(GuardTarget::Store, mode)
        .map_err(|error| {
            let failure = if matches!(
                error.kind(),
                io::ErrorKind::NotFound | io::ErrorKind::InvalidData
            ) {
                OpenFailure::StoreCorrupt
            } else {
                OpenFailure::IoError
            };

            OpenError::caused(failure, error)
        })?
        .ok_or_else(|| OpenError::new(OpenFailure::StoreBusy))
}

fn initialize(
    directory: &Path,
    root: &DurableDirectory,
    marker: &IdentityMarker,
    region: Option<&str>,
    registry: &'static Registry,
) -> Result<(), OpenError> {
    validate_partial(directory)?;
    let path = directory.join("metadata.sqlite3");

    if !path.try_exists()? {
        #[cfg(test)]
        fault::hit("bootstrap.database.create.before")?;

        let file = root.create_file("metadata.sqlite3")?;

        #[cfg(test)]
        fault::hit("bootstrap.database.create.after")?;

        crate::sync_file(&file)?;

        #[cfg(test)]
        fault::hit("bootstrap.database.file_sync.after")?;

        root.sync()?;

        #[cfg(test)]
        fault::hit("bootstrap.database.directory_sync.after")?;
    }

    let mut database =
        MetadataDatabase::open_registered(&path, MetadataOpen::ResumeInitialization, registry)?;

    let needs_bootstrap = database.is_empty()?;

    if !needs_bootstrap {
        validate_record(&database.read_store()?, marker, region)?;
    }

    prepare_layout(directory)?;

    if needs_bootstrap {
        let milliseconds = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map_err(|error| OpenError::caused(OpenFailure::IoError, error))?
            .as_millis();

        let milliseconds = i64::try_from(milliseconds)
            .map_err(|error| OpenError::caused(OpenFailure::IoError, error))?;

        let created_at = EpochMillis::new(milliseconds)
            .map_err(|error| OpenError::caused(OpenFailure::IoError, error))?;

        let values = BootstrapValues::generate(
            marker.store_id(),
            region.unwrap_or("us-east-1").to_owned(),
            created_at,
        )?;

        database.initialize(&values)?;
    }

    validate_record(&database.read_store()?, marker, region)?;
    validate_layout(directory)?;
    database.enable_close_checkpoint()?;

    database
        .close()
        .map_err(|(_, error)| OpenError::from(error))?;

    root.sync()?;

    #[cfg(test)]
    fault::hit("ready.layout.directory_sync.after")?;

    publish_marker(directory, root, &marker.ready(), MarkerPublication::Ready)
}

fn open_or_upgrade(
    directory: PathBuf,
    region: Option<&str>,
    guard: Guard,
    manager: &GuardManager,
    registry: &'static Registry,
) -> Result<OpenedStore, OpenError> {
    let store = open_ready(directory.clone(), region, guard, registry)?;

    if store.record.format_version == registry.current().version {
        return Ok(store);
    }

    let identity = store.record.clone();
    // Never upgrade a shared lock in place. Another opener may win the gap;
    // re-read identity, version and schema after obtaining exclusive ownership.
    store.close().map_err(|(_, error)| OpenError::from(error))?;

    #[cfg(test)]
    fault::hit("upgrade.shared_close.after")?;

    let guard = acquire(manager, GuardMode::Exclusive)?;
    let mut store = open_ready(directory.clone(), region, guard, registry)?;

    if store.record.store_id != identity.store_id
        || store.record.region != identity.region
        || store.record.created_at != identity.created_at
    {
        return Err(OpenError::new(OpenFailure::StoreCorrupt));
    }

    store.database.upgrade(&directory)?;
    store.close().map_err(|(_, error)| OpenError::from(error))?;
    let guard = acquire(manager, GuardMode::Shared)?;
    open_ready(directory, region, guard, registry)
}

fn open_ready(
    directory: PathBuf,
    region: Option<&str>,
    guard: Guard,
    registry: &'static Registry,
) -> Result<OpenedStore, OpenError> {
    let Layout::Recognized(marker) = inspect(&directory)? else {
        return Err(OpenError::new(OpenFailure::StoreCorrupt));
    };

    if marker.state() != MarkerState::Ready {
        return Err(OpenError::new(OpenFailure::StoreBusy));
    }

    validate_temporary_markers(&directory, &marker)?;
    validate_layout(&directory)?;
    validate_file(&directory.join("metadata.sqlite3"))?;

    let database = MetadataDatabase::open_registered(
        &directory.join("metadata.sqlite3"),
        MetadataOpen::Existing,
        registry,
    )?;

    let record = database.read_store()?;
    validate_record(&record, &marker, region)?;
    database.enable_close_checkpoint()?;

    Ok(OpenedStore {
        database,
        guard,
        directory,
        record,
    })
}

fn validate_record(
    record: &StoreRecord,
    marker: &IdentityMarker,
    region: Option<&str>,
) -> Result<(), OpenError> {
    if record.store_id != marker.store_id() {
        return Err(OpenError::new(OpenFailure::StoreCorrupt));
    }

    if region.is_some_and(|region| region != record.region) {
        return Err(OpenError::new(OpenFailure::RegionMismatch));
    }

    Ok(())
}

#[derive(Clone, Copy)]
enum MarkerPublication {
    Initial,
    Ready,
}

fn publish_marker(
    directory: &Path,
    root: &DurableDirectory,
    marker: &IdentityMarker,
    publication: MarkerPublication,
) -> Result<(), OpenError> {
    let nonce =
        StoreId::generate().map_err(|error| OpenError::caused(OpenFailure::IoError, error))?;

    let name = format!(".store-{nonce}.tmp");

    #[cfg(test)]
    fault::marker(publication, "create.before")?;

    let mut file = root.create_file(&name)?;

    #[cfg(test)]
    fault::marker(publication, "create.after")?;

    file.write_all(&marker.encode()?)?;

    #[cfg(test)]
    fault::marker(publication, "write.after")?;

    crate::sync_file(&file)?;

    #[cfg(test)]
    fault::marker(publication, "file_sync.after")?;

    drop(file);

    if matches!(publication, MarkerPublication::Ready) {
        // Replacing the marker requires exclusive store use. Initial publication
        // below always preserves a pre-existing marker.
        #[cfg(test)]
        fault::marker(publication, "rename.before")?;

        fs::rename(directory.join(&name), directory.join("store.json"))?;

        #[cfg(test)]
        fault::marker(publication, "rename.after")?;

        root.sync()?;

        #[cfg(test)]
        fault::marker(publication, "directory_sync.after")?;
    } else {
        #[cfg(test)]
        fault::marker(publication, "publish.before")?;

        root.publish_new(&name, root, "store.json")?;

        #[cfg(test)]
        fault::marker(publication, "publish.after")?;
    }

    Ok(())
}

fn create_root(path: &Path) -> Result<(), OpenError> {
    let mut missing = Vec::new();
    let mut current = path;

    while !current.try_exists()? {
        missing.push(current);

        current = current
            .parent()
            .ok_or_else(|| io::Error::other("missing directory parent"))?;
    }

    // A portable alias can resolve to a nonportable parent. Refuse it before
    // creating any child, not only after resolving the final store directory.
    if current.canonicalize()?.to_str().is_none() {
        return Err(OpenError::new(OpenFailure::InvalidArgument));
    }

    let builder = fs::DirBuilder::new();

    #[cfg(unix)]
    let builder = {
        use std::os::unix::fs::DirBuilderExt;
        let mut builder = builder;
        builder.mode(0o700);
        builder
    };

    for directory in missing.into_iter().rev() {
        match builder.create(directory) {
            Ok(()) => {}
            Err(error) if error.kind() == io::ErrorKind::AlreadyExists => {}
            Err(error) => return Err(error.into()),
        }

        DurableDirectory::open(directory)?.sync()?;

        let parent = directory
            .parent()
            .ok_or_else(|| io::Error::other("missing directory parent"))?;

        DurableDirectory::open(parent)?.sync()?;
    }

    Ok(())
}

// Recovery may repeat only the expected empty initialization artifacts. Inspect
// every existing child before creating any missing directory/guard/database.
fn validate_partial(directory: &Path) -> Result<(), OpenError> {
    for name in ["objects", "staging", "locks"] {
        let parent = directory.join(name);

        match parent.symlink_metadata() {
            Ok(metadata) if metadata.is_dir() => {}
            Ok(_) => return Err(OpenError::new(OpenFailure::StoreCorrupt)),
            Err(error) if error.kind() == io::ErrorKind::NotFound => continue,
            Err(error) => return Err(error.into()),
        }

        for entry in fs::read_dir(&parent)? {
            let entry = entry?;
            let filename = entry.file_name();

            let Some(filename) = filename.to_str() else {
                return Err(OpenError::new(OpenFailure::StoreCorrupt));
            };

            let shard = if name == "locks" {
                filename.strip_suffix(".lock")
            } else {
                Some(filename)
            };

            let Some(shard) = shard else {
                return Err(OpenError::new(OpenFailure::StoreCorrupt));
            };

            if shard.len() != 2 {
                return Err(OpenError::new(OpenFailure::StoreCorrupt));
            }

            if !shard
                .bytes()
                .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
            {
                return Err(OpenError::new(OpenFailure::StoreCorrupt));
            }

            if name == "locks" {
                validate_file(&entry.path())?;
                continue;
            }

            if !entry.path().symlink_metadata()?.is_dir()
                || fs::read_dir(entry.path())?.next().transpose()?.is_some()
            {
                return Err(OpenError::new(OpenFailure::StoreCorrupt));
            }
        }
    }

    for name in [
        "metadata.sqlite3",
        "metadata.sqlite3-wal",
        "metadata.sqlite3-shm",
        "metadata.sqlite3-journal",
    ] {
        let path = directory.join(name);

        match path.symlink_metadata() {
            Ok(_) => validate_file(&path)?,
            Err(error) if error.kind() == io::ErrorKind::NotFound => continue,
            Err(error) => return Err(error.into()),
        }

        if name != "metadata.sqlite3" && !directory.join("metadata.sqlite3").try_exists()? {
            return Err(OpenError::new(OpenFailure::StoreCorrupt));
        }
    }

    Ok(())
}

fn ensure_directory(path: &Path) -> Result<(), OpenError> {
    match fs::create_dir(path) {
        Ok(()) => {}
        Err(error) if error.kind() == io::ErrorKind::AlreadyExists => {}
        Err(error) => return Err(error.into()),
    }

    if !path.symlink_metadata()?.is_dir() {
        return Err(OpenError::new(OpenFailure::StoreCorrupt));
    }

    Ok(())
}

fn prepare_layout(directory: &Path) -> Result<(), OpenError> {
    for name in ["objects", "staging", "locks"] {
        ensure_directory(&directory.join(name))?;

        #[cfg(test)]
        fault::hit("bootstrap.layout.parent_create.after")?;
    }

    for name in ["objects", "staging"] {
        let parent = directory.join(name);

        for shard in 0..=u8::MAX {
            let path = parent.join(format!("{shard:02x}"));
            ensure_directory(&path)?;

            #[cfg(test)]
            fault::hit("bootstrap.layout.shard_create.after")?;

            if fs::read_dir(&path)?.next().transpose()?.is_some() {
                return Err(OpenError::new(OpenFailure::StoreCorrupt));
            }

            DurableDirectory::open(&path)?.sync()?;

            #[cfg(test)]
            fault::hit("bootstrap.layout.shard_sync.after")?;
        }

        DurableDirectory::open(&parent)?.sync()?;

        #[cfg(test)]
        fault::hit("bootstrap.layout.parent_sync.after")?;
    }

    let locks = DurableDirectory::open(&directory.join("locks"))?;

    for shard in 0..=u8::MAX {
        let name = format!("{shard:02x}.lock");

        match locks.create_file(&name) {
            Ok(file) => {
                #[cfg(test)]
                fault::hit("bootstrap.layout.guard_create.after")?;

                crate::sync_file(&file)?;

                #[cfg(test)]
                fault::hit("bootstrap.layout.guard_sync.after")?;
            }
            Err(error) if error.kind() == io::ErrorKind::AlreadyExists => {}
            Err(error) => return Err(error.into()),
        }
    }

    locks.sync()?;

    #[cfg(test)]
    fault::hit("bootstrap.layout.locks_sync.after")?;

    DurableDirectory::open(directory)?.sync()?;
    Ok(())
}

fn validate_layout(directory: &Path) -> Result<(), OpenError> {
    for name in ["objects", "staging", "locks"] {
        if !directory
            .join(name)
            .symlink_metadata()
            .map_err(required)?
            .is_dir()
        {
            return Err(OpenError::new(OpenFailure::StoreCorrupt));
        }
    }

    for shard in 0..=u8::MAX {
        for parent in ["objects", "staging"] {
            if !directory
                .join(parent)
                .join(format!("{shard:02x}"))
                .symlink_metadata()
                .map_err(required)?
                .is_dir()
            {
                return Err(OpenError::new(OpenFailure::StoreCorrupt));
            }
        }

        validate_file(&directory.join("locks").join(format!("{shard:02x}.lock")))?;
    }

    Ok(())
}

fn validate_file(path: &Path) -> Result<(), OpenError> {
    let metadata = path.symlink_metadata().map_err(required)?;

    if !metadata.is_file() {
        return Err(OpenError::new(OpenFailure::StoreCorrupt));
    }

    #[cfg(windows)]
    crate::windows::path_file_identity(path).map_err(|error| {
        if error.kind() == io::ErrorKind::InvalidData {
            return OpenError::new(OpenFailure::StoreCorrupt);
        }

        OpenError::from(error)
    })?;

    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt;

        if metadata.nlink() != 1 {
            return Err(OpenError::new(OpenFailure::StoreCorrupt));
        }
    }

    Ok(())
}

fn required(error: io::Error) -> OpenError {
    let failure = if error.kind() == io::ErrorKind::NotFound {
        OpenFailure::StoreCorrupt
    } else {
        OpenFailure::IoError
    };

    OpenError::caused(failure, error)
}

#[cfg(test)]
mod tests;
