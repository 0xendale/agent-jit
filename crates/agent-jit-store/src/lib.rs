//! Private per-user persistence for agent-jit.
//!
//! The store is deliberately narrow. Callers hand it typed [`Envelope`] records and get typed
//! records back; no SQL crosses this boundary, so the CLI and the runtime cannot invent a query
//! that bypasses a constraint. Records are immutable — writing the same identifier twice is an
//! error, not an update — and the only mutable table is the pointer that says which capability
//! version is currently enabled.
//!
//! Everything here assumes the path has already passed the private-path checks: the database lives
//! outside every observed repository, is a real file rather than a symlink, and is `0600`.

mod error;
mod migrate;

use std::fs::Permissions;
use std::os::unix::fs::PermissionsExt as _;
use std::path::{Path, PathBuf};

use agent_jit_domain::benchmark::BenchmarkRecord;
use agent_jit_domain::candidate::{CandidateContract, Group};
use agent_jit_domain::canonical::canonical_string_of;
use agent_jit_domain::capability::{CapabilityVersion, Invocation, ReplayResult, Workflow};
use agent_jit_domain::envelope::{Envelope, Record};
use agent_jit_domain::ids::{
    BenchmarkId, CandidateId, CapabilityId, GroupId, Id, IdKind, InvocationId, OutcomeId,
    RepositoryId, SessionId, TrajectoryId, WorkflowId,
};
use agent_jit_domain::outcome::Outcome;
use agent_jit_domain::trace::{Event, Repository, Session, Trajectory};
use rusqlite::{Connection, OpenFlags, params};

pub use error::StoreError;
pub use migrate::{CURRENT_SCHEMA_VERSION, MigrationReport};

/// Mode every state file must have.
const PRIVATE_FILE_MODE: u32 = 0o600;

/// How long a writer waits for a competing writer before giving up.
const BUSY_TIMEOUT_MS: u32 = 5_000;

/// A wall clock, behind a trait so tests can pin it.
pub trait Clock: Send + Sync {
    /// Milliseconds since the Unix epoch.
    fn now_unix_ms(&self) -> i64;
}

/// The real clock.
#[derive(Debug, Clone, Copy, Default)]
pub struct SystemClock;

impl Clock for SystemClock {
    fn now_unix_ms(&self) -> i64 {
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map_or(0, |elapsed| {
                i64::try_from(elapsed.as_millis()).unwrap_or(i64::MAX)
            })
    }
}

/// A clock pinned to one instant, for deterministic tests and fixtures.
#[derive(Debug, Clone, Copy)]
pub struct FixedClock(i64);

impl FixedClock {
    /// Builds a clock that always reports `unix_ms`.
    #[must_use]
    pub const fn at(unix_ms: i64) -> Self {
        Self(unix_ms)
    }
}

impl Clock for FixedClock {
    fn now_unix_ms(&self) -> i64 {
        self.0
    }
}

/// A validated path to the state database.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StorePath(PathBuf);

impl StorePath {
    /// Validates `path` as a place the state database may live.
    ///
    /// # Errors
    ///
    /// Returns [`StoreError`] when the path is relative or is a symlink.
    pub fn new(path: &Path) -> Result<Self, StoreError> {
        if !path.is_absolute() {
            return Err(StoreError::PathNotAbsolute {
                path: path.display().to_string(),
            });
        }
        if let Ok(metadata) = std::fs::symlink_metadata(path)
            && metadata.file_type().is_symlink()
        {
            return Err(StoreError::PathSymlink {
                path: path.display().to_string(),
            });
        }
        Ok(Self(path.to_path_buf()))
    }

    /// Returns the path.
    #[must_use]
    pub fn as_path(&self) -> &Path {
        &self.0
    }
}

/// Health of the database.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HealthReport {
    /// Schema version stamped on the database.
    pub schema_version: u32,
    /// Result of `PRAGMA integrity_check`.
    pub integrity: String,
    /// Number of rows `PRAGMA foreign_key_check` reported.
    pub foreign_key_violations: u32,
    /// Whether every check passed.
    pub healthy: bool,
}

/// The private state database.
pub struct Store {
    connection: Connection,
    clock: Box<dyn Clock>,
}

impl std::fmt::Debug for Store {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Store").finish_non_exhaustive()
    }
}

