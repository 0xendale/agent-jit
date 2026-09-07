# ADR 0007 — Exactly one MCP tool: `jit.query`

- Status: accepted
- Date: 2026-08-26

## Context

Exposing one MCP tool per compiled capability would grow the model's tool schema with every
approval, spending the context budget the product is supposed to save, and would push routing
decisions into the model.

## Decision

The plugin registers exactly one stdio MCP tool, `jit.query`, with a fixed input schema. Its result
is exactly one of three typed variants: `executed`, `not_found`, or `fallback_required`.

## Consequences

- Routing is deterministic and lives in Rust; the model asks a question rather than picking a tool.
- A match never bypasses preconditions, fingerprints, sandbox checks, or output validation — any
  failure downgrades to `fallback_required` rather than returning a partial success.
- Capability growth costs no additional tool schema, so usage-based retirement stays cheap.
