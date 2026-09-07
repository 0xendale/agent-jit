//! Recorded outcomes and the integer-only cost metrics attached to a run.

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use crate::envelope::Record;
use crate::ids::{TrajectoryId, kind};

pub use crate::outcome_annotation::{
    AnnotationRevision, AnnotationStatus, EvidenceRef, MAX_ANNOTATION_ACTOR_BYTES,
    MAX_ANNOTATION_RATIONALE_BYTES, MAX_EVIDENCE_REFS, OutcomeAnnotation, OutcomeAnnotationError,
};

/// How a recorded run ended.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum OutcomeStatus {
    /// The intent was solved and the operator confirmed it.
    Solved,
    /// The run finished but did not solve the intent.
    Failed,
    /// The run was abandoned before reaching a conclusion.
    Abandoned,
}

/// Integer cost metrics for one run.
///
/// Every value is an integer: canonical JSON rejects floats, because a digest that depends on
/// float formatting is not reproducible. Ratios are computed at report time from these counters,
/// never stored.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct CostMetrics {
    /// Number of tool calls the agent made.
    pub tool_calls: u32,
    /// Number of agent turns.
    pub agent_turns: u32,
    /// Estimated input tokens consumed.
    pub estimated_input_tokens: u64,
    /// Estimated output tokens produced.
    pub estimated_output_tokens: u64,
    /// Wall-clock duration of the run.
    pub duration_ms: i64,
}

/// The recorded conclusion of a trajectory.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct Outcome {
    /// Trajectory this outcome concludes.
    pub trajectory_id: TrajectoryId,
    /// How the run ended.
    pub status: OutcomeStatus,
    /// Operator note explaining the status; required for anything but `solved`.
    pub note: Option<String>,
    /// Cost of the run.
    pub metrics: CostMetrics,
    /// Whether a human confirmed the status rather than it being inferred.
    pub confirmed_by_human: bool,
}

impl Record for Outcome {
    type Kind = kind::Outcome;
    const SCHEMA: &'static str = "agent_jit.outcome";
    const VERSION: u32 = 1;
}
