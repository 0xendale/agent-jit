//! Finalizing an aggregated session into the database.
//!
//! Identifiers are derived from content, not generated: the same session finalized twice produces
//! the same identifiers, so recovery after a crash converges on one trajectory instead of piling
//! up near-duplicates. Finalization is therefore idempotent by construction rather than by a flag.

use agent_jit_domain::canonical::{Digest, digest_of};
use agent_jit_domain::envelope::{Envelope, Provenance, ProvenanceSource};
use agent_jit_domain::ids::{EventId, RepositoryId, SessionId, TrajectoryId};
use agent_jit_domain::trace::{Event, EventKind, Repository, Runtime, Session, Trajectory};
use agent_jit_store::{Store, StoreError};
use serde_json::json;

use crate::adapters::claude_hooks::{HookEventKind, HookPayload};
use crate::metrics::{MetricInputs, compute_trace_metrics};
use crate::recorder::{RecorderError, SessionAggregate};
use crate::repository::RepositoryIdentity;

/// What finalization did.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FinalizeReport {
    /// The trajectory the session became.
    pub trajectory_id: TrajectoryId,
    /// The session record.
    pub session_id: SessionId,
    /// How many events were written.
    pub events_written: u32,
    /// Whether this call created the trajectory, as opposed to finding it already there.
    pub created: bool,
}

/// Why a session could not be finalized.
#[derive(Debug, thiserror::Error)]
pub enum FinalizeError {
    /// The session is still open; only a complete session may be finalized.
    #[error("session `{session_key}` has no SessionEnd and is still open")]
    Incomplete {
        /// The session that was asked for.
        session_key: String,
    },
    /// The recorder could not aggregate the session.
    #[error("{}: {source}", source.code())]
    Recorder {
        /// The underlying failure.
        #[from]
        source: RecorderError,
    },
    /// The database refused a write.
    #[error("{}: {source}", source.code())]
    Store {
        /// The underlying failure.
        #[from]
        source: StoreError,
    },
    /// A record could not be canonicalized.
    #[error("{}: {source}", source.code())]
    Canonical {
        /// The underlying failure.
        #[from]
        source: agent_jit_domain::canonical::CanonicalError,
    },
}

impl FinalizeError {
    /// Stable machine-readable code.
    #[must_use]
    pub const fn code(&self) -> &'static str {
        match self {
            Self::Incomplete { .. } => "recorder_session_incomplete",
            Self::Recorder { source } => source.code(),
            Self::Store { source } => source.code(),
            Self::Canonical { source } => source.code(),
        }
    }
}

