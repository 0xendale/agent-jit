# agent-jit

Agent JIT compiles repeated, repository-specific agent habits into deterministic, sandboxed
capabilities, so a coding agent stops re-planning work it has already solved.

`AGENT_JIT_SPEC.md` is the living spec and is authoritative on scope, safety model, and metrics.

## Status

Pre-release experiment. This milestone is evidence-gated: it may legitimately end in a documented
`STOP` (Phase 0, insufficient repetition) or `NO_GO` (Gate B, failed release gates) with no release.
Claims in this README are limited to what has been measured.

## Support contract

- macOS **arm64 only**. No Linux, Windows, or x86_64 build is produced or supported.
- One distributable executable: `agent-jit`.
- One instrumented agent runtime: Claude Code, via a materialized local plugin.
- Execution requires a pinned `@anthropic-ai/sandbox-runtime` sidecar; without it, capabilities
  report `fallback_required` rather than running unsandboxed.

## Layout

| Path | Role |
| --- | --- |
| `crates/agent-jit-domain` | Typed contracts, versioned JSON schemas, canonical JSON and digests. |
| `crates/agent-jit-store` | Private per-user state keyed by Git repository identity. |
| `crates/agent-jit-engine` | Recorder, Phase 0 metrics, IR compiler, replay, capability runtime. |
| `crates/agent-jit-cli` | The `agent-jit` binary. |
| `docs/adr/` | Architecture decision records. |

Dependencies flow one way: `domain <- store <- engine <- cli`.

## Local gate

```sh
cargo fmt --all -- --check
cargo test --workspace --all-targets
cargo clippy --workspace --all-targets --all-features -- -D warnings
cargo doc --workspace --no-deps
cargo deny check
```

The toolchain is pinned exactly in `rust-toolchain.toml`; supply-chain policy lives in `deny.toml`.

## License

MIT.
