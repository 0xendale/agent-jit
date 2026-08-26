# ADR 0008 — TDD discipline and two hard stop gates

- Status: accepted
- Date: 2026-08-26

## Context

The failure mode for this project is not a missing feature; it is a plausible-looking result that
nobody can reproduce. Both the thesis (is there enough repetition?) and the product (does the
compiled capability actually win?) can legitimately fail.

## Decision

1. Tests are written first, confirmed failing for the intended reason, then implemented;
   implementation and its test land in the same commit.
2. **Gate A (Phase 0)** stops the project when JIT coverage is below 20% or every candidate exceeds
   a 20-use projected break-even. Compiler and runtime work is forbidden on the stop path.
3. **Gate B (release)** stops the project when any release gate fails: 100% semantic replay match,
   zero silent mismatches, at least 99% deterministic replay, zero false positives across the
   pre-frozen negative corpus, at least 20 real reuses, at least 50% fewer tool calls, and at least
   30% lower median latency and estimated input tokens.

## Consequences

- Gate commands emit machine-readable reports and exit nonzero on failure.
- Thresholds are not tuned, and hard cases are not dropped, after results are known.
- A failed gate produces an evidence-backed stop or no-go report and no release.