/// Writes one aggregated session into the store.
///
/// # Errors
///
/// Returns [`FinalizeError`] when the session is still open or the database refuses a write.
pub fn finalize(
    store: &mut Store,
    aggregate: &SessionAggregate,
    identity: &RepositoryIdentity,
    recorder_version: &str,
) -> Result<FinalizeReport, FinalizeError> {
    if !aggregate.complete {
        return Err(FinalizeError::Incomplete {
            session_key: aggregate.session_key.clone(),
        });
    }

    let repository_id = identity.repo_id;
    ensure_repository(store, identity)?;

    let session_id = derive_session_id(&repository_id, &aggregate.session_key)?;
    let trajectory_id = derive_trajectory_id(&session_id)?;

    if store.get_trajectory(&trajectory_id)?.is_some() {
        return Ok(FinalizeReport {
            trajectory_id,
            session_id,
            events_written: 0,
            created: false,
        });
    }

    let provenance = Provenance {
        produced_by: recorder_version.to_owned(),
        source: ProvenanceSource::Recorded,
        recorded_at_unix_ms: aggregate.started_at_unix_ms,
        parents: Vec::new(),
    };

    write_session(
        store,
        aggregate,
        identity,
        session_id,
        &provenance,
        recorder_version,
    )?;
    let (event_ids, events_written) = write_events(store, aggregate, session_id, &provenance)?;

    let trajectory = Envelope::new(
        trajectory_id,
        provenance,
        Trajectory {
            session_id,
            repository_id,
            head_commit: identity.head_commit.clone(),
            intent: aggregate.intent.clone(),
            events: event_ids,
            // The outcome is annotated by a human later; an unannotated trajectory is a fact, and
            // inventing a status here would put a guess into the evidence Phase 0 reads.
            outcome_id: None,
            started_at_unix_ms: aggregate.started_at_unix_ms,
            duration_ms: aggregate.active_duration_ms,
        },
    );
    store.put_trajectory(&trajectory)?;
    record_health(store, aggregate)?;

    // Metrics are computed from the events that were just stored, not from the in-memory
    // aggregate, so the persisted numbers are exactly what a later recomputation would produce.
    let stored_events = store.list_events(&session_id)?;
    let inputs = MetricInputs {
        repository_id,
        session_id,
        head_commit: identity.head_commit.clone(),
        adapter_schema: aggregate
            .events
            .first()
            .map(|segment| segment.payload.adapter_schema.clone()),
        runtime_version: aggregate.claude_version.clone(),
        // No hook payload carries the model, so it stays absent rather than invented.
        model_id: None,
        exact_input_tokens: None,
        quarantined_segments: u32::try_from(aggregate.quarantined.len()).unwrap_or(u32::MAX),
        duplicates_collapsed: aggregate.duplicates,
    };
    if let Ok(metrics) = compute_trace_metrics(&stored_events, &inputs) {
        store.put_trace_metrics(&trajectory_id, &metrics)?;
    }

    Ok(FinalizeReport {
        trajectory_id,
        session_id,
        events_written,
        created: true,
    })
}

/// Writes the session record if it is not already there.
fn write_session(
    store: &mut Store,
    aggregate: &SessionAggregate,
    identity: &RepositoryIdentity,
    session_id: SessionId,
    provenance: &Provenance,
    recorder_version: &str,
) -> Result<(), FinalizeError> {
    if store.get_session(&session_id)?.is_some() {
        return Ok(());
    }

    let session = Envelope::new(
        session_id,
        provenance.clone(),
        Session {
            repository_id: identity.repo_id,
            worktree_root: identity.worktree_root.to_string_lossy().into_owned(),
            head_commit: identity.head_commit.clone(),
            runtime: Runtime::ClaudeCode,
            runtime_version: aggregate
                .claude_version
                .clone()
                .unwrap_or_else(|| "unknown".to_owned()),
            // The model is not in any hook payload; recording a guess would poison the benchmark,
            // which pins the model explicitly.
            model_id: "unrecorded".to_owned(),
            recorder_version: recorder_version.to_owned(),
            started_at_unix_ms: aggregate.started_at_unix_ms,
            ended_at_unix_ms: Some(aggregate.ended_at_unix_ms),
        },
    );
    store.put_session(&session)?;
    Ok(())
}

/// Writes every event, skipping any already present, and returns their identifiers in order.
fn write_events(
    store: &mut Store,
    aggregate: &SessionAggregate,
    session_id: SessionId,
    provenance: &Provenance,
) -> Result<(Vec<EventId>, u32), FinalizeError> {
    let existing: Vec<u32> = store
        .list_events(&session_id)?
        .iter()
        .map(|event| event.body().sequence)
        .collect();

    let mut event_ids = Vec::with_capacity(aggregate.events.len());
    let mut written = 0_u32;

    for (index, segment) in aggregate.events.iter().enumerate() {
        let sequence = u32::try_from(index).unwrap_or(u32::MAX);
        let event_id = derive_event_id(&session_id, sequence, &segment.event_id)?;
        event_ids.push(event_id);

        if existing.contains(&sequence) {
            continue;
        }

        let (kind, tool_name, payload_digest) = describe(segment);
        let event = Envelope::new(
            event_id,
            provenance.clone(),
            Event {
                session_id,
                sequence,
                kind,
                tool_name,
                argv: Vec::new(),
                exit_code: None,
                payload_digest: Some(payload_digest),
                payload_bytes: segment.length,
                payload_truncated: false,
                started_at_unix_ms: segment.recorded_at_unix_ms,
                duration_ms: 0,
            },
        );
        store.put_event(&event)?;
        written = written.saturating_add(1);
    }

    Ok((event_ids, written))
}