impl Store {
    /// Opens (or creates) the database at `path`.
    ///
    /// # Errors
    ///
    /// Returns [`StoreError`] when the file is readable by others, was written by a newer build,
    /// or is not a usable database.
    pub fn open(path: &StorePath) -> Result<Self, StoreError> {
        let existed = path.as_path().exists();
        if existed {
            enforce_private_mode(path.as_path())?;
        }

        let connection = Connection::open_with_flags(
            path.as_path(),
            OpenFlags::SQLITE_OPEN_READ_WRITE
                | OpenFlags::SQLITE_OPEN_CREATE
                | OpenFlags::SQLITE_OPEN_NO_MUTEX,
        )?;

        if !existed {
            secure_file(path.as_path())?;
        }

        connection.pragma_update(None, "journal_mode", "WAL")?;
        connection.pragma_update(None, "foreign_keys", "ON")?;
        connection.pragma_update(None, "synchronous", "FULL")?;
        connection.busy_timeout(std::time::Duration::from_millis(u64::from(BUSY_TIMEOUT_MS)))?;

        let store = Self {
            connection,
            clock: Box::new(SystemClock),
        };

        let version = store.schema_version()?;
        if version > CURRENT_SCHEMA_VERSION {
            return Err(StoreError::SchemaFromTheFuture {
                found: version,
                supported: CURRENT_SCHEMA_VERSION,
            });
        }

        // The WAL and shared-memory sidecars appear on first write; secure them as they show up.
        Self::secure_sidecars(path.as_path());
        Ok(store)
    }

    /// Replaces the clock, for deterministic tests and fixtures.
    #[must_use]
    pub fn with_clock(mut self, clock: impl Clock + 'static) -> Self {
        self.clock = Box::new(clock);
        self
    }

    /// Applies pending migrations.
    ///
    /// # Errors
    ///
    /// Returns [`StoreError::MigrationFailed`] when a migration fails; the database is unchanged.
    pub fn migrate(&mut self) -> Result<MigrationReport, StoreError> {
        let report = migrate::migrate(&mut self.connection)?;
        Ok(report)
    }

    /// Returns the schema version stamped on the database.
    ///
    /// # Errors
    ///
    /// Returns [`StoreError`] when the pragma cannot be read.
    pub fn schema_version(&self) -> Result<u32, StoreError> {
        migrate::schema_version(&self.connection)
    }

    /// Returns the configured busy timeout in milliseconds.
    #[must_use]
    pub const fn busy_timeout_ms(&self) -> u32 {
        BUSY_TIMEOUT_MS
    }

    /// Runs the integrity and reference checks.
    ///
    /// # Errors
    ///
    /// Returns [`StoreError`] when a check cannot be run.
    pub fn check(&self) -> Result<HealthReport, StoreError> {
        let integrity: String = self
            .connection
            .query_row("PRAGMA integrity_check", [], |row| row.get(0))?;

        let mut statement = self.connection.prepare("PRAGMA foreign_key_check")?;
        let violations = statement.query_map([], |_| Ok(()))?.count();
        let foreign_key_violations = u32::try_from(violations).unwrap_or(u32::MAX);

        let schema_version = self.schema_version()?;
        Ok(HealthReport {
            healthy: integrity == "ok"
                && foreign_key_violations == 0
                && schema_version == CURRENT_SCHEMA_VERSION,
            integrity,
            foreign_key_violations,
            schema_version,
        })
    }

    /// Stamps a schema version. Test-only: the product never writes a version by hand.
    ///
    /// # Errors
    ///
    /// Returns [`StoreError`] when the pragma cannot be written.
    pub fn force_schema_version_for_test(&mut self, version: u32) -> Result<(), StoreError> {
        self.connection
            .execute_batch(&format!("PRAGMA user_version = {version}"))?;
        Ok(())
    }

    /// Runs a migration that is guaranteed to fail, to prove rollback. Test-only.
    ///
    /// # Errors
    ///
    /// Always returns [`StoreError::MigrationFailed`].
    pub fn apply_failing_migration_for_test(&mut self) -> Result<(), StoreError> {
        let transaction = self.connection.transaction()?;
        let attempt = transaction
            .execute_batch(
                "CREATE TABLE will_not_survive (id TEXT PRIMARY KEY) STRICT;\n\
                 PRAGMA user_version = 999;\n\
                 INSERT INTO will_not_survive (id) VALUES ('a'), ('a');",
            )
            .map_err(StoreError::from);
        // Dropping the transaction without committing rolls everything back.
        drop(transaction);

        match attempt {
            Ok(()) => Ok(()),
            Err(error) => Err(StoreError::MigrationFailed {
                version: 999,
                name: "deliberate-failure",
                reason: error.to_string(),
            }),
        }
    }

    /// Records a repository.
    ///
    /// # Errors
    ///
    /// Returns [`StoreError`] when the record already exists or SQL refuses the write.
    pub fn put_repository(&mut self, record: &Envelope<Repository>) -> Result<(), StoreError> {
        let (json, digest) = encode(record)?;
        let body = record.body();
        self.insert(
            "repository",
            &record.id().to_string(),
            "INSERT INTO repositories (repo_id, git_common_dir, identity_digest, label, record_json, digest, written_at) \
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)",
            params![
                record.id().to_string(),
                body.git_common_dir,
                body.identity_digest.to_string(),
                body.label,
                json,
                digest,
                self.clock.now_unix_ms(),
            ],
        )
    }

