# ADR 0002 — Claude Code is the first (and only) instrumented runtime

- Status: accepted
- Date: 2026-08-26

## Context

The spec's open questions ask which runtime gives the cleanest first integration. Options were
Claude Code, Codex, and a generic tool-call proxy. A proxy would require reimplementing transport
and session semantics before any product question could be answered.

## Decision

Instrument Claude Code only, through its **documented hook payloads**, delivered as a materialized
local plugin directory launched via `agent-jit claude run --plugin-dir`.

## Consequences

- User settings are never mutated by the product; the plugin is materialized and passed explicitly.
- Claude's internal transcript JSONL is treated as an *optional, exact-shape import* for historical
  backfill only. It is never a live adapter contract, because its shape is unstable.
- Records whose shape is unknown or malformed are rejected with a reason code and never count
  toward the Phase 0 corpus of 50 qualifying trajectories.
