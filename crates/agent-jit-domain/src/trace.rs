//! Recorded observations: repositories, sessions, events, and normalized trajectories.

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use crate::canonical::Digest;
use crate::envelope::Record;
use crate::ids::{EventId, OutcomeId, RepositoryId, SessionId, kind};

/// A Git repository the recorder observes.
///
/// Identity is derived from the Git common directory, so every worktree of one repository maps to
/// one record while each trajectory still records the worktree it ran in.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct Repository {
    /// Absolute path of the Git common directory that defines this repository's identity.
    pub git_common_dir: String,
    /// Digest of the canonical identity inputs, stable across worktrees and checkouts.
    pub identity_digest: Digest,
    /// Human-facing name, used only in reports.
    pub label: String,
}

impl Record for Repository {
    type Kind = kind::Repository;
    const SCHEMA: &'static str = "agent_jit.repository";
    const VERSION: u32 = 1;
}

/// The agent runtime a session was recorded from.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum Runtime {
    /// Claude Code, the only runtime instrumented in v1.
    ClaudeCode,
}

/// One agent session against one repository worktree.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct Session {
    /// Repository the session ran against.
    pub repository_id: RepositoryId,
    /// Absolute path of the worktree the session ran in.
    pub worktree_root: String,
    /// Commit checked out when the session started.
    pub head_commit: String,
    /// Runtime that produced the session.
    pub runtime: Runtime,
    /// Exact runtime version, e.g. the Claude Code CLI version.
    pub runtime_version: String,
    /// Explicit model identifier; never inferred or defaulted.
    pub model_id: String,
    /// Version of the recorder plugin that captured the session.
    pub recorder_version: String,
    /// Wall-clock start. Volatile: excluded from the digest.
    pub started_at_unix_ms: i64,
    /// Wall-clock end, absent while the session is open. Volatile: excluded from the digest.
    pub ended_at_unix_ms: Option<i64>,
}

impl Record for Session {
    type Kind = kind::Session;
    const SCHEMA: &'static str = "agent_jit.session";
    const VERSION: u32 = 1;
    const VOLATILE: &'static [&'static str] = &["/started_at_unix_ms", "/ended_at_unix_ms"];
}

/// What kind of observation an event carries.
///
/// Turn boundaries are their own kinds rather than being folded into [`Self::AgentMessage`].
/// Active duration is a sum of prompt-to-stop intervals computed from *stored* events, so a Stop
/// that is indistinguishable from an ordinary message once persisted would make Phase 0's primary
/// denominator impossible to recompute from the database.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum EventKind {
    /// The user stated an intent. Opens a turn.
    UserPrompt,
    /// The agent invoked a tool.
    ToolCall,
    /// A tool returned a result.
    ToolResult,
    /// The agent produced a message.
    AgentMessage,
    /// A session began.
    SessionStart,
    /// The agent stopped. Closes a turn.
    Stop,
    /// A session ended.
    SessionEnd,
}

impl EventKind {
    /// Whether this kind marks a session or turn boundary rather than work inside a turn.
    #[must_use]
    pub const fn is_turn_boundary(self) -> bool {
        matches!(self, Self::SessionStart | Self::Stop | Self::SessionEnd)
    }

    /// Whether this kind is an agent-visible tool call.
    ///
    /// A tool *call* is what cost the agent a round trip; the matching result is the same call
    /// observed from the other side, and counting both would double the number Phase 0 divides by.
    #[must_use]
    pub const fn is_agent_tool_call(self) -> bool {
        matches!(self, Self::ToolCall)
    }
}

/// One recorded event inside a session.
///
/// Payloads are redacted and bounded before they reach this type: the event stores digests and
/// byte counts rather than raw output, so a trace can be compared without retaining tool output
/// verbatim.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct Event {
    /// Session the event belongs to.
    pub session_id: SessionId,
    /// Monotonic position within the session, starting at zero.
    pub sequence: u32,
    /// Kind of observation.
    pub kind: EventKind,
    /// Tool name for tool events, absent otherwise.
    pub tool_name: Option<String>,
    /// Redacted argument vector, one element per argument; never a shell string.
    #[serde(default)]
    pub argv: Vec<String>,
    /// Process exit status for tool results that ran a process.
    pub exit_code: Option<i32>,
    /// Digest of the redacted, bounded payload.
    pub payload_digest: Option<Digest>,
    /// Size of the payload before bounding, in bytes.
    pub payload_bytes: u64,
    /// Whether the payload was truncated to respect the output cap.
    pub payload_truncated: bool,
    /// Wall-clock start. Volatile: excluded from the digest.
    pub started_at_unix_ms: i64,
    /// Observed duration. Volatile: excluded from the digest.
    pub duration_ms: i64,
}

impl Record for Event {
    type Kind = kind::Event;
    const SCHEMA: &'static str = "agent_jit.event";
    const VERSION: u32 = 1;
    const VOLATILE: &'static [&'static str] = &["/started_at_unix_ms", "/duration_ms"];
}

/// A normalized run: one stated intent, the events that served it, and its outcome.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct Trajectory {
    /// Session the trajectory was extracted from.
    pub session_id: SessionId,
    /// Repository the trajectory ran against.
    pub repository_id: RepositoryId,
    /// Commit checked out while the trajectory ran.
    pub head_commit: String,
    /// The intent this run solved, as stated by the user or annotated by the operator.
    pub intent: String,
    /// Events belonging to this trajectory, in observed order.
    pub events: Vec<EventId>,
    /// Recorded outcome, absent until the run is annotated.
    pub outcome_id: Option<OutcomeId>,
    /// Wall-clock start. Volatile: excluded from the digest.
    pub started_at_unix_ms: i64,
    /// Observed duration. Volatile: excluded from the digest.
    pub duration_ms: i64,
}

impl Record for Trajectory {
    type Kind = kind::Trajectory;
    const SCHEMA: &'static str = "agent_jit.trajectory";
    const VERSION: u32 = 1;
    const VOLATILE: &'static [&'static str] = &["/started_at_unix_ms", "/duration_ms"];
}

impl Trajectory {
    /// A deterministic trajectory used by contract tests and schema snapshots.
    ///
    /// It lives beside the type so that a field added without updating the fixtures fails to
    /// compile rather than silently weakening the tests.
    #[must_use]
    pub fn sample() -> Self {
        Self {
            session_id: Self::sample_session_id(),
            repository_id: Self::sample_repository_id(),
            head_commit: "0000000000000000000000000000000000000000".to_owned(),
            intent: "validate this change the way this repository expects".to_owned(),
            events: Vec::new(),
            outcome_id: None,
            started_at_unix_ms: 1_756_000_000_000,
            duration_ms: 42_000,
        }
    }

    fn sample_session_id() -> SessionId {
        SessionId::from_body("01J0000000000000000000000S").unwrap_or_else(|_| {
            unreachable!("the sample identifier body is a compile-time constant")
        })
    }

    fn sample_repository_id() -> RepositoryId {
        RepositoryId::from_body("01J0000000000000000000000R").unwrap_or_else(|_| {
            unreachable!("the sample identifier body is a compile-time constant")
        })
    }
}
