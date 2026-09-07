//! Deterministic, session-unit retention planning and application.

use agent_jit_domain::ids::SessionId;
use rusqlite::{Connection, OptionalExtension as _, TransactionBehavior};

use crate::audit::{AuditAction, insert_audit};
use crate::retention_size::{logical_bytes, size_overflow, total_bytes};
use crate::{Store, StoreError};

const DEFAULT_MAX_AGE_MS: i64 = 30 * 86_400_000;
const DEFAULT_MAX_BYTES: u64 = 1_073_741_824;

/// Retention limits evaluated against one pinned instant.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RetentionPolicy {
    now_unix_ms: i64,
    max_age_ms: i64,
    max_bytes: u64,
}

impl RetentionPolicy {
    /// Builds a policy after validating age and cutoff arithmetic.
    ///
    /// # Errors
    ///
    /// Returns a typed age error for a negative limit or overflowing cutoff.
    pub const fn new(
        now_unix_ms: i64,
        max_age_ms: i64,
        max_bytes: u64,
    ) -> Result<Self, StoreError> {
        if max_age_ms < 0 {
            return Err(StoreError::RetentionAgeInvalid);
        }
        if now_unix_ms.checked_sub(max_age_ms).is_none() {
            return Err(StoreError::RetentionAgeOverflow);
        }
        Ok(Self {
            now_unix_ms,
            max_age_ms,
            max_bytes,
        })
    }

    /// Builds a policy after checking its age cutoff.
    ///
    /// # Errors
    ///
    /// Returns [`StoreError::RetentionAgeOverflow`] when cutoff arithmetic overflows.
    pub fn checked(now_unix_ms: i64, max_age_ms: i64, max_bytes: u64) -> Result<Self, StoreError> {
        Self::new(now_unix_ms, max_age_ms, max_bytes)
    }

    /// Returns the default 30-day, one-GiB policy.
    #[must_use]
    pub const fn default_at(now_unix_ms: i64) -> Self {
        Self {
            now_unix_ms,
            max_age_ms: DEFAULT_MAX_AGE_MS,
            max_bytes: DEFAULT_MAX_BYTES,
        }
    }

    /// Maximum retained age in milliseconds.
    #[must_use]
    pub const fn max_age_ms(self) -> i64 {
        self.max_age_ms
    }

    /// Maximum logical evidence bytes.
    #[must_use]
    pub const fn max_bytes(self) -> u64 {
        self.max_bytes
    }

    fn cutoff(self) -> Result<i64, StoreError> {
        self.now_unix_ms
            .checked_sub(self.max_age_ms)
            .ok_or(StoreError::RetentionAgeOverflow)
    }
}

/// Whether retention only plans or atomically applies deletion.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PruneMode {
    /// Compute selection without writing anything.
    DryRun,
    /// Delete selected session units and append audit in one transaction.
    Apply,
}

/// Current logical retention usage.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RetentionUsage {
    /// Canonical record bytes plus declared event payload bytes.
    pub logical_bytes: u64,
}

/// Deterministic retention result.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PruneReport {
    /// Sessions selected by policy, oldest first.
    pub selected_sessions: Vec<SessionId>,
    /// Sessions actually deleted; empty for dry-run.
    pub deleted_sessions: Vec<SessionId>,
}

struct Candidate {
    id: SessionId,
    ended_at: Option<i64>,
    bytes: u64,
    protected: bool,
}

impl Store {
    /// Returns logical bytes owned by one complete session unit.
    ///
    /// # Errors
    ///
    /// Returns a typed size error for negative or overflowing accounting.
    pub fn session_logical_bytes(&self, session_id: &SessionId) -> Result<u64, StoreError> {
        logical_bytes(&self.connection, session_id)
    }

    /// Returns total logical bytes across session-owned evidence.
    ///
    /// # Errors
    ///
    /// Returns a typed size error when any session cannot be accounted safely.
    pub fn retention_usage(&self) -> Result<RetentionUsage, StoreError> {
        Ok(RetentionUsage {
            logical_bytes: total_bytes(&self.connection)?,
        })
    }

    /// Plans or atomically applies session-unit retention.
    ///
    /// # Errors
    ///
    /// Returns a typed age, size, protection, or SQL error without deleting evidence.
    pub fn prune(
        &mut self,
        policy: RetentionPolicy,
        mode: PruneMode,
    ) -> Result<PruneReport, StoreError> {
        if mode == PruneMode::DryRun {
            return Ok(PruneReport {
                selected_sessions: plan(&self.connection, policy)?,
                deleted_sessions: Vec::new(),
            });
        }

        let transaction = self
            .connection
            .transaction_with_behavior(TransactionBehavior::Immediate)?;
        let selected = plan(&transaction, policy)?;
        for session_id in &selected {
            if is_protected(&transaction, session_id)? {
                return Err(StoreError::RetentionLimitUnmetProtected);
            }
            delete_session_unit(&transaction, session_id)?;
        }
        insert_audit(
            &transaction,
            AuditAction::Prune,
            None,
            "agent-jit",
            "retention policy",
            self.clock.now_unix_ms(),
        )?;
        transaction.commit()?;
        Ok(PruneReport {
            selected_sessions: selected.clone(),
            deleted_sessions: selected,
        })
    }
}