    /// Loads a repository.
    ///
    /// # Errors
    ///
    /// Returns [`StoreError`] when the stored record cannot be read back as its contract.
    pub fn get_repository(
        &self,
        id: &RepositoryId,
    ) -> Result<Option<Envelope<Repository>>, StoreError> {
        self.get_one(
            "SELECT record_json FROM repositories WHERE repo_id = ?1",
            id,
        )
    }

    /// Records a session.
    ///
    /// # Errors
    ///
    /// Returns [`StoreError`] when the record exists or its repository does not.
    pub fn put_session(&mut self, record: &Envelope<Session>) -> Result<(), StoreError> {
        let (json, digest) = encode(record)?;
        let body = record.body();
        self.insert(
            "session",
            &record.id().to_string(),
            "INSERT INTO sessions (session_id, repo_id, worktree_root, head_commit, runtime, runtime_version, model_id, started_at_unix_ms, ended_at_unix_ms, record_json, digest, written_at) \
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12)",
            params![
                record.id().to_string(),
                body.repository_id.to_string(),
                body.worktree_root,
                body.head_commit,
                format!("{:?}", body.runtime),
                body.runtime_version,
                body.model_id,
                body.started_at_unix_ms,
                body.ended_at_unix_ms,
                json,
                digest,
                self.clock.now_unix_ms(),
            ],
        )
    }

    /// Loads a session.
    ///
    /// # Errors
    ///
    /// Returns [`StoreError`] when the stored record cannot be read back as its contract.
    pub fn get_session(&self, id: &SessionId) -> Result<Option<Envelope<Session>>, StoreError> {
        self.get_one("SELECT record_json FROM sessions WHERE session_id = ?1", id)
    }

    /// Lists a repository's sessions, ordered by identifier.
    ///
    /// # Errors
    ///
    /// Returns [`StoreError`] when a stored record cannot be read back as its contract.
    pub fn list_sessions(
        &self,
        repository_id: &RepositoryId,
    ) -> Result<Vec<Envelope<Session>>, StoreError> {
        self.list(
            "SELECT record_json FROM sessions WHERE repo_id = ?1 ORDER BY session_id ASC",
            &repository_id.to_string(),
        )
    }

    /// Records an event.
    ///
    /// # Errors
    ///
    /// Returns [`StoreError`] when the record exists, its session does not, or the sequence repeats.
    pub fn put_event(&mut self, record: &Envelope<Event>) -> Result<(), StoreError> {
        let (json, digest) = encode(record)?;
        let body = record.body();
        self.insert(
            "event",
            &record.id().to_string(),
            "INSERT INTO events (event_id, session_id, sequence, kind, tool_name, exit_code, payload_digest, payload_bytes, payload_truncated, record_json, digest, written_at) \
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12)",
            params![
                record.id().to_string(),
                body.session_id.to_string(),
                body.sequence,
                format!("{:?}", body.kind),
                body.tool_name,
                body.exit_code,
                body.payload_digest.map(|digest| digest.to_string()),
                body.payload_bytes,
                i64::from(body.payload_truncated),
                json,
                digest,
                self.clock.now_unix_ms(),
            ],
        )
    }

    /// Lists a session's events, ordered by sequence.
    ///
    /// # Errors
    ///
    /// Returns [`StoreError`] when a stored record cannot be read back as its contract.
    pub fn list_events(&self, session_id: &SessionId) -> Result<Vec<Envelope<Event>>, StoreError> {
        self.list(
            "SELECT record_json FROM events WHERE session_id = ?1 ORDER BY sequence ASC",
            &session_id.to_string(),
        )
    }

    /// Records a trajectory.
    ///
    /// # Errors
    ///
    /// Returns [`StoreError`] when the record exists or its session or repository does not.
    pub fn put_trajectory(&mut self, record: &Envelope<Trajectory>) -> Result<(), StoreError> {
        let (json, digest) = encode(record)?;
        let body = record.body();
        self.insert(
            "trajectory",
            &record.id().to_string(),
            "INSERT INTO trajectories (trajectory_id, session_id, repo_id, head_commit, intent, record_json, digest, written_at) \
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)",
            params![
                record.id().to_string(),
                body.session_id.to_string(),
                body.repository_id.to_string(),
                body.head_commit,
                body.intent,
                json,
                digest,
                self.clock.now_unix_ms(),
            ],
        )
    }

