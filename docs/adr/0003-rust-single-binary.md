# ADR 0003 — Rust workspace, one distributable binary

- Status: accepted
- Date: 2026-08-26

## Context

Hooks run on the critical path of an interactive agent session, so recorder overhead is
user-visible. The product also needs long-lived local state, strict process control over a
sandboxed child, and a distribution story with no runtime prerequisites.

## Decision

Implement in Rust (edition 2024, exact pinned stable toolchain) as a Cargo workspace of four
crates with one-way dependencies — `domain <- store <- engine <- cli` — producing a single
executable named `agent-jit`.

## Consequences

- `unsafe_code` is forbidden workspace-wide; `unwrap`/`expect`/`panic` are denied in product code.
- Layering is enforced by the dependency graph: `domain` holds contracts and stays dependency-light.
- Distribution is one arm64 Mach-O binary plus one pinned Node sidecar (see ADR 0006).
