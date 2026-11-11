//! Bundled SQLite policy and transaction primitives. No adapter-owned SQL.

use std::fmt;
use std::path::Path;
use std::time::Duration;

use crate::{EpochMillis, OperationError, OperationFailure, StoreId};
use rusqlite::{Connection, OpenFlags, config::DbConfig};

mod access;
mod admin;
mod bucket_configuration;
mod bucket_listing;
mod buckets;
mod checkpoint;
mod cors;
pub(crate) mod gc;
pub(crate) mod lifecycle;
mod listing;
pub(crate) mod migration;
mod multipart;
mod namespace;
mod objects;
pub(crate) mod receipts;
mod retirement;
pub(crate) mod sweep;
mod token;
mod transaction;
pub use access::{AccessError, AccessRequest, OwnedAccessRequest, PublicAccessBlock};

const DDL: &str = include_str!("../schema.sql");
const STATEMENT_CACHE_CAPACITY: usize = 16;

/// Immutable validated singleton values, without the continuation-token secret.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StoreRecord {
    /// The identity shared with the bootstrap marker.
    pub store_id: StoreId,
    /// Operational schema version, not the marker version.
    pub format_version: u32,
    /// Persisted signing region.
    pub region: String,
    /// Original creation instant, preserved on reopen/migration.
    pub created_at: EpochMillis,
}

/// Private bootstrap values prepared before entering the SQL transaction.
/// Deliberately has no Debug implementation because it owns secret material.
pub struct BootstrapValues {
    record: StoreRecord,
    owner_account_id: String,
    canonical_owner_id: String,
    token_key: [u8; 32],
    sweep_generations: Vec<[u8; 16]>,
}

impl BootstrapValues {
    /// Prepare random owner/token/sweep identities without filesystem effects.
    ///
    /// # Errors
    /// Rejects invalid regions and returns OS entropy failures without fallback.
    pub fn generate(
        store_id: StoreId,
        region: String,
        created_at: EpochMillis,
    ) -> Result<Self, MetadataError> {
        if !valid_region(&region) {
            return Err(MetadataError::InvalidRegion);
        }

        let mut owner_account_id = String::with_capacity(12);

        while owner_account_id.len() < 12 {
            let mut bytes = [0; 16];
            getrandom::fill(&mut bytes).map_err(MetadataError::Entropy)?;

            for byte in bytes {
                if byte < 250 && owner_account_id.len() < 12 {
                    owner_account_id.push(char::from(b'0' + byte % 10));
                }
            }
        }

        let mut owner = [0; 32];
        let mut token_key = [0; 32];
        getrandom::fill(&mut owner).map_err(MetadataError::Entropy)?;
        getrandom::fill(&mut token_key).map_err(MetadataError::Entropy)?;
        let mut canonical_owner_id = String::with_capacity(64);

        for byte in owner {
            canonical_owner_id.push(char::from(b"0123456789abcdef"[usize::from(byte >> 4)]));
            canonical_owner_id.push(char::from(b"0123456789abcdef"[usize::from(byte & 15)]));
        }

        let mut sweep_generations = vec![[0; 16]; 259];

        for generation in &mut sweep_generations {
            getrandom::fill(generation).map_err(MetadataError::Entropy)?;
        }

        Ok(Self {
            record: StoreRecord {
                store_id,
                format_version: migration::REGISTRY.current().version,
                region,
                created_at,
            },
            owner_account_id,
            canonical_owner_id,
            token_key,
            sweep_generations,
        })
    }
}

/// Whether exclusive initialization may establish WAL on an empty database.
#[derive(Clone, Copy)]
pub enum MetadataOpen {
    /// Only the recognized store initializer may select this mode.
    Initialize,
    /// A recognized Initializing marker permits resuming an empty database;
    /// committed metadata still has to pass complete schema/identity validation.
    ResumeInitialization,
    /// Existing metadata must already use the expected WAL mode.
    Existing,
    /// Raw SQLite fixtures exercise transactions without claiming a store format.
    #[cfg(test)]
    SqliteFixture,
}

/// Owned metadata connection, intended for the bounded metadata worker.
///
/// No connection/statement or caller SQL escapes to an adapter. Callers must
/// retain store-use protection throughout this object's lifetime.
pub struct MetadataDatabase {
    connection: Connection,
    registry: &'static migration::Registry,
    unusable: bool,
    #[cfg(test)]
    transaction_fault: Option<transaction::Fault>,
}

/// Failed SQLite work or refusal of an unsupported connection policy.
#[derive(Debug)]
pub enum MetadataError {
    /// The operational format has no supported decoder or migration.
    UnsupportedVersion(i64),
    /// Input region is outside the portable signing-region envelope.
    InvalidRegion,
    /// Both the original transaction and its explicit rollback failed.
    Settlement {
        /// Original failure; does not imply a mutation outcome.
        operation: Box<MetadataError>,
        /// Failed settlement; the connection cannot prove committed state.
        rollback: rusqlite::Error,
    },
    /// Operating-system entropy was unavailable during bootstrap preparation.
    Entropy(getrandom::Error),
    /// Stored state violates the exact schema or validated value contract.
    Corrupt(&'static str),
    /// Filesystem failure while resolving the managed directory.
    Io(std::io::Error),
    /// Native SQLite failure; transaction state is not inferred from this alone.
    Sqlite(rusqlite::Error),
    /// The linked runtime or stored connection settings are incompatible.
    Incompatible(&'static str),
}

impl fmt::Display for MetadataError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::UnsupportedVersion(_) => {
                formatter.write_str("unsupported operational store version")
            }
            Self::InvalidRegion => formatter.write_str("invalid store region"),
            Self::Settlement { .. } => {
                formatter.write_str("metadata transaction settlement failed")
            }
            Self::Entropy(_) => formatter.write_str("bootstrap entropy unavailable"),
            Self::Corrupt(message) | Self::Incompatible(message) => formatter.write_str(message),
            Self::Io(_) => formatter.write_str("metadata directory could not be resolved"),
            Self::Sqlite(_) => formatter.write_str("metadata operation failed"),
        }
    }
}

