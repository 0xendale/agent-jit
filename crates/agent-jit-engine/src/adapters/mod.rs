//! Adapters from agent runtimes into the product's normalized events.
//!
//! Claude Code was the first instrumented runtime (`docs/adr/0002-claude-code-first-runtime.md`);
//! `OpenCode` is the second (`docs/adr/0009-opencode-second-runtime.md`). Every adapter normalizes
//! into the one shared event model in [`claude_hooks`], so the recorder never learns a second
//! vocabulary per runtime.

pub mod claude_history;
pub mod claude_hooks;
pub mod opencode_hooks;
