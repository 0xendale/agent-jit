//! Forward-only schema migrations.
//!
//! Migrations are numbered, embedded in the binary, and applied in one transaction each. There is
//! no downgrade path: a database written by a newer build is refused rather than reinterpreted,
//! because reinterpreting it would silently rewrite the evidence a gate decision rests on.

use rusqlite::{Connection, Transaction};

use crate::error::StoreError;

/// One numbered migration.
pub struct Migration {
    /// Schema version this migration produces.
    pub version: u32,
    /// Human-readable name, used in reports.
    pub name: &'static str,
    /// SQL applied to reach `version`.
    pub sql: &'static str,
}

/// Every migration, in order. Appending is the only legal change.
pub const MIGRATIONS: &[Migration] = &[Migration {
    version: 1,
    name: "initial",
    sql: include_str!("../migrations/0001_initial.sql"),
}];

/// Schema version this build implements.
pub const CURRENT_SCHEMA_VERSION: u32 = 1;

/// What one call to migrate did.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct MigrationReport {
    /// Version before migrating.
    pub from_version: u32,
    /// Version after migrating.
    pub to_version: u32,
    /// How many migrations ran.
    pub applied: u32,
}

/// Reads the schema version stamped on the database.
///
/// # Errors
///
/// Returns [`StoreError`] when the pragma cannot be read.
pub fn schema_version(connection: &Connection) -> Result<u32, StoreError> {
    let version: i64 = connection.query_row("PRAGMA user_version", [], |row| row.get(0))?;
    u32::try_from(version).map_err(|_| StoreError::NotADatabase {
        reason: format!("negative schema version {version}"),
    })
}

/// Applies every migration the database has not seen yet.
///
/// Each migration runs inside its own transaction together with the version stamp, so a failure
/// leaves the database exactly as it was: either a migration and its stamp both land, or neither
/// does.
///
/// # Errors
///
/// Returns [`StoreError::MigrationFailed`] when a migration fails, leaving the previous schema and
/// all data intact.
pub fn migrate(connection: &mut Connection) -> Result<MigrationReport, StoreError> {
    let from_version = schema_version(connection)?;
    let mut applied = 0_u32;

    for migration in MIGRATIONS {
        if migration.version <= from_version {
            continue;
        }

        let transaction = connection.transaction()?;
        apply(&transaction, migration).map_err(|error| StoreError::MigrationFailed {
            version: migration.version,
            name: migration.name,
            reason: error.to_string(),
        })?;
        transaction.commit()?;
        applied += 1;
    }

    Ok(MigrationReport {
        from_version,
        to_version: schema_version(connection)?,
        applied,
    })
}

/// Applies one migration and stamps its version inside the same transaction.
fn apply(transaction: &Transaction<'_>, migration: &Migration) -> Result<(), StoreError> {
    transaction.execute_batch(migration.sql)?;
    // `PRAGMA user_version` does not accept a bound parameter.
    transaction.execute_batch(&format!("PRAGMA user_version = {}", migration.version))?;
    Ok(())
}