impl std::error::Error for MetadataError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::Settlement { operation, .. } => Some(operation.as_ref()),
            Self::Entropy(error) => Some(error),
            Self::Io(error) => Some(error),
            Self::Sqlite(error) => Some(error),
            Self::Incompatible(_)
            | Self::Corrupt(_)
            | Self::UnsupportedVersion(_)
            | Self::InvalidRegion => None,
        }
    }
}

impl From<rusqlite::Error> for MetadataError {
    fn from(error: rusqlite::Error) -> Self {
        Self::Sqlite(error)
    }
}

impl MetadataDatabase {
    /// Open an existing file and validate the actual bundled runtime/settings.
    ///
    /// The initializer exclusively creates the file first. This function
    /// never creates a missing database, executes DDL, or resets a corrupt one.
    /// It performs synchronous I/O and belongs on the metadata worker.
    ///
    /// # Errors
    /// Reports SQLite errors or unsupported runtime/configuration. Ordinary
    /// opens refuse non-WAL databases instead of changing their journal mode.
    pub fn open(path: &Path, mode: MetadataOpen) -> Result<Self, MetadataError> {
        let database = Self::open_registered(path, mode, &migration::REGISTRY)?;
        database.enable_close_checkpoint()?;
        Ok(database)
    }

    pub(crate) fn open_registered(
        path: &Path,
        mode: MetadataOpen,
        registry: &'static migration::Registry,
    ) -> Result<Self, MetadataError> {
        registry.validate()?;

        // Store directory aliases are supported; SQLite NOFOLLOW also rejects
        // symlinked ancestors such as macOS /var. Resolve only the parent and
        // preserve refusal of a symlink at the database filename itself.
        let name = path
            .file_name()
            .ok_or(MetadataError::Incompatible("missing database filename"))?;

        let parent = path
            .parent()
            .filter(|parent| !parent.as_os_str().is_empty())
            .unwrap_or_else(|| Path::new("."));

        let path = parent.canonicalize().map_err(MetadataError::Io)?.join(name);
        let metadata = path.symlink_metadata().map_err(MetadataError::Io)?;

        if !metadata.is_file() {
            return Err(MetadataError::Corrupt("metadata is not a regular file"));
        }

        #[cfg(windows)]
        crate::windows::path_file_identity(&path).map_err(|error| {
            if error.kind() == std::io::ErrorKind::InvalidData {
                return MetadataError::Corrupt("metadata is not a single regular file");
            }

            MetadataError::Io(error)
        })?;

        #[cfg(unix)]
        {
            use std::os::unix::fs::MetadataExt;

            if metadata.nlink() != 1 {
                return Err(MetadataError::Corrupt(
                    "hard-linked metadata is unsupported",
                ));
            }
        }

        validate_shared_runtime()?;

        let connection = Connection::open_with_flags(
            &path,
            OpenFlags::SQLITE_OPEN_READ_WRITE
                | OpenFlags::SQLITE_OPEN_NO_MUTEX
                | OpenFlags::SQLITE_OPEN_PRIVATE_CACHE
                | OpenFlags::SQLITE_OPEN_NOFOLLOW,
        )?;

        // Refusal must not checkpoint unvalidated WAL into the database or
        // remove the original recovery evidence when this connection drops.
        if !connection.set_db_config(DbConfig::SQLITE_DBCONFIG_NO_CKPT_ON_CLOSE, true)? {
            return Err(MetadataError::Incompatible(
                "refused close checkpoint policy",
            ));
        }

        validate_runtime(&connection)?;
        discover_version(&connection, mode, registry)?;
        configure(&connection, mode)?;

        Ok(Self {
            connection,
            registry,
            unusable: false,
            #[cfg(test)]
            transaction_fault: None,
        })
    }

    /// Restore SQLite's ordinary checkpoint-on-close behavior after validation.
    pub(crate) fn enable_close_checkpoint(&self) -> Result<(), MetadataError> {
        if self
            .connection
            .set_db_config(DbConfig::SQLITE_DBCONFIG_NO_CKPT_ON_CLOSE, false)?
        {
            return Err(MetadataError::Incompatible(
                "refused close checkpoint policy",
            ));
        }

        Ok(())
    }

    /// Whether SQLite has no schema objects after recovering its own journal.
    ///
    /// # Errors
    /// Reports a corrupt/unreadable database; never interprets that as empty.
    pub fn is_empty(&self) -> Result<bool, MetadataError> {
        Ok(!self
            .connection
            .prepare("SELECT 1 FROM sqlite_schema LIMIT 1")?
            .exists([])?)
    }

    /// The observed version and source identity of the linked native library.
    ///
    /// # Errors
    /// Reports failure to read the actual SQLite source ID.
    pub fn runtime_identity(&self) -> Result<(String, String), MetadataError> {
        Ok(self
            .connection
            .query_row("SELECT sqlite_version(), sqlite_source_id()", [], |row| {
                Ok((row.get(0)?, row.get(1)?))
            })?)
    }

    /// Create the schema, singleton record and sweeps in one transaction.
    ///
    /// Requires exclusive store-use protection and a recognized Initializing
    /// marker. This is a SQL primitive, not a successful public Store open.
    ///
    /// # Errors
    /// Existing schema, constraints and transaction errors are preserved. No
    /// caller may infer rollback merely from a returned COMMIT error.
    pub fn initialize(&mut self, values: &BootstrapValues) -> Result<(), MetadataError> {
        self.connection.flush_prepared_statement_cache();

        #[cfg(test)]
        crate::initialize::fault::hit("bootstrap.sql.begin.before").map_err(MetadataError::Io)?;

        let mut transaction = self
            .connection
            .transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)?;

