//! Append-only manual outcome annotations.

use std::path::{Component, Path};

use schemars::JsonSchema;
use serde::{Deserialize, Deserializer, Serialize};

use crate::envelope::Record;
use crate::ids::{TrajectoryId, kind};

/// Maximum UTF-8 bytes retained for annotation actor identity.
pub const MAX_ANNOTATION_ACTOR_BYTES: usize = 256;
/// Maximum UTF-8 bytes retained for annotation rationale.
pub const MAX_ANNOTATION_RATIONALE_BYTES: usize = 4_096;
/// Maximum evidence references retained by one annotation.
pub const MAX_EVIDENCE_REFS: usize = 16;

/// Manual outcome vocabulary, separate from legacy [`crate::outcome::OutcomeStatus`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum AnnotationStatus {
    /// Human review confirmed success.
    Succeeded,
    /// Human review confirmed failure.
    Failed,
    /// Work stopped before reaching a conclusion.
    Abandoned,
    /// Available evidence cannot determine an outcome.
    Unknown,
}

/// Nonzero, monotonically increasing annotation revision.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize)]
#[serde(transparent)]
pub struct AnnotationRevision(u32);

impl AnnotationRevision {
    /// Validates a revision number.
    ///
    /// # Errors
    ///
    /// Returns [`OutcomeAnnotationError::RevisionZero`] when `value` is zero.
    pub const fn new(value: u32) -> Result<Self, OutcomeAnnotationError> {
        if value == 0 {
            Err(OutcomeAnnotationError::RevisionZero)
        } else {
            Ok(Self(value))
        }
    }

    /// Returns the integer revision.
    #[must_use]
    pub const fn get(self) -> u32 {
        self.0
    }

    /// Advances one revision using checked arithmetic.
    ///
    /// # Errors
    ///
    /// Returns [`OutcomeAnnotationError::RevisionOverflow`] at [`u32::MAX`].
    pub const fn next(self) -> Result<Self, OutcomeAnnotationError> {
        match self.0.checked_add(1) {
            Some(value) => Ok(Self(value)),
            None => Err(OutcomeAnnotationError::RevisionOverflow),
        }
    }
}

impl<'de> Deserialize<'de> for AnnotationRevision {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let value = u32::deserialize(deserializer)?;
        Self::new(value).map_err(serde::de::Error::custom)
    }
}

/// Validated repository-relative path naming annotation evidence.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Serialize)]
#[serde(transparent)]
pub struct EvidenceRef(String);

impl EvidenceRef {
    /// Validates and stores one evidence reference.
    ///
    /// # Errors
    ///
    /// Returns an error for empty, absolute, or traversing paths.
    pub fn new(value: &str) -> Result<Self, EvidenceRefError> {
        if value.is_empty()
            || value.contains('\0')
            || value.contains('\\')
            || Path::new(value).is_absolute()
            || value
                .split('/')
                .any(|segment| segment.is_empty() || segment == "." || segment == "..")
            || Path::new(value)
                .components()
                .any(|part| !matches!(part, Component::Normal(_)))
        {
            return Err(EvidenceRefError);
        }
        Ok(Self(value.to_owned()))
    }

    /// Returns the validated relative path.
    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl<'de> Deserialize<'de> for EvidenceRef {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let value = String::deserialize(deserializer)?;
        Self::new(&value).map_err(serde::de::Error::custom)
    }
}

/// An invalid evidence reference.
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
#[error("evidence reference must be a nonempty normalized relative path")]
pub struct EvidenceRefError;

/// One immutable human outcome decision.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct OutcomeAnnotation {
    /// Trajectory reviewed by the operator.
    trajectory_id: TrajectoryId,
    /// Monotonic revision within the trajectory.
    revision: AnnotationRevision,
    /// Human-confirmed outcome.
    status: AnnotationStatus,
    /// Local identity of the operator.
    actor: String,
    /// Wall-clock time of annotation.
    annotated_at_unix_ms: i64,
    /// Reason supporting the decision.
    rationale: String,
    /// Bounded repository-relative evidence references.
    evidence: Vec<EvidenceRef>,
}

