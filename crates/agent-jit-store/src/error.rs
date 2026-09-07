//! Store failures.

use agent_jit_domain::canonical::CanonicalError;
use agent_jit_domain::envelope::EnvelopeError;

/// Why a store operation failed.
#[derive(Debug, thiserror::Error)]
pub enum StoreError {
    /// The database path is a symlink and could be repointed between runs.
    #[error("`{path}` is a symlink; the state database must be a real file")]
    PathSymlink {
        /// The rejected path.
        path: String,
    },
    /// The database path is not absolute, so it depends on the working directory.
    #[error("`{path}` must be an absolute path")]
    PathNotAbsolute {
        /// The rejected path.
        path: String,
    },
    /// The database file is readable or writable by someone other than its owner.
    #[error("`{path}` has mode {mode:o}; the state database must be 0600")]
    PermissionsTooOpen {
        /// The rejected path.
        path: String,
        /// The mode that was found.
        mode: u32,
    },
    /// The database was written by a newer build.
    #[error("database schema is v{found}; this build implements v{supported} and never downgrades")]
    SchemaFromTheFuture {
        /// Version stamped on the database.
        found: u32,
        /// Version this build implements.
        supported: u32,
    },
    /// A migration failed; the database is unchanged.
    #[error("migration {version} ({name}) failed and was rolled back: {reason}")]
    MigrationFailed {
        /// Version the migration would have produced.
        version: u32,
        /// Migration name.
        name: &'static str,
        /// Why it failed.
        reason: String,
    },
    /// The file is not a `SQLite` database this build can read.
    #[error("not a usable state database: {reason}")]
    NotADatabase {
        /// What was wrong.
        reason: String,
    },
    /// A record with that identifier already exists; records are immutable.
    #[error("{kind} `{id}` already exists; records are immutable")]
    AlreadyExists {
        /// Record kind.
        kind: &'static str,
        /// The identifier that collided.
        id: String,
    },
    /// SQL refused the write: a missing reference, or a uniqueness rule.
    #[error("the database refused the write: {reason}")]
    ConstraintViolated {
        /// Which rule was broken.
        reason: String,
    },
    /// A stored record could not be read back as its contract.
    #[error("stored record is not valid: {reason}")]
    RecordInvalid {
        /// Why it was rejected.
        reason: String,
    },
    /// An annotation revision was not the next consecutive revision.
    #[error("annotation revision {found} does not follow {current}")]
    AnnotationRevisionInvalid {
        /// Current revision, or zero when no annotation exists.
        current: u32,
        /// Revision supplied by the caller.
        found: u32,
    },
    /// The current annotation revision cannot be advanced.
    #[error("annotation revision cannot advance beyond u32::MAX")]
    AnnotationRevisionOverflow,
    /// Retention age arithmetic exceeded the timestamp range.
    #[error("retention age arithmetic overflowed")]
    RetentionAgeOverflow,
    /// Retention age must not be negative.
    #[error("retention maximum age must be nonnegative")]
    RetentionAgeInvalid,
    /// Logical evidence bytes could not be computed safely.
    #[error("retention logical size is unavailable: {reason}")]
    RetentionSizeUnavailable {
        /// Invalid or overflowing accounting detail.
        reason: String,
    },
    /// Retention limits cannot be met without deleting protected evidence.
    #[error("retention limits cannot be met because remaining sessions are protected")]
    RetentionLimitUnmetProtected,
    /// `SQLite` reported an error the store does not classify further.
    #[error("sqlite: {source}")]
    Sqlite {
        /// The underlying error.
        #[from]
        source: rusqlite::Error,
    },
    /// The filesystem refused an operation.
    #[error("`{path}`: {reason}")]
    Io {
        /// The path involved.
        path: String,
        /// Reason reported by the operating system.
        reason: String,
    },
}

impl StoreError {
    /// Stable machine-readable code.
    #[must_use]
    pub const fn code(&self) -> &'static str {
        match self {
            Self::PathSymlink { .. } => "store_path_symlink",
            Self::PathNotAbsolute { .. } => "store_path_not_absolute",
            Self::PermissionsTooOpen { .. } => "store_permissions_too_open",
            Self::SchemaFromTheFuture { .. } => "store_schema_from_the_future",
            Self::MigrationFailed { .. } => "store_migration_failed",
            Self::NotADatabase { .. } => "store_not_a_database",
            Self::AlreadyExists { .. } => "store_already_exists",
            Self::ConstraintViolated { .. } => "store_constraint_violated",
            Self::RecordInvalid { .. } => "store_record_invalid",
            Self::AnnotationRevisionInvalid { .. } => "outcome_annotation_revision_invalid",
            Self::AnnotationRevisionOverflow => "outcome_annotation_revision_overflow",
            Self::RetentionAgeOverflow => "retention_age_overflow",
            Self::RetentionAgeInvalid => "retention_age_invalid",
            Self::RetentionSizeUnavailable { .. } => "retention_size_unavailable",
            Self::RetentionLimitUnmetProtected => "retention_limit_unmet_protected",
            Self::Sqlite { .. } => "store_sqlite",
            Self::Io { .. } => "store_io",
        }
    }
}

impl From<CanonicalError> for StoreError {
    fn from(error: CanonicalError) -> Self {
        Self::RecordInvalid {
            reason: format!("{}: {error}", error.code()),
        }
    }
}

impl From<EnvelopeError> for StoreError {
    fn from(error: EnvelopeError) -> Self {
        Self::RecordInvalid {
            reason: format!("{}: {error}", error.code()),
        }
    }
}
