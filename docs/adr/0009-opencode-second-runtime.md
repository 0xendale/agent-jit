# ADR 0009 — OpenCode becomes the second instrumented runtime

- Status: accepted
- Date: 2026-09-08

## Context

ADR-0002 scoped the MVP to Claude Code as the only instrumented runtime, deferring a second
runtime to Phase 2. The operator's primary daily runtime is OpenCode, so the corpus the whole
experiment depends on accrues where OpenCode runs, not where Claude Code runs. Instrumenting
only Claude Code leaves the 50-trajectory Phase 0 gate dependent on work the operator rarely
performs in that runtime.

## Decision

Pull the second-runtime roadmap item forward: instrument OpenCode through its documented
TypeScript plugin hooks (`tool.execute.before`, `tool.execute.after`, and the event bus),
delivered as a materialized plugin plus config fragment in private state, launched via
`agent-jit opencode run` with environment-based injection. OpenCode's internal state files are
never a live adapter contract, mirroring the Claude transcript rule.

## Consequences

- Both runtimes share one recorder, one redaction pipeline, one store, and one corpus; adapter
  schema IDs and runtime-version provenance distinguish sources.
- The same invariants apply: no user-settings mutation, no state inside observed repositories,
  hook-safe degradation so recorder faults never disturb the runtime.
- Phase 2's remaining second-runtime work reduces to integrating a second repository.
- The scope-fidelity and runtime acceptance criteria now require exactly two instrumented
  runtimes instead of one.