    /// Loads a trajectory.
    ///
    /// # Errors
    ///
    /// Returns [`StoreError`] when the stored record cannot be read back as its contract.
    pub fn get_trajectory(
        &self,
        id: &TrajectoryId,
    ) -> Result<Option<Envelope<Trajectory>>, StoreError> {
        self.get_one(
            "SELECT record_json FROM trajectories WHERE trajectory_id = ?1",
            id,
        )
    }

    /// Lists a repository's trajectories, ordered by identifier.
    ///
    /// # Errors
    ///
    /// Returns [`StoreError`] when a stored record cannot be read back as its contract.
    pub fn list_trajectories(
        &self,
        repository_id: &RepositoryId,
    ) -> Result<Vec<Envelope<Trajectory>>, StoreError> {
        self.list(
            "SELECT record_json FROM trajectories WHERE repo_id = ?1 ORDER BY trajectory_id ASC",
            &repository_id.to_string(),
        )
    }

    /// Counts a repository's trajectories.
    ///
    /// # Errors
    ///
    /// Returns [`StoreError`] when the query fails.
    pub fn count_trajectories(&self, repository_id: &RepositoryId) -> Result<u32, StoreError> {
        let count: i64 = self.connection.query_row(
            "SELECT COUNT(*) FROM trajectories WHERE repo_id = ?1",
            params![repository_id.to_string()],
            |row| row.get(0),
        )?;
        Ok(u32::try_from(count).unwrap_or(u32::MAX))
    }

    /// Records an outcome.
    ///
    /// # Errors
    ///
    /// Returns [`StoreError`] when the record exists or its trajectory does not.
    pub fn put_outcome(&mut self, record: &Envelope<Outcome>) -> Result<(), StoreError> {
        let (json, digest) = encode(record)?;
        let body = record.body();
        self.insert(
            "outcome",
            &record.id().to_string(),
            "INSERT INTO outcomes (outcome_id, trajectory_id, status, confirmed_by_human, record_json, digest, written_at) \
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)",
            params![
                record.id().to_string(),
                body.trajectory_id.to_string(),
                format!("{:?}", body.status),
                i64::from(body.confirmed_by_human),
                json,
                digest,
                self.clock.now_unix_ms(),
            ],
        )
    }

    /// Loads an outcome.
    ///
    /// # Errors
    ///
    /// Returns [`StoreError`] when the stored record cannot be read back as its contract.
    pub fn get_outcome(&self, id: &OutcomeId) -> Result<Option<Envelope<Outcome>>, StoreError> {
        self.get_one("SELECT record_json FROM outcomes WHERE outcome_id = ?1", id)
    }

    /// Records a group and its membership.
    ///
    /// # Errors
    ///
    /// Returns [`StoreError`] when the group exists or a member trajectory does not.
    pub fn put_group(&mut self, record: &Envelope<Group>) -> Result<(), StoreError> {
        let (json, digest) = encode(record)?;
        let body = record.body();
        let group_id = record.id().to_string();
        let written_at = self.clock.now_unix_ms();

        let transaction = self.connection.transaction()?;
        let inserted = transaction.execute(
            "INSERT INTO groups (group_id, repo_id, intent_label, manifest_digest, record_json, digest, written_at) \
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)",
            params![
                group_id,
                body.repository_id.to_string(),
                body.intent_label,
                body.manifest_digest.to_string(),
                json,
                digest,
                written_at,
            ],
        );
        classify_write(inserted, "group", &group_id)?;

        for member in &body.members {
            let member_insert = transaction.execute(
                "INSERT INTO group_members (group_id, trajectory_id) VALUES (?1, ?2)",
                params![group_id, member.to_string()],
            );
            classify_write(member_insert, "group_member", &member.to_string())?;
        }

        transaction.commit()?;
        Ok(())
    }

    /// Loads a group.
    ///
    /// # Errors
    ///
    /// Returns [`StoreError`] when the stored record cannot be read back as its contract.
    pub fn get_group(&self, id: &GroupId) -> Result<Option<Envelope<Group>>, StoreError> {
        self.get_one("SELECT record_json FROM groups WHERE group_id = ?1", id)
    }

    /// Records a candidate contract version.
    ///
    /// # Errors
    ///
    /// Returns [`StoreError`] when the record exists or its group does not.
    pub fn put_candidate(
        &mut self,
        record: &Envelope<CandidateContract>,
    ) -> Result<(), StoreError> {
        let (json, digest) = encode(record)?;
        let body = record.body();
        self.insert(
            "candidate",
            &record.id().to_string(),
            "INSERT INTO candidate_versions (candidate_id, group_id, name, state, break_even_uses, record_json, digest, written_at) \
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)",
            params![
                record.id().to_string(),
                body.group_id.to_string(),
                body.name,
                body.state.as_str(),
                body.projected.break_even_uses,
                json,
                digest,
                self.clock.now_unix_ms(),
            ],
        )
    }