impl OutcomeAnnotation {
    /// Validates a complete annotation body.
    ///
    /// # Errors
    ///
    /// Returns an actor, rationale, or evidence limit error when input is invalid.
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        trajectory_id: TrajectoryId,
        revision: AnnotationRevision,
        status: AnnotationStatus,
        actor: &str,
        annotated_at_unix_ms: i64,
        rationale: &str,
        evidence: Vec<EvidenceRef>,
    ) -> Result<Self, OutcomeAnnotationError> {
        validate_text(actor, MAX_ANNOTATION_ACTOR_BYTES, true)?;
        validate_text(rationale, MAX_ANNOTATION_RATIONALE_BYTES, false)?;
        validate_evidence(&evidence)?;
        Ok(Self {
            trajectory_id,
            revision,
            status,
            actor: actor.to_owned(),
            annotated_at_unix_ms,
            rationale: rationale.to_owned(),
            evidence,
        })
    }

    /// Replaces evidence while preserving all other immutable body fields.
    ///
    /// # Errors
    ///
    /// Returns [`OutcomeAnnotationError::TooManyEvidence`] above the evidence limit.
    pub fn with_evidence(
        mut self,
        evidence: Vec<EvidenceRef>,
    ) -> Result<Self, OutcomeAnnotationError> {
        validate_evidence(&evidence)?;
        self.evidence = evidence;
        Ok(self)
    }

    /// Returns the reviewed trajectory.
    #[must_use]
    pub const fn trajectory_id(&self) -> TrajectoryId {
        self.trajectory_id
    }

    /// Returns the annotation revision.
    #[must_use]
    pub const fn revision(&self) -> AnnotationRevision {
        self.revision
    }

    /// Returns the confirmed status.
    #[must_use]
    pub const fn status(&self) -> AnnotationStatus {
        self.status
    }

    /// Returns the operator identity.
    #[must_use]
    pub fn actor(&self) -> &str {
        &self.actor
    }

    /// Returns the annotation timestamp.
    #[must_use]
    pub const fn annotated_at_unix_ms(&self) -> i64 {
        self.annotated_at_unix_ms
    }

    /// Returns the decision rationale.
    #[must_use]
    pub fn rationale(&self) -> &str {
        &self.rationale
    }

    /// Returns validated evidence references.
    #[must_use]
    pub fn evidence(&self) -> &[EvidenceRef] {
        &self.evidence
    }
}

impl Record for OutcomeAnnotation {
    type Kind = kind::OutcomeAnnotation;
    const SCHEMA: &'static str = "agent_jit.outcome_annotation";
    const VERSION: u32 = 1;
    const VOLATILE: &'static [&'static str] = &["/annotated_at_unix_ms"];
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct RawOutcomeAnnotation {
    trajectory_id: TrajectoryId,
    revision: AnnotationRevision,
    status: AnnotationStatus,
    actor: String,
    annotated_at_unix_ms: i64,
    rationale: String,
    evidence: Vec<EvidenceRef>,
}

impl<'de> Deserialize<'de> for OutcomeAnnotation {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let raw = RawOutcomeAnnotation::deserialize(deserializer)?;
        Self::new(
            raw.trajectory_id,
            raw.revision,
            raw.status,
            &raw.actor,
            raw.annotated_at_unix_ms,
            &raw.rationale,
            raw.evidence,
        )
        .map_err(serde::de::Error::custom)
    }
}

/// Why an annotation body was refused.
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
pub enum OutcomeAnnotationError {
    /// Revision zero is not part of the append-only sequence.
    #[error("annotation revision must be nonzero")]
    RevisionZero,
    /// No next revision exists.
    #[error("annotation revision overflow")]
    RevisionOverflow,
    /// Actor is empty.
    #[error("annotation actor must not be empty")]
    ActorEmpty,
    /// Actor exceeds its byte limit.
    #[error("annotation actor is too long")]
    ActorTooLong,
    /// Rationale is empty.
    #[error("annotation rationale must not be empty")]
    RationaleEmpty,
    /// Rationale exceeds its byte limit.
    #[error("annotation rationale is too long")]
    RationaleTooLong,
    /// Evidence count exceeds its limit.
    #[error("annotation has too many evidence references")]
    TooManyEvidence,
}

impl OutcomeAnnotationError {
    /// Stable machine-readable code.
    #[must_use]
    pub const fn code(self) -> &'static str {
        match self {
            Self::RevisionZero => "outcome_annotation_revision_zero",
            Self::RevisionOverflow => "outcome_annotation_revision_overflow",
            Self::ActorEmpty => "outcome_annotation_actor_empty",
            Self::ActorTooLong => "outcome_annotation_actor_too_long",
            Self::RationaleEmpty => "outcome_annotation_rationale_empty",
            Self::RationaleTooLong => "outcome_annotation_rationale_too_long",
            Self::TooManyEvidence => "outcome_annotation_too_many_evidence",
        }
    }
}

fn validate_text(value: &str, max_bytes: usize, actor: bool) -> Result<(), OutcomeAnnotationError> {
    if value.is_empty() {
        return Err(if actor {
            OutcomeAnnotationError::ActorEmpty
        } else {
            OutcomeAnnotationError::RationaleEmpty
        });
    }
    if value.len() > max_bytes {
        return Err(if actor {
            OutcomeAnnotationError::ActorTooLong
        } else {
            OutcomeAnnotationError::RationaleTooLong
        });
    }
    Ok(())
}

fn validate_evidence(evidence: &[EvidenceRef]) -> Result<(), OutcomeAnnotationError> {
    if evidence.len() > MAX_EVIDENCE_REFS {
        Err(OutcomeAnnotationError::TooManyEvidence)
    } else {
        Ok(())
    }
}
