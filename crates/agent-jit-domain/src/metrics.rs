//! Cost metrics and the provenance of every number in them.
//!
//! Phase 0 divides by these values to decide whether the whole project continues, so a number
//! without a stated origin is worse than no number at all. Two rules are enforced by the types
//! rather than by convention:
//!
//! * **absence is not zero.** A session with no observed `Stop` has an *unknown* active duration,
//!   not a duration of zero. [`Measured`] has no `Default`, and `value()` returns [`Option`].
//! * **exact and estimated never merge.** Adding an exact count to an estimated one produces
//!   [`Unknown::MixedMeasurementSources`], because a total that is half-measured and half-guessed
//!   describes nothing.

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use crate::ids::{RepositoryId, SessionId};

/// Why a metric has no value.
#[derive(
    Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize, JsonSchema,
)]
#[serde(rename_all = "snake_case")]
pub enum Unknown {
    /// The session never reported a `Stop`, so no turn could be closed.
    NoStopObserved,
    /// A timestamp went backwards, so an interval could not be trusted.
    BackwardsTimestamp,
    /// The arithmetic would have overflowed.
    Overflow,
    /// No versioned source supplied an exact value.
    NoVersionedSource,
    /// There were no events to measure.
    NoEventsObserved,
    /// Combining these values would have mixed exact and estimated measurements.
    MixedMeasurementSources,
}

impl Unknown {
    /// Stable `snake_case` code.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::NoStopObserved => "no_stop_observed",
            Self::BackwardsTimestamp => "backwards_timestamp",
            Self::Overflow => "overflow",
            Self::NoVersionedSource => "no_versioned_source",
            Self::NoEventsObserved => "no_events_observed",
            Self::MixedMeasurementSources => "mixed_measurement_sources",
        }
    }
}

/// A metric value together with how it was obtained.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(tag = "measurement_source", rename_all = "snake_case")]
pub enum Measured<T> {
    /// Reported by a named, versioned source.
    Exact {
        /// The measured value.
        value: T,
        /// Identifier of the source that reported it, e.g. `claude_code.usage.v1`.
        source: String,
    },
    /// Derived by a named deterministic method.
    Estimated {
        /// The estimated value.
        value: T,
        /// Identifier of the method, e.g. `utf8_bytes_div_4`.
        method: String,
    },
    /// Not available. Deliberately carries no value.
    Unknown {
        /// Why the value is missing.
        reason: Unknown,
    },
}

impl<T: Copy> Measured<T> {
    /// Builds an exact value attributed to `source`.
    #[must_use]
    pub fn exact(value: T, source: &str) -> Self {
        Self::Exact {
            value,
            source: source.to_owned(),
        }
    }

    /// Builds an estimate attributed to `method`.
    #[must_use]
    pub fn estimated(value: T, method: &str) -> Self {
        Self::Estimated {
            value,
            method: method.to_owned(),
        }
    }

    /// Builds a missing value.
    #[must_use]
    pub const fn unknown(reason: Unknown) -> Self {
        Self::Unknown { reason }
    }

    /// Returns the value, or `None` when it is unknown.
    ///
    /// There is deliberately no `unwrap_or(0)` convenience: a caller that wants to treat absence as
    /// zero has to write that down.
    #[must_use]
    pub const fn value(&self) -> Option<T> {
        match self {
            Self::Exact { value, .. } | Self::Estimated { value, .. } => Some(*value),
            Self::Unknown { .. } => None,
        }
    }

    /// Whether this value came from a versioned source.
    #[must_use]
    pub const fn is_exact(&self) -> bool {
        matches!(self, Self::Exact { .. })
    }

    /// Whether this value was derived rather than reported.
    #[must_use]
    pub const fn is_estimated(&self) -> bool {
        matches!(self, Self::Estimated { .. })
    }

    /// Renders the provenance as a stable string, e.g. `estimated:utf8_bytes_div_4`.
    #[must_use]
    pub fn source(&self) -> String {
        match self {
            Self::Exact { source, .. } => format!("exact:{source}"),
            Self::Estimated { method, .. } => format!("estimated:{method}"),
            Self::Unknown { reason } => format!("unknown:{}", reason.as_str()),
        }
    }
}

/// Integer types this crate knows how to add without wrapping.
pub trait CheckedAdd: Sized + Copy {
    /// Adds, returning `None` on overflow.
    fn checked_add_metric(self, other: Self) -> Option<Self>;
}

impl CheckedAdd for u64 {
    fn checked_add_metric(self, other: Self) -> Option<Self> {
        self.checked_add(other)
    }
}

impl CheckedAdd for u32 {
    fn checked_add_metric(self, other: Self) -> Option<Self> {
        self.checked_add(other)
    }
}