    /// Loads a candidate contract version.
    ///
    /// # Errors
    ///
    /// Returns [`StoreError`] when the stored record cannot be read back as its contract.
    pub fn get_candidate(
        &self,
        id: &CandidateId,
    ) -> Result<Option<Envelope<CandidateContract>>, StoreError> {
        self.get_one(
            "SELECT record_json FROM candidate_versions WHERE candidate_id = ?1",
            id,
        )
    }

    /// Records a compiled workflow.
    ///
    /// # Errors
    ///
    /// Returns [`StoreError`] when the record exists or its candidate does not.
    pub fn put_workflow(&mut self, record: &Envelope<Workflow>) -> Result<(), StoreError> {
        let (json, digest) = encode(record)?;
        let body = record.body();
        self.insert(
            "workflow",
            &record.id().to_string(),
            "INSERT INTO workflows (workflow_id, candidate_id, ir_version, ir_digest, node_count, record_json, digest, written_at) \
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)",
            params![
                record.id().to_string(),
                body.candidate_id.to_string(),
                body.ir_version,
                body.ir_digest.to_string(),
                body.node_count,
                json,
                digest,
                self.clock.now_unix_ms(),
            ],
        )
    }

    /// Loads a compiled workflow.
    ///
    /// # Errors
    ///
    /// Returns [`StoreError`] when the stored record cannot be read back as its contract.
    pub fn get_workflow(&self, id: &WorkflowId) -> Result<Option<Envelope<Workflow>>, StoreError> {
        self.get_one(
            "SELECT record_json FROM workflows WHERE workflow_id = ?1",
            id,
        )
    }

    /// Records a replay result.
    ///
    /// # Errors
    ///
    /// Returns [`StoreError`] when the record exists or its workflow or fixture does not.
    pub fn put_replay(&mut self, record: &Envelope<ReplayResult>) -> Result<(), StoreError> {
        let (json, digest) = encode(record)?;
        let body = record.body();
        self.insert(
            "replay",
            &record.id().to_string(),
            "INSERT INTO replays (replay_id, workflow_id, fixture_trajectory_id, verdict, deterministic, record_json, digest, written_at) \
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)",
            params![
                record.id().to_string(),
                body.workflow_id.to_string(),
                body.fixture_trajectory_id.to_string(),
                format!("{:?}", body.verdict),
                i64::from(body.deterministic),
                json,
                digest,
                self.clock.now_unix_ms(),
            ],
        )
    }

    /// Lists a workflow's replays, ordered by identifier.
    ///
    /// # Errors
    ///
    /// Returns [`StoreError`] when a stored record cannot be read back as its contract.
    pub fn list_replays(
        &self,
        workflow_id: &WorkflowId,
    ) -> Result<Vec<Envelope<ReplayResult>>, StoreError> {
        self.list(
            "SELECT record_json FROM replays WHERE workflow_id = ?1 ORDER BY replay_id ASC",
            &workflow_id.to_string(),
        )
    }

    /// Records an immutable capability version.
    ///
    /// # Errors
    ///
    /// Returns [`StoreError`] when the record exists or its workflow does not.
    pub fn put_capability(
        &mut self,
        record: &Envelope<CapabilityVersion>,
    ) -> Result<(), StoreError> {
        let (json, digest) = encode(record)?;
        let body = record.body();
        self.insert(
            "capability",
            &record.id().to_string(),
            "INSERT INTO capability_versions (capability_id, workflow_id, name, version, state, fingerprint_digest, record_json, digest, written_at) \
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9)",
            params![
                record.id().to_string(),
                body.workflow_id.to_string(),
                body.name,
                body.version,
                body.state.as_str(),
                body.fingerprint_digest.to_string(),
                json,
                digest,
                self.clock.now_unix_ms(),
            ],
        )
    }

    /// Loads a capability version.
    ///
    /// # Errors
    ///
    /// Returns [`StoreError`] when the stored record cannot be read back as its contract.
    pub fn get_capability(
        &self,
        id: &CapabilityId,
    ) -> Result<Option<Envelope<CapabilityVersion>>, StoreError> {
        self.get_one(
            "SELECT record_json FROM capability_versions WHERE capability_id = ?1",
            id,
        )
    }

