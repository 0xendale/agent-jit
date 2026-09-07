//! Adapters from agent runtimes into the product's normalized events.
//!
//! There is exactly one adapter in v1 (see `docs/adr/0002-claude-code-first-runtime.md`). The
//! module exists so that a second runtime, if it is ever justified, has an obvious place to live
//! without the recorder learning two vocabularies.

pub mod claude_history;
pub mod claude_hooks;
