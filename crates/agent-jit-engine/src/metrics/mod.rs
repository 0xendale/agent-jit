//! Trace cost metrics, derived from stored events.
//!
//! Everything here is computed from the rows the recorder wrote, never from in-memory state that
//! only the recorder saw. That is the point: a Phase 0 report must be recomputable from the
//! database by someone who does not trust the process that produced it.
//!
//! The metric that decides Phase 0 is **active duration** — the sum of prompt-to-stop intervals.
//! Time between a `Stop` and the next prompt is the human thinking, and charging the agent for it
//! would make every capability look better than it is.

use agent_jit_domain::envelope::Envelope;
use agent_jit_domain::ids::{RepositoryId, SessionId};
use agent_jit_domain::metrics::{
    Measured, MetricProvenance, RecorderHealth, TOKEN_ESTIMATE_METHOD, TraceMetrics, Unknown,
    estimate_tokens_from_bytes,
};
use agent_jit_domain::trace::{Event, EventKind};

/// Source label for values read directly off stored events.
const STORED_EVENTS: &str = "stored_events";

/// What the caller must supply alongside the events themselves.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MetricInputs {
    /// Repository the trajectory belongs to.
    pub repository_id: RepositoryId,
    /// Session the events belong to.
    pub session_id: SessionId,
    /// Commit the trajectory ran against.
    pub head_commit: String,
    /// Adapter contract that normalized the events, when known.
    pub adapter_schema: Option<String>,
    /// Agent runtime version, when known.
    pub runtime_version: Option<String>,
    /// Model identifier, when a versioned source supplied one.
    pub model_id: Option<String>,
    /// Exact input tokens and the versioned source that reported them, when available.
    pub exact_input_tokens: Option<(u64, String)>,
    /// Segments recovery had to quarantine for this session.
    pub quarantined_segments: u32,
    /// Duplicate events recovery collapsed for this session.
    pub duplicates_collapsed: u32,
}

/// Why metrics could not be computed.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum MetricsError {
    /// There were no stored events, so there is nothing to measure.
    #[error("no stored events for session `{session_id}`")]
    NoEvents {
        /// The session that was asked for.
        session_id: String,
    },
}

impl MetricsError {
    /// Stable machine-readable code.
    #[must_use]
    pub const fn code(&self) -> &'static str {
        match self {
            Self::NoEvents { .. } => "metrics_no_events",
        }
    }
}

/// Computes one trajectory's metrics from its stored events.
///
/// # Errors
///
/// Returns [`MetricsError::NoEvents`] when there is nothing to measure. An empty trajectory is not
/// a zero-cost trajectory, so it is refused rather than reported as zero.
pub fn compute_trace_metrics(
    events: &[Envelope<Event>],
    inputs: &MetricInputs,
) -> Result<TraceMetrics, MetricsError> {
    if events.is_empty() {
        return Err(MetricsError::NoEvents {
            session_id: inputs.session_id.to_string(),
        });
    }

    // Stored sequence is the authority on order; a query may return rows in any order.
    let mut ordered: Vec<&Envelope<Event>> = events.iter().collect();
    ordered.sort_by_key(|event| event.body().sequence);

    let counts = count(&ordered);
    let active_duration_ms = active_duration(&ordered);
    let wall_duration_ms = wall_duration(&ordered);

    let observed_bytes = ordered.iter().try_fold(0_u64, |total, event| {
        total.checked_add(event.body().payload_bytes)
    });

    let estimated_input_tokens = observed_bytes.map_or_else(
        || Measured::unknown(Unknown::Overflow),
        |bytes| Measured::estimated(estimate_tokens_from_bytes(bytes), TOKEN_ESTIMATE_METHOD),
    );

    let exact_input_tokens = inputs.exact_input_tokens.as_ref().map_or_else(
        || Measured::unknown(Unknown::NoVersionedSource),
        |(value, source)| Measured::exact(*value, source),
    );

    Ok(TraceMetrics {
        active_duration_ms,
        wall_duration_ms,
        turns: counts.turns,
        agent_tool_calls: counts.agent_tool_calls,
        failed_tool_calls: counts.failed_tool_calls,
        observed_bytes: observed_bytes.unwrap_or(u64::MAX),
        estimated_input_tokens,
        exact_input_tokens,
        health: RecorderHealth {
            truncated_payloads: counts.truncated_payloads,
            quarantined_segments: inputs.quarantined_segments,
            duplicates_collapsed: inputs.duplicates_collapsed,
        },
        provenance: MetricProvenance {
            computed_from: STORED_EVENTS.to_owned(),
            repository_id: inputs.repository_id,
            session_id: inputs.session_id,
            head_commit: inputs.head_commit.clone(),
            event_count: u32::try_from(ordered.len()).unwrap_or(u32::MAX),
            adapter_schema: inputs.adapter_schema.clone(),
            runtime_version: inputs.runtime_version.clone(),
            model_id: inputs.model_id.clone(),
        },
    })
}