    /// Points a capability name at one immutable version.
    ///
    /// This is the only mutable state in the store: enabling a new version replaces the pointer,
    /// while every version it ever pointed at stays exactly as it was written.
    ///
    /// # Errors
    ///
    /// Returns [`StoreError`] when the capability version does not exist.
    pub fn set_active_capability(
        &mut self,
        name: &str,
        capability_id: &CapabilityId,
        approved_by: Option<&str>,
    ) -> Result<(), StoreError> {
        let result = self.connection.execute(
            "INSERT INTO capability_active (name, capability_id, approved_by, approved_at) \
             VALUES (?1, ?2, ?3, ?4) \
             ON CONFLICT(name) DO UPDATE SET capability_id = excluded.capability_id, \
             approved_by = excluded.approved_by, approved_at = excluded.approved_at",
            params![
                name,
                capability_id.to_string(),
                approved_by,
                self.clock.now_unix_ms()
            ],
        );
        classify_write(result, "capability_active", name)
    }

    /// Returns the capability version currently enabled under `name`.
    ///
    /// # Errors
    ///
    /// Returns [`StoreError`] when the stored record cannot be read back as its contract.
    pub fn active_capability(
        &self,
        name: &str,
    ) -> Result<Option<Envelope<CapabilityVersion>>, StoreError> {
        let json: Option<String> = self
            .connection
            .query_row(
                "SELECT v.record_json FROM capability_active a \
                 JOIN capability_versions v ON v.capability_id = a.capability_id \
                 WHERE a.name = ?1",
                params![name],
                |row| row.get(0),
            )
            .map_or_else(swallow_missing, |value: String| Ok(Some(value)))?;

        json.map(|text| decode::<CapabilityVersion>(&text))
            .transpose()
    }

    /// Records one runtime invocation.
    ///
    /// # Errors
    ///
    /// Returns [`StoreError`] when the record exists or its capability does not.
    pub fn put_invocation(&mut self, record: &Envelope<Invocation>) -> Result<(), StoreError> {
        let (json, digest) = encode(record)?;
        let body = record.body();
        let result = match &body.response {
            agent_jit_domain::capability::RuntimeResponse::Executed { .. } => "executed",
            agent_jit_domain::capability::RuntimeResponse::NotFound => "not_found",
            agent_jit_domain::capability::RuntimeResponse::FallbackRequired { .. } => {
                "fallback_required"
            }
        };
        self.insert(
            "invocation",
            &record.id().to_string(),
            "INSERT INTO invocations (invocation_id, capability_id, result, record_json, digest, written_at) \
             VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
            params![
                record.id().to_string(),
                body.capability_id.map(|id| id.to_string()),
                result,
                json,
                digest,
                self.clock.now_unix_ms(),
            ],
        )
    }

    /// Loads an invocation.
    ///
    /// # Errors
    ///
    /// Returns [`StoreError`] when the stored record cannot be read back as its contract.
    pub fn get_invocation(
        &self,
        id: &InvocationId,
    ) -> Result<Option<Envelope<Invocation>>, StoreError> {
        self.get_one(
            "SELECT record_json FROM invocations WHERE invocation_id = ?1",
            id,
        )
    }

    /// Records one benchmark measurement.
    ///
    /// # Errors
    ///
    /// Returns [`StoreError`] when the record exists or the task/condition/order triple repeats.
    pub fn put_benchmark(&mut self, record: &Envelope<BenchmarkRecord>) -> Result<(), StoreError> {
        let (json, digest) = encode(record)?;
        let body = record.body();
        self.insert(
            "benchmark",
            &record.id().to_string(),
            "INSERT INTO benchmark_records (benchmark_id, repo_id, task_id, condition, order_index, correctness, record_json, digest, written_at) \
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9)",
            params![
                record.id().to_string(),
                body.repository_id.to_string(),
                body.task_id,
                format!("{:?}", body.condition),
                body.order_index,
                format!("{:?}", body.correctness),
                json,
                digest,
                self.clock.now_unix_ms(),
            ],
        )
    }

    /// Lists a repository's benchmark records in a deterministic order.
    ///
    /// # Errors
    ///
    /// Returns [`StoreError`] when a stored record cannot be read back as its contract.
    pub fn list_benchmarks(
        &self,
        repository_id: &RepositoryId,
    ) -> Result<Vec<Envelope<BenchmarkRecord>>, StoreError> {
        self.list(
            "SELECT record_json FROM benchmark_records WHERE repo_id = ?1 \
             ORDER BY task_id ASC, condition ASC, order_index ASC, benchmark_id ASC",
            &repository_id.to_string(),
        )
    }

    /// Loads a benchmark record.
    ///
    /// # Errors
    ///
    /// Returns [`StoreError`] when the stored record cannot be read back as its contract.
    pub fn get_benchmark(
        &self,
        id: &BenchmarkId,
    ) -> Result<Option<Envelope<BenchmarkRecord>>, StoreError> {
        self.get_one(
            "SELECT record_json FROM benchmark_records WHERE benchmark_id = ?1",
            id,
        )
    }

