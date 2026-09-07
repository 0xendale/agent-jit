# ADR 0004 — Private per-user state keyed by repository identity

- Status: accepted
- Date: 2026-08-26

## Context

Traces contain command lines, file paths, and tool output from real work. The spec lists secret
leakage as a top risk, and the pilot repository is read-only and must never carry agent-local state.

## Decision

All state lives outside observed repositories, in a private per-user directory keyed by Git
repository identity. SQLite owns indexed state; bounded append-only spools absorb hook writes so
hook latency stays low and a crash stays recoverable. Redaction runs **before** any persistent
write. Directories are `0700`, files `0600`.

## Consequences

- Nothing is written inside `pipeline-viz`, including during benchmarks (worktrees are disposable).
- Whether compiled capabilities should ever be committed to a repository stays an open question;
  the MVP answers "no".
- Retention and export are explicit user-invoked operations, not background jobs.
