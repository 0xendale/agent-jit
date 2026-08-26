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
pub mod fingerprint;
pub mod ids;
pub mod lifecycle;
pub mod outcome;
pub mod schema;
pub mod trace;