        let result = (|| -> Result<(), MetadataError> {
            #[cfg(test)]
            crate::initialize::fault::hit("bootstrap.sql.begin.after")
                .map_err(MetadataError::Io)?;

            self.registry.current().create_schema(&transaction)?;

            #[cfg(test)]
            crate::initialize::fault::hit("bootstrap.sql.schema.after")
                .map_err(MetadataError::Io)?;

            transaction.execute(
                "INSERT INTO store_metadata (singleton_id, store_id, owner_account_id,
                 canonical_owner_id, format_version, region, shard_count, shard_algorithm,
                 continuation_token_key, created_at_ms)
                 VALUES (1, ?1, ?2, ?3, ?7, ?4, 256, 'first_byte', ?5, ?6)",
                rusqlite::params![
                    values.record.store_id.as_bytes().as_slice(),
                    values.owner_account_id,
                    values.canonical_owner_id,
                    values.record.region,
                    values.token_key.as_slice(),
                    values.record.created_at.get(),
                    self.registry.current().version
                ],
            )?;

            #[cfg(test)]
            crate::initialize::fault::hit("bootstrap.sql.singleton.after")
                .map_err(MetadataError::Io)?;

            {
                let mut statement = transaction.prepare(
                    "INSERT INTO maintenance_sweeps (task, shard, generation, upper_row_id, cursor_row_id, started_at_ms) VALUES (?1, ?2, ?3, 0, 0, ?4)",
                )?;

                let tasks = (0..256).map(|shard| ("gc_blobs", shard)).chain([
                    ("expire_objects", -1),
                    ("abort_uploads", -1),
                    ("purge_receipts", -1),
                ]);

                for ((task, shard), generation) in tasks.zip(&values.sweep_generations) {
                    statement.execute(rusqlite::params![
                        task,
                        shard,
                        generation.as_slice(),
                        values.record.created_at.get()
                    ])?;
                }
            }

            #[cfg(test)]
            crate::initialize::fault::hit("bootstrap.sql.sweeps.after")
                .map_err(MetadataError::Io)?;

            let violations = transaction
                .prepare("PRAGMA foreign_key_check")?
                .exists([])?;

            if violations {
                return Err(MetadataError::Corrupt("bootstrap foreign-key violation"));
            }

            #[cfg(test)]
            crate::initialize::fault::hit("bootstrap.sql.commit.before")
                .map_err(MetadataError::Io)?;

            transaction.execute_batch("COMMIT")?;

            #[cfg(test)]
            crate::initialize::fault::hit("bootstrap.sql.commit.after")
                .map_err(MetadataError::Io)?;

            Ok(())
        })();

        transaction.flush_prepared_statement_cache();

        if let Err(error) = result {
            if !transaction.is_autocommit()
                && let Err(rollback) = transaction.execute_batch("ROLLBACK")
            {
                // Preserve ownership for explicit close; do not let Drop
                // silently retry and hide that settlement failed.
                transaction.set_drop_behavior(rusqlite::DropBehavior::Ignore);
                self.unusable = true;

                return Err(MetadataError::Settlement {
                    operation: Box::new(error),
                    rollback,
                });
            }

            return Err(error);
        }

        Ok(())
    }

    /// Validate the exact operational schema and decode the immutable singleton.
    ///
    /// # Errors
    /// Rejects newer/unsupported versions before interpreting ordinary tables,
    /// altered/missing schema objects, malformed identities/configuration and
    /// invalid timestamps. Never substitutes defaults for malformed stored data.
    pub fn read_store(&self) -> Result<StoreRecord, MetadataError> {
        if self.unusable || !self.connection.is_autocommit() {
            return Err(MetadataError::Corrupt(
                "metadata transaction has not settled",
            ));
        }

        let version = migration::version(&self.connection)?;
        let format = self.registry.format(version)?;

        // Fixed-size schema inspection, not an integrity scan over stored objects.
        // The reference is the bundled DDL, never SQL read from a store directory.
        format.validate_schema(&self.connection)?;

        let singleton_count: i64 = self.connection.query_row(
            "SELECT count(*) FROM (SELECT singleton_id FROM store_metadata LIMIT 2)",
            [],
            |row| row.get(0),
        )?;

        if singleton_count != 1 {
            return Err(MetadataError::Corrupt("invalid store singleton count"));
        }

        validate_sweeps(&self.connection)?;

        let (store_id, account, owner, region, shards, algorithm, token_length, created_at) =
            self.connection.query_row(
                "SELECT store_id, owner_account_id, canonical_owner_id, region, shard_count, shard_algorithm, length(continuation_token_key), created_at_ms FROM store_metadata WHERE singleton_id = 1",
                [],
                |row| {
                    Ok((
                        row.get::<_, Vec<u8>>(0)?,
                        row.get::<_, String>(1)?,
                        row.get::<_, String>(2)?,
                        row.get::<_, String>(3)?,
                        row.get::<_, i64>(4)?,
                        row.get::<_, String>(5)?,
                        row.get::<_, i64>(6)?,
                        row.get::<_, i64>(7)?,
                    ))
                },
            ).map_err(decode_error)?;

        if account.len() != 12 || !account.bytes().all(|byte| byte.is_ascii_digit()) {
            return Err(MetadataError::Corrupt("invalid store configuration"));
        }

        if owner.len() != 64 {
            return Err(MetadataError::Corrupt("invalid store configuration"));
        }

        if !owner
            .bytes()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
        {
            return Err(MetadataError::Corrupt("invalid store configuration"));
        }

        if !valid_region(&region) {
            return Err(MetadataError::Corrupt("invalid store configuration"));
        }

        if shards != 256 || algorithm != "first_byte" || token_length != 32 {
            return Err(MetadataError::Corrupt("invalid store configuration"));
        }

        let store_id = StoreId::from_bytes(
            store_id
                .try_into()
                .map_err(|_| MetadataError::Corrupt("invalid store identity"))?,
        );

        let created_at = EpochMillis::new(created_at)
            .map_err(|_| MetadataError::Corrupt("invalid store creation time"))?;

        Ok(StoreRecord {
            store_id,
            format_version: format.version,
            region,
            created_at,
        })
    }

    /// Explicitly close after all worker commands and statements have settled.
    ///
    /// # Errors
    /// On failure, returns the still-owned connection for shutdown retry.
    pub fn close(self) -> Result<(), (Box<Self>, MetadataError)> {
        self.connection.close().map_err(|(connection, error)| {
            (
                Box::new(Self {
                    connection,
                    registry: self.registry,
                    unusable: self.unusable,
                    #[cfg(test)]
                    transaction_fault: self.transaction_fault,
                }),
                MetadataError::Sqlite(error),
            )
        })
    }
}

