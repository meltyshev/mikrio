//! Compiled SQL-only format transitions under exclusive store use.

use std::path::Path;

use rusqlite::{Connection, DropBehavior, TransactionBehavior};

use super::{DDL, MetadataDatabase, MetadataError, decode_error, schema, validate_sweeps};

pub(crate) static REGISTRY: Registry = Registry {
    formats: &[Format {
        version: 1,
        schema: &[DDL],
    }],
    transitions: &[],
    #[cfg(test)]
    checkpoint: None,
};

pub(crate) struct Format {
    pub(crate) version: u32,
    pub(crate) schema: &'static [&'static str],
}

impl Format {
    pub(super) fn create_schema(&self, connection: &Connection) -> Result<(), MetadataError> {
        for sql in self.schema {
            connection.execute_batch(sql)?;
        }

        Ok(())
    }

    pub(super) fn validate_schema(&self, connection: &Connection) -> Result<(), MetadataError> {
        let reference = Connection::open_in_memory()?;
        self.create_schema(&reference)?;

        if schema(connection)? != schema(&reference)? {
            return Err(MetadataError::Corrupt(
                "store schema differs from the supported format",
            ));
        }

        Ok(())
    }
}

pub(crate) struct Transition {
    pub(crate) from: u32,
    pub(crate) to: u32,
    // A destructive transition must make and verify its independent backup here,
    // under exclusive store use, before the runner permits any SQL changes.
    pub(crate) prepare: fn(&Path) -> Result<(), MetadataError>,
    pub(crate) apply: fn(&Connection) -> Result<(), MetadataError>,
    pub(crate) rebuild: bool,
}

pub(crate) struct Registry {
    pub(crate) formats: &'static [Format],
    pub(crate) transitions: &'static [Transition],
    #[cfg(test)]
    pub(crate) checkpoint: Option<fn(Phase) -> Result<(), MetadataError>>,
}

#[cfg(test)]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Phase {
    Begun,
    Transformed,
    Stamped,
    Committed,
}

#[cfg(test)]
impl Registry {
    fn checkpoint(&self, phase: Phase) -> Result<(), MetadataError> {
        self.checkpoint.map_or(Ok(()), |hook| hook(phase))
    }
}

impl Registry {
    pub(super) fn validate(&self) -> Result<(), MetadataError> {
        if self.formats.is_empty() || self.transitions.len() + 1 != self.formats.len() {
            return Err(MetadataError::Incompatible(
                "invalid compiled migration registry",
            ));
        }

        if self
            .formats
            .iter()
            .any(|format| format.version == 0 || format.schema.is_empty())
        {
            return Err(MetadataError::Incompatible(
                "invalid compiled format description",
            ));
        }

        for (pair, transition) in self.formats.windows(2).zip(self.transitions) {
            if pair[0].version.checked_add(1) != Some(pair[1].version)
                || transition.from != pair[0].version
                || transition.to != pair[1].version
            {
                return Err(MetadataError::Incompatible(
                    "gapped or duplicate migration path",
                ));
            }
        }

        Ok(())
    }

    pub(crate) fn current(&self) -> &Format {
        // Access follows registry validation, including the nonempty check.
        &self.formats[self.formats.len() - 1]
    }

    pub(super) fn format(&self, version: i64) -> Result<&Format, MetadataError> {
        self.formats
            .iter()
            .find(|format| i64::from(format.version) == version)
            .ok_or(MetadataError::UnsupportedVersion(version))
    }
}

pub(super) fn version(connection: &Connection) -> Result<i64, MetadataError> {
    connection
        .query_row(
            "SELECT format_version FROM store_metadata WHERE singleton_id = 1",
            [],
            |row| row.get(0),
        )
        .map_err(decode_error)
}

// Deliberately has no Debug implementation: the token key is secret. A format
// transition changes the representation, never these persisted store values.
#[derive(PartialEq, Eq)]
struct Identity {
    store_id: Vec<u8>,
    owner_account_id: String,
    canonical_owner_id: String,
    region: String,
    shard_count: i64,
    shard_algorithm: String,
    continuation_token_key: Vec<u8>,
    created_at_ms: i64,
}

