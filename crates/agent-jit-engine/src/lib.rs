//! Recorder, Phase 0, compiler, replay, and runtime engine for agent-jit.
//!
//! The engine owns everything that observes or acts: discovering repositories, computing
//! fingerprints, running processes, and — once Phase 0 unlocks it — compiling and replaying
//! capabilities. It depends on `agent-jit-domain` for contracts and never reinterprets them.

pub mod adapters;
pub mod fingerprint;
pub mod host;
pub mod normalize;
pub mod process;
pub mod recorder;
pub mod repository;
pub mod spool;