pub(crate) fn valid_region(region: &str) -> bool {
    if !(1..=63).contains(&region.len()) {
        return false;
    }

    if region.starts_with('-') || region.ends_with('-') {
        return false;
    }

    region
        .bytes()
        .all(|byte| byte.is_ascii_lowercase() || byte.is_ascii_digit() || byte == b'-')
}

fn now() -> Result<EpochMillis, OperationError> {
    EpochMillis::from_system_time(std::time::SystemTime::now())
        .map_err(|error| OperationError::caused(OperationFailure::IoError, error))
}

// Only used for fixed store-decoding SQL. SQLITE_ERROR here means the stable
// discovery columns/table are missing or malformed, not a caller SQL mistake.
fn decode_error(error: rusqlite::Error) -> MetadataError {
    match error {
        rusqlite::Error::QueryReturnedNoRows | rusqlite::Error::InvalidColumnType(..) => {
            MetadataError::Corrupt("invalid store singleton")
        }
        rusqlite::Error::SqliteFailure(ref code, _) if code.extended_code == 1 => {
            MetadataError::Corrupt("invalid store discovery schema")
        }
        error => MetadataError::Sqlite(error),
    }
}

fn schema(
    connection: &Connection,
) -> Result<Vec<(String, String, String, String)>, rusqlite::Error> {
    connection
        .prepare(
            "SELECT type, name, tbl_name, sql FROM sqlite_schema
             WHERE name NOT GLOB 'sqlite_*' ORDER BY type, name LIMIT 65",
        )?
        .query_map([], |row| {
            Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?))
        })?
        .collect()
}

fn validate_sweeps(connection: &Connection) -> Result<(), MetadataError> {
    let mut statement = connection.prepare(
        "SELECT task, shard, length(generation), cursor_row_id, upper_row_id, started_at_ms FROM maintenance_sweeps LIMIT 260",
    )?;

    let mut rows = statement.query([])?;
    let mut count = 0;

    while let Some(row) = rows.next()? {
        let task: String = row.get(0).map_err(decode_error)?;
        let shard: i64 = row.get(1).map_err(decode_error)?;
        let generation_length: i64 = row.get(2).map_err(decode_error)?;
        let cursor: i64 = row.get(3).map_err(decode_error)?;
        let upper: i64 = row.get(4).map_err(decode_error)?;
        let started: i64 = row.get(5).map_err(decode_error)?;

        let valid_task = match task.as_str() {
            "gc_blobs" => (0..=255).contains(&shard),
            "expire_objects" | "abort_uploads" | "purge_receipts" => shard == -1,
            _ => false,
        };

        if !valid_task
            || generation_length != 16
            || cursor < 0
            || upper < cursor
            || EpochMillis::new(started).is_err()
        {
            return Err(MetadataError::Corrupt("invalid maintenance sweep"));
        }

        count += 1;
    }

    // The exact schema's composite primary key makes these 259 legal identities
    // unique; together they constitute the full fixed sweep inventory.
    if count != 259 {
        return Err(MetadataError::Corrupt("missing required maintenance sweep"));
    }

    Ok(())
}

pub(crate) fn validate_shared_runtime() -> Result<(), MetadataError> {
    if !rusqlite::ffi::mikrio_runtime_matches() {
        return Err(MetadataError::Incompatible(
            "incompatible shared SQLite runtime",
        ));
    }

    Ok(())
}

fn validate_runtime(connection: &Connection) -> Result<(), MetadataError> {
    // Pin the native source too: a system-library override must not silently
    // satisfy a check against only rusqlite's Rust package version.
    let source: String = connection.query_row("SELECT sqlite_source_id()", [], |row| row.get(0))?;

    if rusqlite::version_number() != 3_054_001
        || source
            != "2026-10-16 14:45:20 555e07ef33f69a0957d24d8e555b702464a131d6f1607f4c3c1e4551e8e6ca76"
    {
        return Err(MetadataError::Incompatible("unqualified SQLite runtime"));
    }

    for option in ["THREADSAFE=1", "ENABLE_API_ARMOR", "DEFAULT_FOREIGN_KEYS"] {
        let enabled: bool =
            connection.query_row("SELECT sqlite_compileoption_used(?1)", [option], |row| {
                row.get(0)
            })?;

        if !enabled {
            return Err(MetadataError::Incompatible("missing SQLite compile option"));
        }
    }

    for option in ["OMIT_WAL", "OMIT_FOREIGN_KEY", "OMIT_CHECK", "OMIT_TRIGGER"] {
        let enabled: bool =
            connection.query_row("SELECT sqlite_compileoption_used(?1)", [option], |row| {
                row.get(0)
            })?;

        if enabled {
            return Err(MetadataError::Incompatible(
                "unsupported SQLite compile option",
            ));
        }
    }

    Ok(())
}