    /// Records a recorder health event. Malformed input is counted here, never stored as a record.
    ///
    /// # Errors
    ///
    /// Returns [`StoreError`] when the write fails.
    pub fn record_health_event(&mut self, code: &str, detail: &str) -> Result<(), StoreError> {
        self.connection.execute(
            "INSERT INTO health_events (code, detail, observed_at) VALUES (?1, ?2, ?3)",
            params![code, detail, self.clock.now_unix_ms()],
        )?;
        Ok(())
    }

    /// Counts health events with a given code.
    ///
    /// # Errors
    ///
    /// Returns [`StoreError`] when the query fails.
    pub fn count_health_events(&self, code: &str) -> Result<u32, StoreError> {
        let count: i64 = self.connection.query_row(
            "SELECT COUNT(*) FROM health_events WHERE code = ?1",
            params![code],
            |row| row.get(0),
        )?;
        Ok(u32::try_from(count).unwrap_or(u32::MAX))
    }

    /// Returns when a repository record was written, according to the store's clock.
    ///
    /// # Errors
    ///
    /// Returns [`StoreError`] when the query fails.
    pub fn written_at(&self, id: &RepositoryId) -> Result<Option<i64>, StoreError> {
        self.connection
            .query_row(
                "SELECT written_at FROM repositories WHERE repo_id = ?1",
                params![id.to_string()],
                |row| row.get(0),
            )
            .map_or_else(swallow_missing, |value: i64| Ok(Some(value)))
    }

    /// Inserts one record, classifying the failure modes that matter.
    fn insert(
        &self,
        kind: &'static str,
        id: &str,
        sql: &str,
        parameters: impl rusqlite::Params,
    ) -> Result<(), StoreError> {
        classify_write(self.connection.execute(sql, parameters), kind, id)
    }

    /// Loads one record addressed by its identifier.
    fn get_one<T: Record, K: IdKind>(
        &self,
        sql: &str,
        id: &Id<K>,
    ) -> Result<Option<Envelope<T>>, StoreError> {
        let json: Option<String> = self
            .connection
            .query_row(sql, params![id.to_string()], |row| row.get(0))
            .map_or_else(swallow_missing, |value: String| Ok(Some(value)))?;

        json.map(|text| decode::<T>(&text)).transpose()
    }

    /// Loads a list of records in whatever order the query specified.
    fn list<T: Record>(&self, sql: &str, key: &str) -> Result<Vec<Envelope<T>>, StoreError> {
        let mut statement = self.connection.prepare(sql)?;
        let rows = statement.query_map(params![key], |row| row.get::<_, String>(0))?;

        let mut records = Vec::new();
        for row in rows {
            records.push(decode::<T>(&row?)?);
        }
        Ok(records)
    }

    /// Secures the WAL and shared-memory files once `SQLite` has created them.
    fn secure_sidecars(database: &Path) {
        for suffix in ["-wal", "-shm"] {
            let mut path = database.as_os_str().to_owned();
            path.push(suffix);
            let sidecar = PathBuf::from(path);
            if sidecar.exists() {
                let _ =
                    std::fs::set_permissions(&sidecar, Permissions::from_mode(PRIVATE_FILE_MODE));
            }
        }
    }
}

impl Drop for Store {
    fn drop(&mut self) {
        // The WAL and shared-memory files may only appear after the first write; catch them here
        // as well, so a database that is opened, written, and closed never leaves a readable file.
        if let Ok(path) = self
            .connection
            .path()
            .map_or_else(|| Err(()), |path| Ok(PathBuf::from(path)))
        {
            Self::secure_sidecars(&path);
        }
    }
}

/// Serializes a record to canonical JSON with its digest.
fn encode<T: Record>(record: &Envelope<T>) -> Result<(String, String), StoreError> {
    let json = canonical_string_of(record)?;
    let digest = record.digest()?;
    Ok((json, digest.to_string()))
}

/// Parses a stored record back into its contract.
fn decode<T: Record>(json: &str) -> Result<Envelope<T>, StoreError> {
    serde_json::from_str(json).map_err(|error| StoreError::RecordInvalid {
        reason: error.to_string(),
    })
}

