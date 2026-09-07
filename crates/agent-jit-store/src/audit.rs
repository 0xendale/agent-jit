//! Session holds and durable corpus audit entries.

use agent_jit_domain::ids::SessionId;
use rusqlite::{OptionalExtension as _, TransactionBehavior, params};

use crate::{Store, StoreError};

/// Active retention hold on one session.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ActiveHold {
    /// Protected session.
    pub session_id: SessionId,
    /// Operator who placed the hold.
    pub actor: String,
    /// Why evidence must be retained.
    pub rationale: String,
    /// Store-clock timestamp.
    pub recorded_at_unix_ms: i64,
}

/// Audited corpus operation.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AuditAction {
    /// Hold created or refreshed.
    Hold,
    /// Hold released.
    Release,
    /// Retention deletion applied.
    Prune,
}

impl AuditAction {
    /// Stable database spelling.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Hold => "hold",
            Self::Release => "release",
            Self::Prune => "prune",
        }
    }

    fn parse(value: &str) -> Result<Self, StoreError> {
        match value {
            "hold" => Ok(Self::Hold),
            "release" => Ok(Self::Release),
            "prune" => Ok(Self::Prune),
            _ => Err(StoreError::RecordInvalid {
                reason: format!("unknown corpus audit action `{value}`"),
            }),
        }
    }
}

/// One immutable corpus audit entry.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CorpusAudit {
    /// Operation performed.
    pub action: AuditAction,
    /// Session affected, absent for a multi-session prune.
    pub session_id: Option<SessionId>,
    /// Operator or component responsible.
    pub actor: String,
    /// Human-readable reason.
    pub rationale: String,
    /// Store-clock timestamp.
    pub recorded_at_unix_ms: i64,
}

impl Store {
    /// Places or refreshes a session hold and records the operation atomically.
    ///
    /// # Errors
    ///
    /// Returns [`StoreError`] when the session is absent or either write fails.
    pub fn hold_session(
        &mut self,
        session_id: &SessionId,
        actor: &str,
        rationale: &str,
    ) -> Result<(), StoreError> {
        let transaction = self
            .connection
            .transaction_with_behavior(TransactionBehavior::Immediate)?;
        let existing = transaction
            .query_row(
                "SELECT actor,rationale FROM session_holds WHERE session_id=?1",
                [session_id.to_string()],
                |row| Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?)),
            )
            .optional()?;
        if existing
            .as_ref()
            .is_some_and(|(current_actor, current_rationale)| {
                current_actor == actor && current_rationale == rationale
            })
        {
            transaction.commit()?;
            return Ok(());
        }
        let now = self.clock.now_unix_ms();
        transaction.execute(
            "INSERT INTO session_holds (session_id,actor,rationale,recorded_at_unix_ms) \
             VALUES (?1,?2,?3,?4) ON CONFLICT(session_id) DO UPDATE SET \
             actor=excluded.actor,rationale=excluded.rationale,\
             recorded_at_unix_ms=excluded.recorded_at_unix_ms",
            params![session_id.to_string(), actor, rationale, now],
        )?;
        insert_audit(
            &transaction,
            AuditAction::Hold,
            Some(session_id),
            actor,
            rationale,
            now,
        )?;
        transaction.commit()?;
        Ok(())
    }

    /// Releases a session hold and records the operation atomically.
    ///
    /// # Errors
    ///
    /// Returns [`StoreError`] when either SQL operation fails.
    pub fn release_session(
        &mut self,
        session_id: &SessionId,
        actor: &str,
        rationale: &str,
    ) -> Result<(), StoreError> {
        let transaction = self
            .connection
            .transaction_with_behavior(TransactionBehavior::Immediate)?;
        let now = self.clock.now_unix_ms();
        let deleted = transaction.execute(
            "DELETE FROM session_holds WHERE session_id=?1",
            [session_id.to_string()],
        )?;
        if deleted == 0 {
            transaction.commit()?;
            return Ok(());
        }
        insert_audit(
            &transaction,
            AuditAction::Release,
            Some(session_id),
            actor,
            rationale,
            now,
        )?;
        transaction.commit()?;
        Ok(())
    }

    /// Loads the active hold for a session.
    ///
    /// # Errors
    ///
    /// Returns [`StoreError`] when SQL or a stored identifier is invalid.
    pub fn active_hold(&self, session_id: &SessionId) -> Result<Option<ActiveHold>, StoreError> {
        let row = self
            .connection
            .query_row(
                "SELECT actor,rationale,recorded_at_unix_ms FROM session_holds WHERE session_id=?1",
                [session_id.to_string()],
                |row| {
                    Ok((
                        row.get::<_, String>(0)?,
                        row.get::<_, String>(1)?,
                        row.get(2)?,
                    ))
                },
            )
            .optional()?;
        Ok(
            row.map(|(actor, rationale, recorded_at_unix_ms)| ActiveHold {
                session_id: *session_id,
                actor,
                rationale,
                recorded_at_unix_ms,
            }),
        )
    }

    /// Lists corpus audit entries in insertion order.
    ///
    /// # Errors
    ///
    /// Returns [`StoreError`] when SQL or stored values are invalid.
    pub fn list_corpus_audit(&self) -> Result<Vec<CorpusAudit>, StoreError> {
        let mut statement = self.connection.prepare(
            "SELECT action,session_id,actor,rationale,recorded_at_unix_ms \
             FROM corpus_audit ORDER BY audit_id",
        )?;
        let rows = statement.query_map([], |row| {
            Ok((
                row.get::<_, String>(0)?,
                row.get::<_, Option<String>>(1)?,
                row.get::<_, String>(2)?,
                row.get::<_, String>(3)?,
                row.get::<_, i64>(4)?,
            ))
        })?;
        let mut audit = Vec::new();
        for row in rows {
            let (action, session, actor, rationale, recorded_at_unix_ms) = row?;
            audit.push(CorpusAudit {
                action: AuditAction::parse(&action)?,
                session_id: session.map(|value| parse_session(&value)).transpose()?,
                actor,
                rationale,
                recorded_at_unix_ms,
            });
        }
        Ok(audit)
    }
}

pub(crate) fn insert_audit(
    transaction: &rusqlite::Transaction<'_>,
    action: AuditAction,
    session_id: Option<&SessionId>,
    actor: &str,
    rationale: &str,
    recorded_at_unix_ms: i64,
) -> Result<(), StoreError> {
    transaction.execute(
        "INSERT INTO corpus_audit (action,session_id,actor,rationale,recorded_at_unix_ms) \
         VALUES (?1,?2,?3,?4,?5)",
        params![
            action.as_str(),
            session_id.map(ToString::to_string),
            actor,
            rationale,
            recorded_at_unix_ms,
        ],
    )?;
    Ok(())
}

fn parse_session(value: &str) -> Result<SessionId, StoreError> {
    value.parse().map_err(|error| StoreError::RecordInvalid {
        reason: format!("invalid stored session id: {error}"),
    })
}