fn discover_version(
    connection: &Connection,
    mode: MetadataOpen,
    registry: &migration::Registry,
) -> Result<(), MetadataError> {
    #[cfg(test)]
    if matches!(mode, MetadataOpen::SqliteFixture) {
        return Ok(());
    }

    if matches!(mode, MetadataOpen::Initialize) {
        // The initializer may configure only an empty database. Its existing
        // nonempty refusal remains separate from ordinary version discovery.
        return Ok(());
    }

    let can_initialize = !matches!(mode, MetadataOpen::Existing)
        && !connection
            .prepare("SELECT 1 FROM sqlite_schema LIMIT 1")?
            .exists([])?;

    if can_initialize {
        return Ok(());
    }

    // Only the stable bootstrap projection is understood before format selection.
    // In particular, a future format need not retain this version's WAL policy.
    registry.format(migration::version(connection)?)?;

    Ok(())
}

fn configure(connection: &Connection, mode: MetadataOpen) -> Result<(), MetadataError> {
    // Fixed SQL only. Cache bytecode, never result rows or authorization state.
    connection.set_prepared_statement_cache_capacity(STATEMENT_CACHE_CAPACITY);
    connection.busy_timeout(Duration::from_millis(5_000))?;
    // Extension loading remains disabled: its enabling feature/API is absent.
    // NO_MUTEX is safe because only one owning worker accesses a Connection.
    connection.set_db_config(DbConfig::SQLITE_DBCONFIG_DEFENSIVE, true)?;
    connection.set_db_config(DbConfig::SQLITE_DBCONFIG_DQS_DDL, false)?;
    connection.set_db_config(DbConfig::SQLITE_DBCONFIG_DQS_DML, false)?;

    connection.execute_batch(
        "PRAGMA synchronous = FULL;
         PRAGMA foreign_keys = ON;
         PRAGMA trusted_schema = OFF;
         PRAGMA locking_mode = NORMAL;
         PRAGMA read_uncommitted = OFF;
         PRAGMA ignore_check_constraints = OFF;
         PRAGMA wal_autocheckpoint = 1000;",
    )?;

    #[cfg(target_os = "macos")]
    connection.execute_batch("PRAGMA fullfsync = ON; PRAGMA checkpoint_fullfsync = ON;")?;

    if matches!(
        mode,
        MetadataOpen::Initialize | MetadataOpen::ResumeInitialization
    ) {
        let has_schema = connection
            .prepare("SELECT 1 FROM sqlite_schema LIMIT 1")?
            .exists([])?;

        if has_schema && matches!(mode, MetadataOpen::Initialize) {
            return Err(MetadataError::Incompatible(
                "initialization requires an empty database",
            ));
        }

        if !has_schema {
            connection.pragma_update(None, "journal_mode", "WAL")?;
        }
    }

    let journal: String = connection.pragma_query_value(None, "journal_mode", |row| row.get(0))?;
    let locking: String = connection.pragma_query_value(None, "locking_mode", |row| row.get(0))?;

    if journal != "wal" || locking != "normal" {
        return Err(MetadataError::Corrupt(
            "unexpected SQLite journal or locking mode",
        ));
    }

    for (setting, expected) in [
        ("synchronous", 2),
        ("foreign_keys", 1),
        ("trusted_schema", 0),
        ("read_uncommitted", 0),
        ("ignore_check_constraints", 0),
        ("busy_timeout", 5_000),
        ("wal_autocheckpoint", 1_000),
        ("auto_vacuum", 0),
    ] {
        let actual: i64 = connection.pragma_query_value(None, setting, |row| row.get(0))?;

        if actual != expected {
            return Err(MetadataError::Incompatible(
                "SQLite setting did not take effect",
            ));
        }
    }

    #[cfg(target_os = "macos")]
    for setting in ["fullfsync", "checkpoint_fullfsync"] {
        let actual: i64 = connection.pragma_query_value(None, setting, |row| row.get(0))?;

        if actual != 1 {
            return Err(MetadataError::Incompatible(
                "SQLite full synchronization unavailable",
            ));
        }
    }

    Ok(())
}

#[cfg(test)]
mod tests {
    mod bucket_configuration;
    mod buckets;
    mod corruption;
    mod maintenance;
    mod migration;
    mod multipart;
    mod objects;
    #[cfg(target_os = "macos")]
    mod policy_refusal;
    mod queries;
    mod runtime_build;
    mod sqlite_policy;

    use super::*;
    use crate::test_directory::TestDirectory;
    use rusqlite::ErrorCode;

    fn database(directory: &TestDirectory) -> Result<MetadataDatabase, Box<dyn std::error::Error>> {
        let path = directory.path().join("metadata.sqlite3");
        std::fs::File::create_new(&path)?;
        Ok(MetadataDatabase::open(&path, MetadataOpen::Initialize)?)
    }

    fn close(database: MetadataDatabase) -> Result<(), MetadataError> {
        database.close().map_err(|(_, error)| error)
    }