/// Turns `SQLite`'s write failures into the store's own vocabulary.
fn classify_write(
    result: Result<usize, rusqlite::Error>,
    kind: &'static str,
    id: &str,
) -> Result<(), StoreError> {
    match result {
        Ok(_) => Ok(()),
        Err(rusqlite::Error::SqliteFailure(error, message)) => {
            let detail = message.unwrap_or_else(|| error.to_string());
            if detail.contains("UNIQUE constraint failed") || detail.contains("PRIMARY KEY") {
                Err(StoreError::AlreadyExists {
                    kind,
                    id: id.to_owned(),
                })
            } else if detail.contains("constraint failed") {
                Err(StoreError::ConstraintViolated { reason: detail })
            } else {
                Err(StoreError::Sqlite {
                    source: rusqlite::Error::SqliteFailure(error, Some(detail)),
                })
            }
        }
        Err(other) => Err(StoreError::Sqlite { source: other }),
    }
}

/// Turns "no such row" into `None` and leaves every other error alone.
fn swallow_missing<T>(error: rusqlite::Error) -> Result<Option<T>, StoreError> {
    if matches!(error, rusqlite::Error::QueryReturnedNoRows) {
        Ok(None)
    } else {
        Err(StoreError::Sqlite { source: error })
    }
}

/// Refuses a database file that anyone but its owner can read or write.
fn enforce_private_mode(path: &Path) -> Result<(), StoreError> {
    let metadata = std::fs::metadata(path).map_err(|error| StoreError::Io {
        path: path.display().to_string(),
        reason: error.to_string(),
    })?;
    let mode = metadata.permissions().mode() & 0o777;
    if mode & 0o077 != 0 {
        return Err(StoreError::PermissionsTooOpen {
            path: path.display().to_string(),
            mode,
        });
    }
    Ok(())
}

/// Sets `0600` on a file the store just created.
fn secure_file(path: &Path) -> Result<(), StoreError> {
    std::fs::set_permissions(path, Permissions::from_mode(PRIVATE_FILE_MODE)).map_err(|error| {
        StoreError::Io {
            path: path.display().to_string(),
            reason: error.to_string(),
        }
    })
}

/// Generates identifiers for new records.
///
/// The body is time-ordered so that records sort by creation, and random in its tail so two
/// processes recording at the same millisecond cannot collide.
pub trait IdSource: Send + Sync {
    /// Returns the next identifier body: 26 Crockford base32 characters.
    fn next_body(&self) -> String;

    /// Returns the next identifier of kind `K`.
    ///
    /// # Errors
    ///
    /// Returns [`StoreError`] when the source produced a body that is not well-formed.
    fn next<K: IdKind>(&self) -> Result<Id<K>, StoreError>
    where
        Self: Sized,
    {
        Id::from_body(&self.next_body()).map_err(|error| StoreError::RecordInvalid {
            reason: format!("{}: {error}", error.code()),
        })
    }
}

/// Crockford base32 alphabet, matching `agent_jit_domain::ids`.
const ALPHABET: &[u8; 32] = b"0123456789ABCDEFGHJKMNPQRSTVWXYZ";

/// Time-ordered random identifiers.
#[derive(Debug, Clone, Copy, Default)]
pub struct SystemIdSource;

impl IdSource for SystemIdSource {
    fn next_body(&self) -> String {
        let millis = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map_or(0_u64, |elapsed| {
                u64::try_from(elapsed.as_millis()).unwrap_or(u64::MAX)
            });

        let mut random = [0_u8; 16];
        // A failure here would mean the operating system has no entropy; fall back to the clock
        // rather than panicking inside a hook.
        if getrandom::fill(&mut random).is_err() {
            random[..8].copy_from_slice(&millis.to_be_bytes());
        }

        let mut body = String::with_capacity(26);
        // 10 characters of timestamp: 50 bits, which is milliseconds well past the year 3000.
        for index in (0..10).rev() {
            let shift = index * 5;
            let value = usize::try_from((millis >> shift) & 0x1f).unwrap_or(0);
            body.push(char::from(ALPHABET[value]));
        }
        // 16 characters of randomness.
        for byte in random {
            body.push(char::from(ALPHABET[usize::from(byte & 0x1f)]));
        }
        body
    }
}

/// A deterministic identifier source for tests: `prefix` plus a counter.
#[derive(Debug)]
pub struct SequentialIdSource {
    counter: std::sync::atomic::AtomicU64,
}

impl Default for SequentialIdSource {
    fn default() -> Self {
        Self::new()
    }
}

impl SequentialIdSource {
    /// Builds a source that starts at zero.
    #[must_use]
    pub const fn new() -> Self {
        Self {
            counter: std::sync::atomic::AtomicU64::new(0),
        }
    }
}

impl IdSource for SequentialIdSource {
    fn next_body(&self) -> String {
        let next = self
            .counter
            .fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        let mut body = String::with_capacity(26);
        for index in (0..26).rev() {
            let shift = index * 5;
            let value = usize::try_from((next >> shift) & 0x1f).unwrap_or(0);
            body.push(char::from(ALPHABET[value]));
        }
        body
    }
}