fn plan(connection: &Connection, policy: RetentionPolicy) -> Result<Vec<SessionId>, StoreError> {
    let cutoff = policy.cutoff()?;
    let mut candidates = candidates(connection)?;
    let mut remaining = candidates.iter().try_fold(0_u64, |total, candidate| {
        total.checked_add(candidate.bytes).ok_or_else(size_overflow)
    })?;
    let mut selected = Vec::new();

    for candidate in &candidates {
        if candidate
            .ended_at
            .is_some_and(|ended_at| ended_at <= cutoff)
        {
            if candidate.protected {
                return Err(StoreError::RetentionLimitUnmetProtected);
            }
            selected.push(candidate.id);
            remaining = remaining
                .checked_sub(candidate.bytes)
                .ok_or_else(size_overflow)?;
        }
    }
    for candidate in &mut candidates {
        if remaining <= policy.max_bytes || selected.contains(&candidate.id) {
            continue;
        }
        if !candidate.protected {
            selected.push(candidate.id);
            remaining = remaining
                .checked_sub(candidate.bytes)
                .ok_or_else(size_overflow)?;
        }
    }
    if remaining > policy.max_bytes {
        return Err(StoreError::RetentionLimitUnmetProtected);
    }
    Ok(selected)
}

fn candidates(connection: &Connection) -> Result<Vec<Candidate>, StoreError> {
    let mut statement = connection.prepare(
        "SELECT session_id,ended_at_unix_ms FROM sessions \
         ORDER BY ended_at_unix_ms IS NULL,ended_at_unix_ms,session_id",
    )?;
    let rows = statement.query_map([], |row| {
        Ok((row.get::<_, String>(0)?, row.get::<_, Option<i64>>(1)?))
    })?;
    let mut candidates = Vec::new();
    for row in rows {
        let (id, ended_at) = row?;
        let id = id.parse().map_err(|error| StoreError::RecordInvalid {
            reason: format!("invalid stored session id: {error}"),
        })?;
        candidates.push(Candidate {
            id,
            ended_at,
            bytes: logical_bytes(connection, &id)?,
            protected: ended_at.is_none() || is_protected(connection, &id)?,
        });
    }
    Ok(candidates)
}

fn is_protected(connection: &Connection, session_id: &SessionId) -> Result<bool, StoreError> {
    connection
        .query_row(
            "SELECT 1 WHERE EXISTS(SELECT 1 FROM session_holds WHERE session_id=?1) OR EXISTS(\
             SELECT 1 FROM trajectories t WHERE t.session_id=?1 AND (\
             EXISTS(SELECT 1 FROM frozen_corpus_trajectories f WHERE f.trajectory_id=t.trajectory_id) OR \
             EXISTS(SELECT 1 FROM group_members g WHERE g.trajectory_id=t.trajectory_id) OR \
             EXISTS(SELECT 1 FROM candidate_trajectories c WHERE c.trajectory_id=t.trajectory_id) OR \
             EXISTS(SELECT 1 FROM replays r WHERE r.fixture_trajectory_id=t.trajectory_id) OR \
             EXISTS(SELECT 1 FROM benchmark_trajectories b WHERE b.trajectory_id=t.trajectory_id)))",
            [session_id.to_string()],
            |_| Ok(true),
        )
        .optional()
        .map(|value| value.unwrap_or(false))
        .map_err(StoreError::from)
}

fn delete_session_unit(
    transaction: &rusqlite::Transaction<'_>,
    session_id: &SessionId,
) -> Result<(), StoreError> {
    let id = session_id.to_string();
    for sql in [
        "DELETE FROM outcome_annotation_current WHERE trajectory_id IN (SELECT trajectory_id FROM trajectories WHERE session_id=?1)",
        "DELETE FROM outcome_annotations WHERE trajectory_id IN (SELECT trajectory_id FROM trajectories WHERE session_id=?1)",
        "DELETE FROM trace_metrics WHERE trajectory_id IN (SELECT trajectory_id FROM trajectories WHERE session_id=?1)",
        "DELETE FROM outcomes WHERE trajectory_id IN (SELECT trajectory_id FROM trajectories WHERE session_id=?1)",
        "DELETE FROM trajectories WHERE session_id=?1",
        "DELETE FROM events WHERE session_id=?1",
        "DELETE FROM session_holds WHERE session_id=?1",
        "DELETE FROM sessions WHERE session_id=?1",
    ] {
        transaction.execute(sql, [&id])?;
    }
    Ok(())
}