    #[test]
    fn future_format_is_refused_before_current_schema_or_journal_policy()
    -> Result<(), Box<dyn std::error::Error>> {
        let directory = TestDirectory::new()?;
        let path = directory.path().join("metadata.sqlite3");
        let connection = Connection::open(&path)?;

        connection.execute_batch(
            "CREATE TABLE store_metadata (singleton_id INTEGER PRIMARY KEY, store_id BLOB, format_version INTEGER) STRICT;
             INSERT INTO store_metadata (singleton_id, store_id, format_version) VALUES (1, zeroblob(16), 99);"
        )?;

        connection.close().map_err(|(_, error)| error)?;
        let before = std::fs::read(&path)?;

        for mode in [MetadataOpen::Existing, MetadataOpen::ResumeInitialization] {
            assert!(matches!(
                MetadataDatabase::open(&path, mode),
                Err(MetadataError::UnsupportedVersion(99))
            ));

            assert_eq!(std::fs::read(&path)?, before);
            assert!(!path.with_file_name("metadata.sqlite3-wal").exists());
        }

        directory.finish()?;
        Ok(())
    }

    #[test]
    fn region_envelope_is_enforced_before_bootstrap_and_on_stored_reads()
    -> Result<(), Box<dyn std::error::Error>> {
        let store_id = StoreId::from_bytes([3; 16]);
        let created_at = EpochMillis::new(0)?;

        for region in [
            String::new(),
            "A".into(),
            "a/region".into(),
            "-a".into(),
            "a-".into(),
            " a".into(),
            "a\n".into(),
            "é".into(),
            "a".repeat(64),
        ] {
            assert!(matches!(
                BootstrapValues::generate(store_id, region, created_at),
                Err(MetadataError::InvalidRegion)
            ));
        }

        let directory = TestDirectory::new()?;
        let mut database = database(&directory)?;

        database.initialize(&BootstrapValues::generate(
            store_id,
            "a".repeat(63),
            created_at,
        )?)?;

        assert_eq!(database.read_store()?.region, "a".repeat(63));

        database
            .connection
            .execute("UPDATE store_metadata SET region = ?1", ["a/region"])?;

        assert!(matches!(
            database.read_store(),
            Err(MetadataError::Corrupt("invalid store configuration"))
        ));

        close(database)?;
        directory.finish()?;
        Ok(())
    }

    #[cfg(any(unix, windows))]
    #[test]
    fn hard_linked_metadata_is_rejected_without_creating_sidecars()
    -> Result<(), Box<dyn std::error::Error>> {
        let directory = TestDirectory::new()?;
        let path = directory.path().join("original");
        std::fs::File::create_new(&path)?;
        let alias = directory.path().join("metadata.sqlite3");
        std::fs::hard_link(&path, &alias)?;

        #[cfg(unix)]
        let expected = "hard-linked metadata is unsupported";
        #[cfg(windows)]
        let expected = "metadata is not a single regular file";

        assert!(matches!(
            MetadataDatabase::open(&alias, MetadataOpen::Initialize),
            Err(MetadataError::Corrupt(message)) if message == expected
        ));

        assert_eq!(std::fs::metadata(&path)?.len(), 0);
        assert!(!directory.path().join("metadata.sqlite3-wal").exists());
        assert!(!directory.path().join("metadata.sqlite3-shm").exists());
        directory.finish()?;
        Ok(())
    }

    #[test]
    fn format_one_schema_matches_frozen_contract() -> Result<(), Box<dyn std::error::Error>> {
        use crate::metadata::migration::REGISTRY;
        use sha2::{Digest, Sha256};

        let contract: serde_json::Value =
            serde_json::from_str(include_str!("../../../tests/contract/schema.json"))?;

        let version = contract["format_version"]
            .as_i64()
            .ok_or("missing format version")?;

        REGISTRY.validate()?;
        let format = REGISTRY.format(version)?;
        let connection = Connection::open_in_memory()?;
        format.create_schema(&connection)?;
        let encoded = serde_json::to_vec(&schema(&connection)?)?;
        let actual: [u8; 32] = Sha256::digest(encoded).into();
        let expected = crate::values::decode_hex::<32>(
            contract["schema_sha256"]
                .as_str()
                .ok_or("missing schema digest")?
                .as_bytes(),
        )?;

        assert_eq!(
            actual, expected,
            "preserve the released schema and register a migration"
        );

        Ok(())
    }

    #[test]
    fn bootstrap_commits_complete_identity_and_refuses_reinitialization()
    -> Result<(), Box<dyn std::error::Error>> {
        let directory = TestDirectory::new()?;
        let mut first = database(&directory)?;
        let store_id = StoreId::parse("00112233445566778899aabbccddeeff")?;

        let values = BootstrapValues::generate(
            store_id,
            "us-east-1".into(),
            EpochMillis::new(1_700_000_000_123)?,
        )?;

        first.initialize(&values)?;
        let record = first.read_store()?;
        assert_eq!(record.store_id, store_id);
        assert_eq!(record.region, "us-east-1");
        assert_eq!(record.format_version, 1);
        assert_eq!(record.created_at.get(), 1_700_000_000_123);

        assert!(matches!(
            first.initialize(&values),
            Err(MetadataError::Sqlite(_))
        ));

        assert!(first.connection.is_autocommit());
        assert_eq!(first.read_store()?, record);
        close(first)?;

        let reopened = MetadataDatabase::open(
            &directory.path().join("metadata.sqlite3"),
            MetadataOpen::Existing,
        )?;

        assert_eq!(reopened.read_store()?, record);
        close(reopened)?;
        directory.finish()?;
        Ok(())
    }