/// Simple tallies taken in one pass.
struct Counts {
    turns: u32,
    agent_tool_calls: u32,
    failed_tool_calls: u32,
    truncated_payloads: u32,
}

/// Counts the events by kind.
fn count(ordered: &[&Envelope<Event>]) -> Counts {
    let mut counts = Counts {
        turns: 0,
        agent_tool_calls: 0,
        failed_tool_calls: 0,
        truncated_payloads: 0,
    };

    for event in ordered {
        let body = event.body();
        match body.kind {
            EventKind::UserPrompt => counts.turns = counts.turns.saturating_add(1),
            kind if kind.is_agent_tool_call() => {
                counts.agent_tool_calls = counts.agent_tool_calls.saturating_add(1);
            }
            EventKind::ToolResult => {
                if body.exit_code.is_some_and(|code| code != 0) {
                    counts.failed_tool_calls = counts.failed_tool_calls.saturating_add(1);
                }
            }
            _ => {}
        }
        if body.payload_truncated {
            counts.truncated_payloads = counts.truncated_payloads.saturating_add(1);
        }
    }

    counts
}

/// Sums non-overlapping prompt-to-stop intervals.
///
/// A turn opens at the first prompt after a close and shuts at the next `Stop`. Consecutive
/// prompts do not open a second interval — the agent was already working — and a `Stop` with no
/// open turn closes nothing. Both rules exist so the same wall-clock second can never be counted
/// twice.
fn active_duration(ordered: &[&Envelope<Event>]) -> Measured<i64> {
    let mut total = 0_i64;
    let mut turn_started_at: Option<i64> = None;
    let mut closed_any = false;

    for event in ordered {
        let body = event.body();
        match body.kind {
            EventKind::UserPrompt => {
                turn_started_at.get_or_insert(body.started_at_unix_ms);
            }
            EventKind::Stop => {
                let Some(started) = turn_started_at.take() else {
                    continue;
                };
                let Some(interval) = body.started_at_unix_ms.checked_sub(started) else {
                    return Measured::unknown(Unknown::Overflow);
                };
                if interval < 0 {
                    return Measured::unknown(Unknown::BackwardsTimestamp);
                }
                let Some(sum) = total.checked_add(interval) else {
                    return Measured::unknown(Unknown::Overflow);
                };
                total = sum;
                closed_any = true;
            }
            _ => {}
        }
    }

    if !closed_any {
        // An open session has done work, but how much is not yet knowable.
        return Measured::unknown(Unknown::NoStopObserved);
    }
    Measured::exact(total, STORED_EVENTS)
}

/// First-to-last event span.
fn wall_duration(ordered: &[&Envelope<Event>]) -> Measured<i64> {
    let (Some(first), Some(last)) = (ordered.first(), ordered.last()) else {
        return Measured::unknown(Unknown::NoEventsObserved);
    };

    let Some(span) = last
        .body()
        .started_at_unix_ms
        .checked_sub(first.body().started_at_unix_ms)
    else {
        return Measured::unknown(Unknown::Overflow);
    };
    if span < 0 {
        return Measured::unknown(Unknown::BackwardsTimestamp);
    }
    Measured::exact(span, STORED_EVENTS)
}
