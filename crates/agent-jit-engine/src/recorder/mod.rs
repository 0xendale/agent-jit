//! The recorder: from spooled hook events to one trajectory.
//!
//! A trajectory is one Claude session. Its *cost* is the work the agent did, not the time the
//! human spent thinking: active duration sums each `UserPromptSubmit → Stop` interval, so a
//! session left open over lunch does not look expensive. Phase 0 divides by these numbers, so
//! getting them wrong would not produce a slightly-off report — it would produce a wrong decision.

mod finalize;
mod segment;

pub use finalize::{FinalizeError, FinalizeReport, finalize};
pub use segment::{
    QuarantinedSegment, SEGMENT_VERSION, Segment, SegmentStore, SegmentWriteError, SessionSegments,
};

use agent_jit_domain::redaction::MAX_TRAJECTORY_BYTES;

use crate::adapters::claude_hooks::{HookEventKind, HookPayload};

/// What one session's segments add up to.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SessionAggregate {
    /// Claude's session identifier.
    pub session_key: String,
    /// Working directory Claude reported, which is where the repository is discovered.
    pub cwd: String,
    /// The intent of the session: the first user prompt.
    pub intent: String,
    /// Claude CLI version observed, when the launcher recorded one.
    pub claude_version: Option<String>,
    /// Number of `UserPromptSubmit` turns.
    pub turns: u32,
    /// Number of tool calls observed.
    pub tool_calls: u32,
    /// Number of tool calls that reported an error.
    pub failed_tool_calls: u32,
    /// Sum of `UserPromptSubmit → Stop` intervals: the agent's working time.
    pub active_duration_ms: i64,
    /// First-to-last event span, including time the user was idle.
    pub wall_duration_ms: i64,
    /// Time of the first event.
    pub started_at_unix_ms: i64,
    /// Time of the last event.
    pub ended_at_unix_ms: i64,
    /// Whether a `SessionEnd` was observed. Only a complete session may be finalized.
    pub complete: bool,
    /// Total retained payload bytes, which the trajectory budget bounds.
    pub retained_bytes: u64,
    /// Segments that failed verification.
    pub quarantined: Vec<QuarantinedSegment>,
    /// Duplicate events collapsed during recovery.
    pub duplicates: u32,
    /// The verified, ordered events.
    pub events: Vec<Segment>,
}

/// Why a session could not be aggregated.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum RecorderError {
    /// The session has no usable segments.
    #[error("session `{session_key}` has no usable events")]
    Empty {
        /// The session that was asked for.
        session_key: String,
    },
    /// The session's payloads exceeded the per-trajectory ceiling.
    #[error(
        "session `{session_key}` retained {bytes} bytes; the ceiling is {MAX_TRAJECTORY_BYTES}"
    )]
    TooLarge {
        /// The session that was asked for.
        session_key: String,
        /// Bytes retained.
        bytes: u64,
    },
    /// The segment store refused an operation.
    #[error("{}: {source}", source.code())]
    Segment {
        /// The underlying failure.
        #[from]
        source: SegmentWriteError,
    },
}

impl RecorderError {
    /// Stable machine-readable code.
    #[must_use]
    pub const fn code(&self) -> &'static str {
        match self {
            Self::Empty { .. } => "recorder_empty_session",
            Self::TooLarge { .. } => "recorder_session_too_large",
            Self::Segment { source } => source.code(),
        }
    }
}

/// Aggregates spooled segments into sessions.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Recorder {
    segments: SegmentStore,
}

impl Recorder {
    /// Builds a recorder over a segment store.
    #[must_use]
    pub const fn new(segments: SegmentStore) -> Self {
        Self { segments }
    }

    /// Returns the segment store.
    #[must_use]
    pub const fn segments(&self) -> &SegmentStore {
        &self.segments
    }