    #[test]
    fn future_versions_missing_indexes_and_invalid_rows_are_refused()
    -> Result<(), Box<dyn std::error::Error>> {
        let directory = TestDirectory::new()?;
        let mut database = database(&directory)?;

        database.initialize(&BootstrapValues::generate(
            StoreId::from_bytes([1; 16]),
            "eu-west-1".into(),
            EpochMillis::new(0)?,
        )?)?;

        database
            .connection
            .execute_batch("CREATE TABLE sqliteaux (value TEXT);")?;

        assert!(matches!(
            database.read_store(),
            Err(MetadataError::Corrupt(
                "store schema differs from the supported format"
            ))
        ));

        database.connection.execute_batch("DROP TABLE sqliteaux;")?;

        database.connection.execute_batch(
            "PRAGMA ignore_check_constraints = ON;
             UPDATE maintenance_sweeps SET shard = 256 WHERE task = 'gc_blobs' AND shard = 255;
             PRAGMA ignore_check_constraints = OFF;",
        )?;

        assert!(matches!(
            database.read_store(),
            Err(MetadataError::Corrupt("invalid maintenance sweep"))
        ));

        database.connection.execute_batch(
            "UPDATE maintenance_sweeps SET shard = 255 WHERE task = 'gc_blobs' AND shard = 256;",
        )?;

        database
            .connection
            .execute_batch("UPDATE store_metadata SET format_version = 99;")?;

        assert!(matches!(
            database.read_store(),
            Err(MetadataError::UnsupportedVersion(99))
        ));

        database.connection.execute_batch(
            "UPDATE store_metadata SET format_version = 1, created_at_ms = 253402300800000;",
        )?;

        assert!(matches!(
            database.read_store(),
            Err(MetadataError::Corrupt("invalid store creation time"))
        ));

        database.connection.execute_batch(
            "UPDATE store_metadata SET created_at_ms = 0; DROP INDEX blobs_gc_idx;",
        )?;

        assert!(matches!(
            database.read_store(),
            Err(MetadataError::Corrupt(
                "store schema differs from the supported format"
            ))
        ));

        close(database)?;
        directory.finish()?;
        Ok(())
    }

    #[test]
    fn changed_bucket_nullability_is_refused_without_resetting_rows()
    -> Result<(), Box<dyn std::error::Error>> {
        let directory = TestDirectory::new()?;
        let mut database = database(&directory)?;
        let store_id = StoreId::from_bytes([1; 16]);

        database.initialize(&BootstrapValues::generate(
            store_id,
            "us-east-1".into(),
            EpochMillis::new(0)?,
        )?)?;

        let original: String = database.connection.query_row(
            "SELECT sql FROM sqlite_schema WHERE type = 'table' AND name = 'buckets'",
            [],
            |row| row.get(0),
        )?;

        let incompatible =
            original.replacen("TEXT COLLATE BINARY", "TEXT NOT NULL COLLATE BINARY", 1);

        assert_ne!(incompatible, original);

        let indexes = database
            .connection
            .prepare("SELECT sql FROM sqlite_schema WHERE type = 'index' AND tbl_name = 'buckets' AND sql IS NOT NULL")?
            .query_map([], |row| row.get::<_, String>(0))?
            .collect::<rusqlite::Result<Vec<_>>>()?;

        database.connection.execute_batch("DROP TABLE buckets;")?;
        database.connection.execute_batch(&incompatible)?;

        for index in indexes {
            database.connection.execute_batch(&index)?;
        }

        database.connection.execute(
            "INSERT INTO buckets (bucket_id,name,created_at_ms) VALUES (?1,'preserved',7)",
            [[2_u8; 16]],
        )?;

        close(database)?;

        let reopened = MetadataDatabase::open(
            &directory.path().join("metadata.sqlite3"),
            MetadataOpen::Existing,
        )?;

        assert!(matches!(
            reopened.read_store(),
            Err(MetadataError::Corrupt(
                "store schema differs from the supported format"
            ))
        ));

        let retained: (String, i64) = reopened.connection.query_row(
            "SELECT name,created_at_ms FROM buckets WHERE bucket_id=?1",
            [[2_u8; 16]],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )?;

        assert_eq!(retained, ("preserved".into(), 7));

        let retained_id: Vec<u8> =
            reopened
                .connection
                .query_row("SELECT store_id FROM store_metadata", [], |row| row.get(0))?;

        assert_eq!(retained_id, store_id.as_bytes());
        close(reopened)?;
        directory.finish()?;
        Ok(())
    }

    #[test]
    fn bootstrap_rolls_back_schema_and_rows_after_a_late_constraint_failure()
    -> Result<(), Box<dyn std::error::Error>> {
        let directory = TestDirectory::new()?;
        let mut database = database(&directory)?;

        let mut values = BootstrapValues::generate(
            StoreId::from_bytes([9; 16]),
            "us-east-1".into(),
            EpochMillis::new(0)?,
        )?;

        values.owner_account_id = "not-an-account".into();

        assert!(matches!(
            database.initialize(&values),
            Err(MetadataError::Sqlite(_))
        ));

        assert!(database.connection.is_autocommit());

        let count: i64 =
            database
                .connection
                .query_row("SELECT count(*) FROM sqlite_schema", [], |row| row.get(0))?;

        assert_eq!(count, 0);
        close(database)?;
        directory.finish()?;
        Ok(())
    }

    #[test]
    fn bundled_runtime_settings_and_extension_refusal_survive_reopen()
    -> Result<(), Box<dyn std::error::Error>> {
        let directory = TestDirectory::new()?;
        let first = database(&directory)?;
        assert_eq!(first.runtime_identity()?.0, "3.54.1");

        first
            .connection
            .execute_batch(include_str!("../schema.sql"))?;

        assert!(matches!(
            first.connection.query_row::<String, _, _>(
                "SELECT load_extension('not-a-library')",
                [],
                |row| row.get(0),
            ),
            Err(rusqlite::Error::SqliteFailure(code, _)) if code.code == ErrorCode::Unknown
        ));

        close(first)?;

        let reopened = MetadataDatabase::open(
            &directory.path().join("metadata.sqlite3"),
            MetadataOpen::SqliteFixture,
        )?;

        let integrity: String =
            reopened
                .connection
                .pragma_query_value(None, "quick_check", |row| row.get(0))?;

        assert_eq!(integrity, "ok");

        let table_count: i64 = reopened.connection.query_row(
            "SELECT count(*) FROM sqlite_schema WHERE type = 'table' AND name <> 'sqlite_sequence'",
            [],
            |row| row.get(0),
        )?;

        assert_eq!(table_count, 12);
        close(reopened)?;
        directory.finish()?;
        Ok(())
    }

