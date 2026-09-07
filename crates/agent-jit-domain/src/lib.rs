//! Typed domain contracts shared by every agent-jit component.
//!
//! This crate owns the vocabulary of the product: branded identifiers, versioned record envelopes,
//! the canonical JSON form used for every digest, and the lifecycle rules that downstream crates
//! are not allowed to reinterpret. It deliberately depends on nothing from the rest of the
//! workspace so the contracts stay the single source of truth.

pub mod benchmark;
pub mod candidate;
pub mod canonical;
pub mod capability;
pub mod envelope;
pub mod export;
pub mod fingerprint;
pub mod ids;
pub mod lifecycle;
pub mod metrics;
pub mod outcome;
mod outcome_annotation;
mod outcome_annotation_schema;
pub mod redaction;
pub mod schema;
pub mod trace;
