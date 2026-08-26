//! Manual grouping and the candidate contract inferred from a group.
//!
//! Grouping is a human act in v1 (see `docs/adr/0001-mvp-boundary.md`): a [`Group`] records which
//! trajectories an operator declared to be the same solved intent, and a [`CandidateContract`]
//! records the typed inputs and outputs that a human confirmed for it.

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use crate::canonical::Digest;
use crate::envelope::Record;
use crate::ids::{GroupId, RepositoryId, TrajectoryId, kind};
use crate::lifecycle::LifecycleState;

/// A human-confirmed set of trajectories that solved the same intent.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct Group {
    /// Repository the group belongs to.
    pub repository_id: RepositoryId,
    /// Short label the operator gave the shared intent.
    pub intent_label: String,
    /// Members, sorted so the manifest digest does not depend on selection order.
    pub members: Vec<TrajectoryId>,
    /// Free-text rationale the operator recorded when confirming the group.
    pub rationale: String,
    /// Digest of the member set, making the manifest immutable once confirmed.
    pub manifest_digest: Digest,
}

impl Record for Group {
    type Kind = kind::Group;
    const SCHEMA: &'static str = "agent_jit.group";
    const VERSION: u32 = 1;
}

/// The type of a candidate parameter or result field.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum ValueType {
    /// A UTF-8 string.
    String,
    /// A signed integer.
    Integer,
    /// A boolean.
    Boolean,
    /// A Git revision such as `HEAD~1`.
    GitRevision,
    /// A repository-relative path.
    RepoPath,
    /// A list of repository-relative paths.
    RepoPathList,
}

/// One typed input or output of a candidate capability.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct ParameterSpec {
    /// Parameter name as it appears in the capability contract.
    pub name: String,
    /// Type of the value.
    pub value_type: ValueType,
    /// Whether the caller must supply the value.
    pub required: bool,
    /// Value observed in every grouped run, when the parameter was constant.
    pub constant_value: Option<String>,
    /// What the parameter means, for the enable card.
    pub description: String,
}

/// Projected value of compiling a candidate, in integers only.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct ProjectedValue {
    /// How many grouped runs the candidate covers.
    pub observed_uses: u32,
    /// Median tool calls per observed run.
    pub median_tool_calls: u32,
    /// Median estimated input tokens per observed run.
    pub median_input_tokens: u64,
    /// Median duration per observed run.
    pub median_duration_ms: i64,
    /// Estimated one-off cost of compiling and validating, in equivalent uses.
    pub compile_cost_uses: u32,
    /// Uses required before the candidate pays for itself.
    pub break_even_uses: u32,
}

/// A candidate capability contract confirmed by a human.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct CandidateContract {
    /// Group the candidate was inferred from.
    pub group_id: GroupId,
    /// Capability name, e.g. `validate_change`.
    pub name: String,
    /// Typed inputs.
    pub inputs: Vec<ParameterSpec>,
    /// Typed outputs.
    pub outputs: Vec<ParameterSpec>,
    /// Preconditions that must hold before the capability may run.
    pub preconditions: Vec<String>,
    /// Projected value used for ranking and the break-even gate.
    pub projected: ProjectedValue,
    /// Lifecycle position of the candidate.
    pub state: LifecycleState,
    /// Whether a human confirmed the inferred contract verbatim.
    pub confirmed_by_human: bool,
}

impl Record for CandidateContract {
    type Kind = kind::Candidate;
    const SCHEMA: &'static str = "agent_jit.candidate_contract";
    const VERSION: u32 = 1;
}
