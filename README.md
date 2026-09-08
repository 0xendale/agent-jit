# agent-jit

[![CI](https://github.com/0xendale/agent-jit/actions/workflows/ci.yml/badge.svg)](https://github.com/0xendale/agent-jit/actions/workflows/ci.yml)

Agent JIT records repeated, repository-specific coding workflows so they can eventually be
compiled into deterministic, sandboxed capabilities instead of being planned from scratch every
time.

## Development status

**Experimental and under active development.** No release artifact, package, stable CLI, or
backward-compatibility guarantee exists yet. Build from source only. Expect commands, schemas, and
private state formats to change.

Current code supports recorder development and evaluation:

- private per-user state with SQLite and crash-recoverable event spools;
- live recorder integrations for Claude Code and OpenCode without changing user settings;
- redacted, bounded event normalization with runtime provenance;
- trajectory recovery, inspection, annotation, export, retention, and corpus status;
- versioned JSON schemas and deterministic digests.

Later experiment and capability commands are visible in `--help` but intentionally return
`not_implemented`. There is no usable compiled-capability release yet.

## Requirements

- Apple Silicon Mac running macOS (`aarch64-apple-darwin`)
- Git
- Rust via [rustup](https://rustup.rs/); this repository selects Rust 1.88.0 automatically
- Claude Code and/or OpenCode only when testing their recorder integrations
- An authenticated provider account for any real runtime invocation

Other operating systems and Intel Macs are unsupported. Build fails closed on unsupported targets.

## Build from source

```sh
git clone https://github.com/0xendale/agent-jit.git
cd agent-jit
cargo build --locked
./target/debug/agent-jit --version
```

Use an isolated state directory while testing:

```sh
export AGENT_JIT_HOME="$(mktemp -d)"
./target/debug/agent-jit store migrate --json
./target/debug/agent-jit doctor --json
./target/debug/agent-jit repo inspect --root "$PWD" --json
```

`AGENT_JIT_HOME` is an operator/testing override. Without it, Agent JIT uses private per-user macOS
application data and cache paths. Inspect resolved paths with:

```sh
./target/debug/agent-jit paths --json
```

## Try recorder integrations

Use a disposable Git repository first. Runtime commands can invoke provider APIs and consume quota.
Agent JIT injects a private plugin for that process only; it does not edit Claude Code settings,
OpenCode settings, shell startup files, or tracked files in the observed repository.

The recorder is not a sandbox for the launched runtime. Claude Code and OpenCode retain their own
permissions and can change files or access the network. Use a disposable repository with a committed
README.md and no sensitive content; a read-only prompt alone does not enforce read-only behavior.

### Claude Code

```sh
./target/debug/agent-jit claude run \
  --repo /absolute/path/to/disposable-repo \
  -- \
  -p --output-format json --allowedTools=Read \
  "Read README.md and summarize it in one sentence."
```

### OpenCode

Replace `provider/model` with a model available in your OpenCode configuration.

```sh
./target/debug/agent-jit opencode run \
  --repo /absolute/path/to/disposable-repo \
  -- \
  run --format json --model provider/model \
  "Read README.md and summarize it in one sentence."
```

After a session, recover completed spools and inspect trajectories:

```sh
./target/debug/agent-jit store recover --json
./target/debug/agent-jit trace list \
  --repo /absolute/path/to/disposable-repo \
  --json
```

Recorder-wide diagnostics currently expect Git, Claude Code, and OpenCode to be installed:

```sh
./target/debug/agent-jit doctor --scope recorder --json
```

Remove generated integration files with:

```sh
./target/debug/agent-jit claude uninstall --json
./target/debug/agent-jit opencode uninstall --json
```

If you used a temporary `AGENT_JIT_HOME`, delete that directory when finished.

## Run tests

The full test suite automatically runs real Claude smoke tests when the `claude` executable is
available. These tests can contact provider APIs and consume quota; executable detection does not
check authentication, so an installed but unauthenticated runtime can fail the suite. OpenCode
smokes also run when its executable and `AGENT_JIT_OPENCODE_TEST_MODEL` are available.

Contributor check:

```sh
cargo fmt --all -- --check
cargo test --workspace --all-targets
cargo clippy --workspace --all-targets --all-features -- -D warnings
```

Full local gate:

```sh
cargo fmt --all -- --check
cargo test --workspace --all-targets
cargo clippy --workspace --all-targets --all-features -- -D warnings
cargo doc --workspace --no-deps
cargo deny check
```

Real recorder smoke tests require installed, authenticated runtimes. OpenCode additionally requires
an explicit model so results do not depend on a machine's default provider:

```sh
cargo test -p agent-jit-cli --test claude_smoke_e2e -- --nocapture

AGENT_JIT_OPENCODE_TEST_MODEL=provider/model \
  cargo test -p agent-jit-cli --test opencode_smoke_e2e -- --nocapture
```

Claude smoke tests report a skip when executable/version detection fails. OpenCode smokes also
skip when no test model is configured. Missing authentication is not a skip condition. A skip is
useful for local development but is not release evidence.

## Architecture

One binary is built from four crates with one-way dependencies:

```text
agent-jit-domain <- agent-jit-store <- agent-jit-engine <- agent-jit-cli
```

| Path | Role |
| --- | --- |
| `crates/agent-jit-domain` | Typed contracts, schemas, canonical JSON, and digests |
| `crates/agent-jit-store` | Private SQLite state and evidence persistence |
| `crates/agent-jit-engine` | Adapters, normalization, repository identity, metrics, and recorder |
| `crates/agent-jit-cli` | `agent-jit` command parsing and process integration |
| `integrations/` | Embedded source templates for runtime plugins |
| `schemas/v1/` | Generated public JSON contracts |

## Feedback and contributions

Bug reports and focused pull requests are welcome while development continues. Read
[`CONTRIBUTING.md`](CONTRIBUTING.md) first. Never attach raw session transcripts, credentials,
private repository contents, or an Agent JIT state directory to a public issue.

## License

[MIT](LICENSE)
