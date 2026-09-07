# ADR 0001 — MVP boundary: one runtime, one repository, one capability

- Status: accepted
- Date: 2026-08-26

## Context

`AGENT_JIT_SPEC.md` frames the MVP as an evidence-gated experiment, not a product backlog. The
thesis ("repeated repository habits can be compiled into deterministic capabilities") is unproven,
so every unit of scope that is not needed to test it is a liability.

## Decision

The MVP compiles at most **one** capability, for **one** repository (the read-only pilot
`pipeline-viz`), observed through **one** agent runtime (Claude Code), shipped as **one** binary
(`agent-jit`) on **one** platform (macOS arm64).

## Consequences

- No second runtime, generic tool-call proxy, background daemon, dashboard, cloud sync, or team
  registry ships in this milestone.
- A documented `STOP` (Phase 0) or `NO_GO` (Gate B) is a valid, successful outcome of the work.
- Cross-repository capability sharing is deferred until contracts and provenance are proven.