impl CheckedAdd for i64 {
    fn checked_add_metric(self, other: Self) -> Option<Self> {
        self.checked_add(other)
    }
}

impl<T: Copy + CheckedAdd> Measured<T> {
    /// Adds two measurements of the same kind and provenance.
    ///
    /// Refuses, as [`Unknown::MixedMeasurementSources`], to combine an exact value with an estimate
    /// or two exact values from different sources: the result would be a total nobody could
    /// attribute. Overflow yields [`Unknown::Overflow`] rather than wrapping.
    #[must_use]
    pub fn checked_add(&self, other: &Self) -> Self {
        match (self, other) {
            (
                Self::Exact { value, source },
                Self::Exact {
                    value: other_value,
                    source: other_source,
                },
            ) if source == other_source => value.checked_add_metric(*other_value).map_or_else(
                || Self::unknown(Unknown::Overflow),
                |sum| Self::exact(sum, source),
            ),
            (
                Self::Estimated { value, method },
                Self::Estimated {
                    value: other_value,
                    method: other_method,
                },
            ) if method == other_method => value.checked_add_metric(*other_value).map_or_else(
                || Self::unknown(Unknown::Overflow),
                |sum| Self::estimated(sum, method),
            ),
            // An unknown operand poisons the sum in either position: a total that silently skipped
            // a value it could not measure would understate the cost.
            (Self::Unknown { reason }, _) | (_, Self::Unknown { reason }) => Self::unknown(*reason),
            _ => Self::unknown(Unknown::MixedMeasurementSources),
        }
    }
}

/// Name of the deterministic token-estimation method.
pub const TOKEN_ESTIMATE_METHOD: &str = "utf8_bytes_div_4";

/// Estimates model input tokens from UTF-8 byte length as `ceil(bytes / 4)`.
///
/// This is an estimate and is always labelled as one. It exists so a corpus recorded without exact
/// usage data still has a *comparable* cost number; it must never be presented as a token count a
/// model actually reported.
#[must_use]
pub const fn estimate_tokens_from_bytes(bytes: u64) -> u64 {
    // Written to avoid overflow at u64::MAX, where bytes + 3 would wrap.
    let whole = bytes / 4;
    if bytes % 4 == 0 { whole } else { whole + 1 }
}

/// How much of the session the recorder failed to keep.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct RecorderHealth {
    /// Payloads that hit the byte cap.
    pub truncated_payloads: u32,
    /// Segments quarantined during recovery.
    pub quarantined_segments: u32,
    /// Duplicate events collapsed during recovery.
    pub duplicates_collapsed: u32,
}

impl RecorderHealth {
    /// Whether anything was lost or altered on the way into the database.
    ///
    /// A lossy trajectory is still usable evidence, but Phase 0 should know it is reading one.
    #[must_use]
    pub const fn is_lossy(&self) -> bool {
        self.truncated_payloads > 0 || self.quarantined_segments > 0
    }
}

/// Where a set of metrics came from.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct MetricProvenance {
    /// What the numbers were computed from. Always `stored_events` in this build.
    pub computed_from: String,
    /// Repository the trajectory belongs to.
    pub repository_id: RepositoryId,
    /// Session the events belong to.
    pub session_id: SessionId,
    /// Commit the trajectory ran against.
    pub head_commit: String,
    /// How many stored events were read.
    pub event_count: u32,
    /// Adapter contract that normalized the events.
    pub adapter_schema: Option<String>,
    /// Agent runtime version.
    pub runtime_version: Option<String>,
    /// Model identifier, absent unless a versioned source supplied it.
    pub model_id: Option<String>,
}

/// Cost of one trajectory.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct TraceMetrics {
    /// Sum of prompt-to-stop intervals: the agent's working time.
    pub active_duration_ms: Measured<i64>,
    /// First-to-last event span, including time the user was idle.
    pub wall_duration_ms: Measured<i64>,
    /// Number of user prompts.
    pub turns: u32,
    /// Tool calls the agent made. Results are not counted: they are the same call, observed later.
    pub agent_tool_calls: u32,
    /// Tool results that reported a nonzero exit status.
    pub failed_tool_calls: u32,
    /// Total retained payload bytes across the session.
    pub observed_bytes: u64,
    /// Deterministic token estimate derived from bytes.
    pub estimated_input_tokens: Measured<u64>,
    /// Exact token count, only when a versioned source supplied one.
    pub exact_input_tokens: Measured<u64>,
    /// What the recorder lost on the way in.
    pub health: RecorderHealth,
    /// Where all of the above came from.
    pub provenance: MetricProvenance,
}
