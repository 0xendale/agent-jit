//! Compiled workflows, replay results, capability versions, and runtime responses.
//!
//! The compiled workflow IR itself is defined by the compiler crate; the records here address it
//! by digest so provenance stays intact even as the IR grows. What matters at the contract level
//! is that a capability version is immutable, carries the fingerprints it was validated against,
//! and answers a runtime query with exactly one of three variants.

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use crate::canonical::Digest;
use crate::envelope::Record;
use crate::ids::{CandidateId, CapabilityId, TrajectoryId, WorkflowId, kind};
use crate::lifecycle::LifecycleState;

/// A compiled typed DAG built from the fixed read-only primitive allowlist.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct Workflow {
    /// Candidate contract this workflow implements.
    pub candidate_id: CandidateId,
    /// Version of the IR grammar the workflow was compiled against.
    pub ir_version: u32,
    /// Digest of the canonical IR document.
    pub ir_digest: Digest,
    /// Primitive identifiers used by the workflow, sorted and deduplicated.
    pub primitives: Vec<String>,
    /// Number of nodes in the DAG, used for cheap sanity checks in reports.
    pub node_count: u32,
}

impl Record for Workflow {
    type Kind = kind::Workflow;
    const SCHEMA: &'static str = "agent_jit.workflow";
    const VERSION: u32 = 1;
}

/// Why a replay did not match.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum ReplayVerdict {
    /// Semantic outputs matched the historical run.
    Match,
    /// Outputs differed; the capability may not be activated.
    Mismatch,
    /// The replay could not run, for example because the sandbox was unavailable.
    NotRun,
}

/// One replay of a compiled workflow against one historical fixture.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct ReplayResult {
    /// Workflow that was replayed.
    pub workflow_id: WorkflowId,
    /// Historical trajectory used as the fixture.
    pub fixture_trajectory_id: TrajectoryId,
    /// Verdict of the comparison.
    pub verdict: ReplayVerdict,
    /// Identifier of the deterministic comparator that produced the verdict.
    pub comparator: String,
    /// Reason text when the verdict is not `match`.
    pub reason: Option<String>,
    /// Digest of the observed output, for provenance.
    pub observed_digest: Option<Digest>,
    /// Digest of the expected output derived from the historical run.
    pub expected_digest: Option<Digest>,
    /// Whether repeated runs produced identical results.
    pub deterministic: bool,
}

impl Record for ReplayResult {
    type Kind = kind::Replay;
    const SCHEMA: &'static str = "agent_jit.replay_result";
    const VERSION: u32 = 1;
}

/// An immutable, approved capability version.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct CapabilityVersion {
    /// Workflow this version executes.
    pub workflow_id: WorkflowId,
    /// Capability name exposed through routing.
    pub name: String,
    /// Monotonic version number within the capability name.
    pub version: u32,
    /// Lifecycle position.
    pub state: LifecycleState,
    /// Digest of the repository and dependency fingerprints this version was validated against.
    pub fingerprint_digest: Digest,
    /// Digests of the replay results that justified approval, in a stable order.
    pub replay_digests: Vec<Digest>,
    /// Operator identity that approved the version, as recorded locally.
    pub approved_by: Option<String>,
}

impl Record for CapabilityVersion {
    type Kind = kind::Capability;
    const SCHEMA: &'static str = "agent_jit.capability_version";
    const VERSION: u32 = 1;
}

/// Why the runtime refused to execute.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum FallbackReason {
    /// A precondition of the capability did not hold.
    PreconditionFailed,
    /// A dependency fingerprint no longer matches what the capability was validated against.
    StaleFingerprint,
    /// The capability is not in a routable state.
    NotRoutable,
    /// The sandbox was unavailable or failed its integrity check.
    SandboxUnavailable,
    /// Execution exceeded its deadline.
    Timeout,
    /// Execution produced more output than the cap allows.
    OutputCapExceeded,
    /// The produced output failed its own contract validation.
    OutputInvalid,
    /// Execution failed for a reason the runtime does not classify further.
    ExecutionFailed,
}

/// A capability result document.
///
/// This is the one place a dynamic JSON value is legitimate: the shape is declared by the
/// capability's own output contract, which is validated before the value is ever constructed.
/// It is a schema boundary, not an escape hatch for untyped internals.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(transparent)]
pub struct CapabilityOutput(pub serde_json::Value);

/// The three — and only three — answers the runtime may give.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(tag = "result", rename_all = "snake_case")]
pub enum RuntimeResponse {
    /// A capability ran and produced a validated result.
    Executed {
        /// Capability version that ran.
        capability_id: CapabilityId,
        /// Digest of the canonical result document.
        output_digest: Digest,
        /// Canonical result document, as produced by the capability.
        output: CapabilityOutput,
    },
    /// No capability matched the query; the agent should proceed normally.
    NotFound,
    /// A capability matched but could not safely run; the agent must use its original path.
    FallbackRequired {
        /// Why execution was refused.
        reason: FallbackReason,
        /// Human-readable detail for the agent's log.
        detail: String,
    },
}

/// One runtime invocation and its response.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct Invocation {
    /// Capability that was considered, when one matched.
    pub capability_id: Option<CapabilityId>,
    /// The intent text the agent supplied.
    pub query: String,
    /// The response returned to the agent.
    pub response: RuntimeResponse,
    /// Wall-clock duration of the invocation. Volatile: excluded from the digest.
    pub duration_ms: i64,
}

impl Record for Invocation {
    type Kind = kind::Invocation;
    const SCHEMA: &'static str = "agent_jit.invocation";
    const VERSION: u32 = 1;
    const VOLATILE: &'static [&'static str] = &["/duration_ms"];
}