    /// Reads and aggregates one session.
    ///
    /// # Errors
    ///
    /// Returns [`RecorderError`] when the session has no usable events, exceeds the trajectory
    /// ceiling, or its segments cannot be read.
    pub fn aggregate(&self, session_key: &str) -> Result<SessionAggregate, RecorderError> {
        let read = self.segments.read_session(session_key)?;
        Self::aggregate_segments(session_key, read)
    }

    /// Aggregates already-read segments.
    ///
    /// # Errors
    ///
    /// Returns [`RecorderError`] when there is nothing usable or the ceiling is exceeded.
    pub fn aggregate_segments(
        session_key: &str,
        read: SessionSegments,
    ) -> Result<SessionAggregate, RecorderError> {
        if read.accepted.is_empty() {
            return Err(RecorderError::Empty {
                session_key: session_key.to_owned(),
            });
        }

        let mut aggregate = SessionAggregate {
            session_key: session_key.to_owned(),
            cwd: String::new(),
            intent: String::new(),
            claude_version: None,
            turns: 0,
            tool_calls: 0,
            failed_tool_calls: 0,
            active_duration_ms: 0,
            wall_duration_ms: 0,
            started_at_unix_ms: read.accepted[0].recorded_at_unix_ms,
            ended_at_unix_ms: read.accepted[0].recorded_at_unix_ms,
            complete: false,
            retained_bytes: 0,
            quarantined: read.quarantined,
            duplicates: read.duplicates,
            events: Vec::new(),
        };

        // A turn opens at a prompt and closes at the next Stop. Time outside a turn is the user
        // thinking, and is not the agent's cost.
        let mut turn_started_at: Option<i64> = None;
        let mut last_event_at = read.accepted[0].recorded_at_unix_ms;

        for segment in &read.accepted {
            aggregate.retained_bytes = aggregate.retained_bytes.saturating_add(segment.length);
            last_event_at = segment.recorded_at_unix_ms;

            if aggregate.cwd.is_empty() {
                aggregate.cwd.clone_from(&segment.payload.cwd);
            }
            if aggregate.claude_version.is_none() {
                aggregate
                    .claude_version
                    .clone_from(&segment.payload.claude_version);
            }

            match (&segment.payload.kind, &segment.payload.payload) {
                (HookEventKind::UserPromptSubmit, HookPayload::UserPrompt { prompt }) => {
                    aggregate.turns = aggregate.turns.saturating_add(1);
                    if aggregate.intent.is_empty() {
                        aggregate.intent = prompt.value().clone();
                    }
                    turn_started_at.get_or_insert(segment.recorded_at_unix_ms);
                }
                (HookEventKind::Stop, _) => {
                    if let Some(started) = turn_started_at.take() {
                        aggregate.active_duration_ms = aggregate
                            .active_duration_ms
                            .saturating_add(segment.recorded_at_unix_ms - started);
                    }
                }
                (HookEventKind::SessionEnd, _) => aggregate.complete = true,
                (HookEventKind::PostToolUse | HookEventKind::PostToolUseFailure, payload) => {
                    aggregate.tool_calls = aggregate.tool_calls.saturating_add(1);
                    if let HookPayload::PostToolUse(result) = payload
                        && result.error.is_some()
                    {
                        aggregate.failed_tool_calls = aggregate.failed_tool_calls.saturating_add(1);
                    }
                }
                _ => {}
            }
        }

        // A session that ended mid-turn still did the work up to its last event.
        if let Some(started) = turn_started_at {
            aggregate.active_duration_ms = aggregate
                .active_duration_ms
                .saturating_add(last_event_at - started);
        }

        aggregate.ended_at_unix_ms = last_event_at;
        aggregate.wall_duration_ms = last_event_at - aggregate.started_at_unix_ms;
        aggregate.events = read.accepted;

        if aggregate.retained_bytes > MAX_TRAJECTORY_BYTES as u64 {
            return Err(RecorderError::TooLarge {
                session_key: session_key.to_owned(),
                bytes: aggregate.retained_bytes,
            });
        }

        Ok(aggregate)
    }
}