    #[test]
    fn failed_commit_retains_provisional_rows_until_rollback()
    -> Result<(), Box<dyn std::error::Error>> {
        let directory = TestDirectory::new()?;
        let first = database(&directory)?;

        first.connection.execute_batch(
            "CREATE TABLE parent (id INTEGER PRIMARY KEY) STRICT; CREATE TABLE child (id INTEGER PRIMARY KEY, parent_id INTEGER REFERENCES parent(id) DEFERRABLE INITIALLY DEFERRED) STRICT;",
        )?;

        let second = MetadataDatabase::open(
            &directory.path().join("metadata.sqlite3"),
            MetadataOpen::SqliteFixture,
        )?;

        first
            .connection
            .execute_batch("BEGIN IMMEDIATE; INSERT INTO child (id, parent_id) VALUES (1, 9);")?;

        let error = first.connection.execute_batch("COMMIT").unwrap_err();

        assert_eq!(
            error.sqlite_error_code(),
            Some(ErrorCode::ConstraintViolation)
        );

        assert!(!first.connection.is_autocommit());

        let provisional: i64 =
            first
                .connection
                .query_row("SELECT count(*) FROM child", [], |row| row.get(0))?;

        let committed: i64 =
            second
                .connection
                .query_row("SELECT count(*) FROM child", [], |row| row.get(0))?;

        assert_eq!((provisional, committed), (1, 0));

        first.connection.execute_batch("ROLLBACK")?;
        assert!(first.connection.is_autocommit());

        let settled: i64 = first
            .connection
            .query_row("SELECT count(*) FROM child", [], |row| row.get(0))?;

        assert_eq!(settled, 0);
        close(second)?;
        close(first)?;
        directory.finish()?;
        Ok(())
    }

    #[test]
    fn wal_read_snapshot_is_stale_until_its_transaction_ends()
    -> Result<(), Box<dyn std::error::Error>> {
        let directory = TestDirectory::new()?;
        let first = database(&directory)?;

        first
            .connection
            .execute_batch("CREATE TABLE witness (generation INTEGER PRIMARY KEY) STRICT;")?;

        let second = MetadataDatabase::open(
            &directory.path().join("metadata.sqlite3"),
            MetadataOpen::SqliteFixture,
        )?;

        second.connection.execute_batch("BEGIN")?;

        let count = || {
            second
                .connection
                .query_row::<i64, _, _>("SELECT count(*) FROM witness", [], |row| row.get(0))
        };

        assert_eq!(count()?, 0);

        first.connection.execute_batch(
            "BEGIN IMMEDIATE; INSERT INTO witness (generation) VALUES (7); COMMIT;",
        )?;

        assert_eq!(count()?, 0);

        let error = second
            .connection
            .execute("INSERT INTO witness (generation) VALUES (9)", [])
            .unwrap_err();

        assert!(matches!(
            error,
            rusqlite::Error::SqliteFailure(cause, _)
                if cause.extended_code == rusqlite::ffi::SQLITE_BUSY_SNAPSHOT
        ));

        assert!(!second.connection.is_autocommit());
        assert_eq!(count()?, 0);
        second.connection.execute_batch("ROLLBACK")?;
        assert_eq!(count()?, 1);

        let generations = first
            .connection
            .prepare("SELECT generation FROM witness ORDER BY generation")?
            .query_map([], |row| row.get::<_, i64>(0))?
            .collect::<Result<Vec<_>, _>>()?;

        assert_eq!(generations, [7], "failed upgrade must not be replayed");

        second.connection.execute_batch(
            "BEGIN IMMEDIATE; INSERT INTO witness (generation) VALUES (9); COMMIT;",
        )?;

        assert_eq!(count()?, 2, "only the explicit fresh transaction may write");
        close(second)?;
        close(first)?;
        directory.finish()?;
        Ok(())
    }

    #[test]
    fn missing_nonwal_and_nonempty_initialization_are_refused()
    -> Result<(), Box<dyn std::error::Error>> {
        let directory = TestDirectory::new()?;
        let path = directory.path().join("metadata.sqlite3");

        assert!(matches!(
            MetadataDatabase::open(&path, MetadataOpen::Existing),
            Err(MetadataError::Io(error)) if error.kind() == std::io::ErrorKind::NotFound
        ));

        assert!(!path.exists());
        let native = Connection::open(&path)?;

        native.execute_batch(
            "CREATE TABLE unrelated (value TEXT); INSERT INTO unrelated (value) VALUES ('preserve');",
        )?;

        native.close().map_err(|(_, error)| error)?;

        assert!(matches!(
            MetadataDatabase::open(&path, MetadataOpen::Existing),
            Err(MetadataError::Corrupt("invalid store discovery schema"))
        ));

        assert!(matches!(
            MetadataDatabase::open(&path, MetadataOpen::Initialize),
            Err(MetadataError::Incompatible(
                "initialization requires an empty database"
            ))
        ));

        let native = Connection::open(&path)?;

        assert_eq!(
            native
                .query_row::<String, _, _>("SELECT value FROM unrelated", [], |row| row.get(0))?,
            "preserve"
        );

        assert_eq!(
            native.pragma_query_value::<String, _>(None, "journal_mode", |row| row.get(0))?,
            "delete"
        );

        native.close().map_err(|(_, error)| error)?;
        directory.finish()?;
        Ok(())
    }
}
