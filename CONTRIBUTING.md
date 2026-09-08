# Contributing to agent-jit

Agent JIT is an early experiment, not a released tool. Contributions should improve testability,
safety, correctness, or a currently implemented workflow without presenting unfinished capability
execution as production-ready.

## Before opening an issue

- Confirm you are using an Apple Silicon Mac. Other targets are unsupported.
- Reproduce against current `main` with the pinned Rust toolchain.
- Check whether the command returns `not_implemented`; reserved commands are not bugs by themselves.
- Remove credentials, prompts, transcript text, private paths, repository contents, and state files
  from logs before sharing them.

Include command, exit code, expected behavior, actual behavior, macOS version, Rust version, and
runtime version when relevant. Use a minimal disposable repository whenever possible.

## Development setup

```sh
git clone https://github.com/0xendale/agent-jit.git
cd agent-jit
cargo build --locked
export AGENT_JIT_HOME="$(mktemp -d)"
./target/debug/agent-jit store migrate --json
./target/debug/agent-jit doctor --json
```

The workspace uses Rust 2024 and pins Rust 1.88.0. Dependencies flow in one direction:

```text
agent-jit-domain <- agent-jit-store <- agent-jit-engine <- agent-jit-cli
```

Keep domain contracts independent of persistence, persistence independent of orchestration, and
CLI concerns at the outer edge.

## Change workflow

1. Open an issue before broad feature or architecture work.
2. Branch from current `main`.
3. Write a failing test that demonstrates the intended behavior.
4. Implement the smallest change that makes it pass.
5. Run focused tests, then the full local gate.
6. Submit a focused pull request explaining behavior, risks, and verification.

Tests and implementation belong in the same atomic commit. Use conventional commit messages.
Never delete or weaken a failing test to obtain a green build.

## Code requirements

- Product code contains no unsafe Rust.
- Do not suppress type or lint errors.
- Avoid `unwrap`, `expect`, and `panic` in product code; return typed errors.
- Use stdout/stderr handles instead of `println!` or `eprintln!`.
- Preserve deterministic ordering and explicit provenance.
- Redact untrusted input before persistence and keep payloads bounded.
- Never place state in an observed repository or mutate runtime user settings.
- Never add raw shell, generated scripts, source writes, or network-backed capability primitives.

## Validation

The full suite includes real Claude smoke tests whenever its executable/version probe succeeds;
they can contact provider APIs and consume quota. Authentication is not checked before running,
so an installed but unauthenticated runtime can fail the suite. OpenCode smoke tests also run
when its executable and `AGENT_JIT_OPENCODE_TEST_MODEL` are available.

```sh
cargo fmt --all -- --check
cargo test --workspace --all-targets
cargo clippy --workspace --all-targets --all-features -- -D warnings
cargo doc --workspace --no-deps
cargo deny check
```

`cargo deny check` requires `cargo-deny`. CI uses version 0.20.2:

```sh
cargo install cargo-deny --locked --version 0.20.2
```

If behavior touches Claude Code or OpenCode integration, include process-level coverage. Real smoke
tests require installed, authenticated runtimes and may consume provider quota. Use only disposable
repositories and harmless prompts.

## Pull requests

Pull requests should state:

- user-visible behavior changed;
- safety or compatibility risks considered;
- exact validation commands run;
- real-runtime checks run or why they were not applicable.

Do not commit local state, captured sessions, benchmark evidence, credentials, private paths, build
outputs, or runtime-generated project files.
