# ADR 0006 — Pinned `@anthropic-ai/sandbox-runtime` sidecar for all execution

- Status: accepted
- Date: 2026-08-26

## Context

A compiled capability executes real commands derived from recorded behavior. Even with a read-only
primitive allowlist, execution needs an enforced boundary — not a promise — around the filesystem,
network, and inter-process automation.

## Decision

Replay and runtime execution run under exactly pinned `@anthropic-ai/sandbox-runtime@0.0.73` on
macOS arm64. Network and Apple Events are denied; writes to the repository and `.git` are denied;
only private scratch and build-cache paths are writable. The Rust parent additionally enforces
timeout, output byte cap, environment stripping, and fail-closed behavior.

## Consequences

- If the sandbox is unavailable or fails integrity checks, execution does not happen — there is no
  unsandboxed fallback path. The runtime answers `fallback_required` instead.
- The sidecar is prerelease software; its exact version and integrity digest are pinned and attested.
- Denials are proven by real Seatbelt probes in the test suite, not by fixtures alone.