impl Identity {
    fn read(connection: &Connection) -> Result<Self, MetadataError> {
        connection
            .query_row(
                "SELECT store_id, owner_account_id, canonical_owner_id, region,
                        shard_count, shard_algorithm, continuation_token_key, created_at_ms
                 FROM store_metadata WHERE singleton_id = 1",
                [],
                |row| {
                    Ok(Self {
                        store_id: row.get(0)?,
                        owner_account_id: row.get(1)?,
                        canonical_owner_id: row.get(2)?,
                        region: row.get(3)?,
                        shard_count: row.get(4)?,
                        shard_algorithm: row.get(5)?,
                        continuation_token_key: row.get(6)?,
                        created_at_ms: row.get(7)?,
                    })
                },
            )
            .map_err(decode_error)
    }

    fn validate(&self, connection: &Connection) -> Result<(), MetadataError> {
        if *self != Self::read(connection)? {
            return Err(MetadataError::Corrupt("migration changed store identity"));
        }

        Ok(())
    }
}

impl MetadataDatabase {
    /// The caller owns exclusive store use for this complete attempt, including
    /// prerequisite backup, settlement and fresh committed validation.
    pub(crate) fn upgrade(&mut self, directory: &Path) -> Result<(), MetadataError> {
        let before = self.read_store()?;

        if before.format_version == self.registry.current().version {
            return Ok(());
        }

        let identity = Identity::read(&self.connection)?;

        let transitions = self
            .registry
            .transitions
            .iter()
            .filter(|step| step.from >= before.format_version)
            .collect::<Vec<_>>();

        for step in &transitions {
            (step.prepare)(directory)?;
        }

        self.connection.flush_prepared_statement_cache();
        let rebuild = transitions.iter().any(|step| step.rebuild);

        if rebuild {
            foreign_keys(&self.connection, false)?;
        }

        let result = self.apply_migrations(&transitions, &identity);
        self.connection.flush_prepared_statement_cache();

        // If settlement failed, neither changing settings nor ordinary work is
        // allowed. Close retains the exclusive guard until SQLite is released.
        if self.unusable {
            return result;
        }

        if rebuild {
            foreign_keys(&self.connection, true)?;
        }

        let after = self.read_store()?;
        identity.validate(&self.connection)?;

        match result {
            Ok(()) => Ok(()),
            // With exclusive use and a settled connection, the new committed
            // format is an authoritative witness of this complete transition.
            Err(_) if after.format_version == self.registry.current().version => Ok(()),
            Err(error) => Err(error),
        }
    }

    fn apply_migrations(
        &mut self,
        transitions: &[&Transition],
        identity: &Identity,
    ) -> Result<(), MetadataError> {
        let mut transaction = self
            .connection
            .transaction_with_behavior(TransactionBehavior::Immediate)?;

        let result = (|| {
            #[cfg(test)]
            self.registry.checkpoint(Phase::Begun)?;

            for step in transitions {
                if version(&transaction)? != i64::from(step.from) {
                    return Err(MetadataError::Corrupt("migration source version changed"));
                }

                (step.apply)(&transaction)?;

                #[cfg(test)]
                self.registry.checkpoint(Phase::Transformed)?;

                if transaction.is_autocommit() {
                    return Err(MetadataError::Incompatible(
                        "migration escaped its transaction",
                    ));
                }

                let changed = transaction.execute(
                    "UPDATE store_metadata SET format_version = ?1 WHERE singleton_id = 1 AND format_version = ?2",
                    (step.to, step.from),
                )?;

                if changed != 1 {
                    return Err(MetadataError::Corrupt("migration source version changed"));
                }

                #[cfg(test)]
                self.registry.checkpoint(Phase::Stamped)?;

                self.registry
                    .format(i64::from(step.to))?
                    .validate_schema(&transaction)?;

                if transaction
                    .prepare("PRAGMA foreign_key_check")?
                    .exists([])?
                {
                    return Err(MetadataError::Corrupt("migration foreign-key violation"));
                }
            }

            identity.validate(&transaction)?;
            validate_sweeps(&transaction)?;

            transaction.execute_batch("COMMIT")?;

            #[cfg(test)]
            self.registry.checkpoint(Phase::Committed)?;

            Ok(())
        })();

        if let Err(error) = result {
            if !transaction.is_autocommit()
                && let Err(rollback) = transaction.execute_batch("ROLLBACK")
            {
                transaction.set_drop_behavior(DropBehavior::Ignore);
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
}

fn foreign_keys(connection: &Connection, enabled: bool) -> Result<(), MetadataError> {
    connection.pragma_update(None, "foreign_keys", enabled)?;
    let actual: bool = connection.pragma_query_value(None, "foreign_keys", |row| row.get(0))?;

    if actual != enabled {
        return Err(MetadataError::Incompatible(
            "migration foreign-key policy did not take effect",
        ));
    }

    Ok(())
}
