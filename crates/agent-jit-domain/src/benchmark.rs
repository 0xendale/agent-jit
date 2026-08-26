//! Paired benchmark records.
//!
//! One record is one task run in one condition. Aggregates are computed from these records at
//! report time; a stored aggregate could not be recomputed from raw evidence, and Gate B requires
//! exactly that.

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use crate::envelope::Record;
use crate::ids::{CapabilityId, RepositoryId, kind};
use crate::outcome::CostMetrics;

/// Which arm of the paired benchmark produced a record.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum BenchmarkCondition {
    /// The agent ran with no JIT tool available.
    Baseline,
    /// The agent ran with exactly one enabled capability available.
    Treatment,
}

/// The verdict of the deterministic correctness oracle for one task.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum CorrectnessVerdict {
    /// The task output matched the oracle exactly.
    Correct,
    /// The task output did not match the oracle.
    Incorrect,
    /// The oracle could not be evaluated, which counts as a failure for gating purposes.
    Indeterminate,
}

/// One task, run once, in one condition.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct BenchmarkRecord {
    /// Repository the task ran against.
    pub repository_id: RepositoryId,
    /// Stable task identifier from the pre-frozen task set.
    pub task_id: String,
    /// Which arm produced this record.
    pub condition: BenchmarkCondition,
    /// Position of this record in the counterbalanced order, starting at zero.
    pub order_index: u32,
    /// Commit the task ran against, pinned per task.
    pub head_commit: String,
    /// Explicit model identifier used for the run.
    pub model_id: String,
    /// Measured cost of the run.
    pub metrics: CostMetrics,
    /// Correctness verdict from the deterministic oracle.
    pub correctness: CorrectnessVerdict,
    /// Capability that was eligible during a treatment run, when one was.
    pub capability_id: Option<CapabilityId>,
    /// Whether the capability actually executed, as opposed to being merely available.
    pub capability_executed: bool,
}

impl Record for BenchmarkRecord {
    type Kind = kind::Benchmark;
    const SCHEMA: &'static str = "agent_jit.benchmark_record";
    const VERSION: u32 = 1;
}