/// Records what recovery had to throw away, so a lossy session is visible rather than silent.
fn record_health(store: &mut Store, aggregate: &SessionAggregate) -> Result<(), FinalizeError> {
    for quarantined in &aggregate.quarantined {
        store.record_health_event(
            quarantined.reason,
            &format!("{} bytes quarantined", quarantined.bytes),
        )?;
    }
    if aggregate.duplicates > 0 {
        store.record_health_event(
            "recorder_duplicate_events",
            &format!("{} duplicates collapsed", aggregate.duplicates),
        )?;
    }
    Ok(())
}

/// Writes the repository record if it is not already there.
fn ensure_repository(
    store: &mut Store,
    identity: &RepositoryIdentity,
) -> Result<(), FinalizeError> {
    if store.get_repository(&identity.repo_id)?.is_some() {
        return Ok(());
    }

    let label = identity.worktree_root.file_name().map_or_else(
        || "repository".to_owned(),
        |name| name.to_string_lossy().into_owned(),
    );

    let record = Envelope::new(
        identity.repo_id,
        Provenance::recorded_by(env!("CARGO_PKG_NAME")),
        Repository {
            git_common_dir: identity.git_common_dir.to_string_lossy().into_owned(),
            identity_digest: digest_of(&json!({
                "kind": "git_common_dir",
                "git_common_dir": identity.git_common_dir.to_string_lossy(),
            }))?,
            label,
        },
    );
    store.put_repository(&record)?;
    Ok(())
}

/// Maps a segment to the stored event's indexed columns.
fn describe(segment: &crate::recorder::Segment) -> (EventKind, Option<String>, Digest) {
    // Every hook kind keeps its own identity in the stored event. Collapsing the boundaries into
    // AgentMessage would erase the turn structure that active duration is computed from.
    let kind = match segment.payload.kind {
        HookEventKind::UserPromptSubmit => EventKind::UserPrompt,
        HookEventKind::PreToolUse => EventKind::ToolCall,
        HookEventKind::PostToolUse | HookEventKind::PostToolUseFailure => EventKind::ToolResult,
        HookEventKind::SessionStart => EventKind::SessionStart,
        HookEventKind::Stop => EventKind::Stop,
        HookEventKind::SessionEnd => EventKind::SessionEnd,
    };

    let tool_name = match &segment.payload.payload {
        HookPayload::PreToolUse { tool_name, .. } => Some(tool_name.clone()),
        HookPayload::PostToolUse(result) => Some(result.tool_name.clone()),
        _ => None,
    };

    (kind, tool_name, segment.checksum)
}

/// Derives the session identifier from the repository and Claude's session key.
fn derive_session_id(
    repository_id: &RepositoryId,
    session_key: &str,
) -> Result<SessionId, FinalizeError> {
    Ok(SessionId::derived(&digest_of(&json!({
        "repository_id": repository_id.to_string(),
        "claude_session_key": session_key,
    }))?))
}

/// Derives an event identifier from its session, position, and payload.
///
/// The payload alone is not identity: a multi-turn session produces byte-identical `Stop` payloads,
/// one per turn, and those are two occurrences rather than one duplicate. Including the sequence
/// keeps them distinct while staying deterministic, so re-running recovery addresses the same rows.
fn derive_event_id(
    session_id: &SessionId,
    sequence: u32,
    payload_digest: &Digest,
) -> Result<EventId, FinalizeError> {
    Ok(EventId::derived(&digest_of(&json!({
        "session_id": session_id.to_string(),
        "sequence": sequence,
        "payload_digest": payload_digest.to_string(),
    }))?))
}

/// Derives the trajectory identifier from the session it summarizes.
fn derive_trajectory_id(session_id: &SessionId) -> Result<TrajectoryId, FinalizeError> {
    Ok(TrajectoryId::derived(&digest_of(&json!({
        "session_id": session_id.to_string(),
    }))?))
}
