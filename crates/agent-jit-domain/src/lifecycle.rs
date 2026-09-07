//! Candidate and capability lifecycle.
//!
//! `observed → proposed → validating → approved → active → degraded/retired` is the only path a
//! capability may take. Nothing in the product may activate a capability that has not been
//! validated and explicitly approved, so the legal transitions live here as data rather than as
//! scattered `if` statements.

use std::fmt;
use std::str::FromStr;

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

/// A position in the capability lifecycle.
#[derive(
    Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize, JsonSchema,
)]
#[serde(rename_all = "snake_case")]
pub enum LifecycleState {
    /// Runs have been recorded but nothing has been grouped yet.
    Observed,
    /// A human grouped runs and a candidate contract exists.
    Proposed,
    /// The candidate is compiled and being replayed against historical fixtures.
    Validating,
    /// Replay passed and the evidence has been accepted by a human.
    Approved,
    /// The capability is enabled and eligible for routing.
    Active,
    /// The capability failed a runtime check and is temporarily ineligible.
    Degraded,
    /// The capability is permanently out of service.
    Retired,
}

impl LifecycleState {
    /// Every state, in lifecycle order.
    pub const ALL: [Self; 7] = [
        Self::Observed,
        Self::Proposed,
        Self::Validating,
        Self::Approved,
        Self::Active,
        Self::Degraded,
        Self::Retired,
    ];

    /// Returns the `snake_case` name of the state.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Observed => "observed",
            Self::Proposed => "proposed",
            Self::Validating => "validating",
            Self::Approved => "approved",
            Self::Active => "active",
            Self::Degraded => "degraded",
            Self::Retired => "retired",
        }
    }

    /// Returns the states reachable from `self` in one legal transition.
    #[must_use]
    pub const fn successors(self) -> &'static [Self] {
        match self {
            Self::Observed => &[Self::Proposed, Self::Retired],
            Self::Proposed => &[Self::Validating, Self::Retired],
            Self::Validating => &[Self::Approved, Self::Retired],
            // An approved capability is enabled explicitly; a degraded one may recover the same way.
            Self::Approved | Self::Degraded => &[Self::Active, Self::Retired],
            Self::Active => &[Self::Degraded, Self::Retired],
            Self::Retired => &[],
        }
    }

    /// Moves to `next`, or reports that the move is not part of the lifecycle.
    ///
    /// # Errors
    ///
    /// Returns [`LifecycleError::IllegalTransition`] when `next` is not a legal successor.
    pub fn transition_to(self, next: Self) -> Result<Self, LifecycleError> {
        if self.successors().contains(&next) {
            Ok(next)
        } else {
            Err(LifecycleError::IllegalTransition {
                from: self,
                to: next,
            })
        }
    }

    /// Whether a capability in this state may be routed to at runtime.
    #[must_use]
    pub const fn is_routable(self) -> bool {
        matches!(self, Self::Active)
    }
}

impl fmt::Display for LifecycleState {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

impl FromStr for LifecycleState {
    type Err = LifecycleError;

    fn from_str(text: &str) -> Result<Self, Self::Err> {
        Self::ALL
            .into_iter()
            .find(|state| state.as_str() == text)
            .ok_or_else(|| LifecycleError::UnknownState {
                found: text.to_owned(),
            })
    }
}

/// Why a lifecycle operation was refused.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum LifecycleError {
    /// The requested transition is not part of the lifecycle.
    #[error("`{from}` may not transition to `{to}`")]
    IllegalTransition {
        /// State the record is in.
        from: LifecycleState,
        /// State that was requested.
        to: LifecycleState,
    },
    /// The stored state name is not one this binary knows.
    #[error("`{found}` is not a known lifecycle state")]
    UnknownState {
        /// The rejected text.
        found: String,
    },
}

impl LifecycleError {
    /// Stable machine-readable code.
    #[must_use]
    pub const fn code(&self) -> &'static str {
        match self {
            Self::IllegalTransition { .. } => "lifecycle_invalid_transition",
            Self::UnknownState { .. } => "lifecycle_unknown_state",
        }
    }
}
