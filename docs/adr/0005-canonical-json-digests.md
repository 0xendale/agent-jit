# ADR 0005 — Canonical JSON and reproducible digests

- Status: accepted
- Date: 2026-08-26

## Context

Grouping, candidate selection, replay comparison, and gate decisions all rest on comparing records
across time. If two byte-identical-in-meaning records hash differently, every downstream decision
becomes unreproducible and no gate can be audited.

## Decision

Every persisted contract is a versioned JSON envelope with a canonical form: sorted object keys,
no insignificant whitespace, normalized number and string encoding, and explicit exclusion of
volatile fields (wall-clock timestamps, durations, process ids, absolute temp paths) from the
digest input. Raw records retain the volatile fields; only the canonical projection is digested.

## Consequences

- Digests are reproducible across runs, hosts, and schema-compatible versions.
- Schema changes require an explicit version bump plus a migration, not an in-place reinterpretation.
- Property tests cover canonicalization (idempotence, key-order independence, digest stability).
