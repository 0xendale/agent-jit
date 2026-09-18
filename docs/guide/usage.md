# Using agent-jit

This guide covers what the current build actually does, command by command, with the output each
command produces. It complements [`README.md`](../../README.md), which covers requirements,
building, and contributor checks.

Today `agent-jit` is a **recorder and a corpus tool**. It launches Claude Code or OpenCode against
a repository you choose, records what the session did into a private local database, and then lets
you recover, inspect, measure, annotate, retain, and export those trajectories. Nothing is compiled
into a reusable capability yet, and nothing is sent anywhere: the store is a local SQLite file that
only you read.

Everything below was produced by running the commands against a throwaway state directory and a
disposable Git repository. Identifiers in the sample output are from that run; yours will differ.

## What the build does

| Command | What it does now |
| --- | --- |
| `agent-jit paths` | Prints the private directories in use |
| `agent-jit doctor` | Checks host support, paths, and store health; `--scope recorder` runs deeper recorder checks |
| `agent-jit store` | `migrate`, `check`, and `recover` for the private database |
| `agent-jit repo` | `inspect` the identity of a repository |
| `agent-jit claude` | `materialize`, `run`, `uninstall` the Claude Code recorder plugin |
| `agent-jit opencode` | `materialize`, `run`, `uninstall` the OpenCode recorder bridge |
| `agent-jit hook` | `ingest` one hook event; called by the plugin, not by you |
| `agent-jit trace` | `list`, `show`, `metrics`, `annotate` recorded trajectories |
| `agent-jit corpus` | `status`, `qualify`, `hold`, `release`, `prune`, `export`, `import-claude` |
| `agent-jit schema` | `generate` the versioned JSON contracts, `validate` a record file |

These command groups are listed in `--help` but **reserved**: `group`, `candidate`, `phase0`,
`profile`, `capability`, `replay`, `sandbox`, `mcp`, `benchmark`, `install`, `uninstall`. They
describe the planned system, parse their arguments, and exit `3` with `not_implemented`. There is
no capability compilation, no replay, no sandbox, and no `jit.query` MCP tool in this build.

## Before you start

Requirements are in [`README.md`](../../README.md#requirements): an Apple Silicon Mac, Git, Rust,
and the runtimes you intend to record. Build a release binary and keep it at a stable path:

```sh
cargo build --release --locked
./target/release/agent-jit --version
```

The materialized plugin calls the binary that launched the session **by absolute path**. If you
move, rebuild elsewhere, or delete that binary, previously materialized plugin files point at a
path that no longer exists. Re-run `claude materialize` or `opencode materialize` after moving it.

The examples below write `agent-jit`; use the path to your build.

## Where state lives

By default the binary uses private per-user application data and cache directories. Print them:

```sh
agent-jit paths --json
```

```json
{
  "home": "<state-home>",
  "data": "<state-home>/data",
  "cache": "<state-home>/cache",
  "state_db": "<state-home>/data/state.sqlite3",
  "spool": "<state-home>/cache/spool",
  "logs": "<state-home>/cache/logs"
}
```

`AGENT_JIT_HOME` overrides all of them and is the easy way to keep experiments separate:

```sh
export AGENT_JIT_HOME=/absolute/path/to/state
```

Two rules are enforced, both refusals:

- the path must be absolute — otherwise `home_not_absolute`;
- it must not be inside a Git repository that could be recorded — otherwise
  `home_inside_repository`. Session data never lives in an observed repository.

Directories are created `0700` and files `0600`. The layout is:

```text
<state-home>/data/state.sqlite3                   recorded repositories, sessions, trajectories
<state-home>/data/claude-plugin/<version>/        materialized Claude Code plugin
<state-home>/data/opencode-plugin/<version>/      materialized OpenCode bridge
<state-home>/cache/spool/segments/<session-id>/   hook events not yet recovered
<state-home>/cache/spool/health.jsonl             recorder faults, when any occurred
<state-home>/cache/logs/
```

Repository identity is derived from the checkout's path, so moving or renaming a checkout after
recording starts produces a different `repo_id` and a separate corpus.

## First run

```sh
agent-jit store migrate --json
```

```json
{"from_version": 0, "to_version": 3, "applied": 3}
```

Migration is idempotent; running it again prints `migrated v3 -> v3 (0 applied)`. Verify the
database:

```sh
agent-jit store check
```

```text
schema_version         3
integrity              ok
foreign_key_violations 0
healthy                true
```

Before the first migration, `store check` creates an empty `state.sqlite3` at version 0 and then
refuses with `store_not_migrated` (exit 4). That is expected on a fresh state directory.

`doctor` reports version, host support, resolved paths, and whether a store is present.
`doctor --scope recorder` additionally checks Git, the installed Claude Code and OpenCode
executables, directory permissions, plugin integrity, and a hook round trip:

```sh
agent-jit doctor --scope recorder
```

```text
recorder: pass
```

The JSON form lists each check; `hook_round_trip` runs the real hook path inside a throwaway
directory and reports `{"events": 1, "stdout_bytes": 0, "trajectory_created": false}`. The recorder
scope expects both runtimes to be installed, so it fails on a machine that has only one.

Confirm which repository you are about to record:

```sh
agent-jit repo inspect --root /absolute/path/to/repo --json
```

```json
{
  "repo_id": "rep_72SA2S2S9K38HD9AD6QZ6G32BT",
  "git_common_dir": "/absolute/path/to/repo/.git",
  "worktree_root": "/absolute/path/to/repo",
  "head_commit": "25e5c4ef9089ce1c39419d86ac428885938ca834",
  "host": {"os": "macos", "arch": "aarch64"}
}
```

## Recording a session

`claude run` and `opencode run` do the whole setup for one launch:

1. validate that `--repo` is inside a Git repository (refusal, exit 4, if not);
2. materialize the plugin into the state directory if it is not already there;
3. launch the runtime in the worktree root with your arguments plus the plugin, and with
   `AGENT_JIT_HOME` and `AGENT_JIT_RUNTIME_VERSION` set for that process only.

Nothing outside the state directory is modified: no Claude Code or OpenCode settings, no shell
startup files, no files in the observed repository.

### Claude Code

```sh
agent-jit claude run \
  --repo /absolute/path/to/repo \
  -- \
  -p --output-format json --allowedTools=Read \
  "Read README.md and summarize it in one sentence."
```

Options before `--` belong to `agent-jit`; everything after `--` is passed through to the runtime
untouched, and `--plugin-dir <dir>` is appended. Omit `--` and its arguments to start an
interactive session with the recorder attached. `--claude-bin <path>` selects a specific
executable; the default is `claude` on `PATH`.

In text mode a successful launch prints nothing and exits with the runtime's own exit code. With
`--json`:

```json
{
  "repository_id": "rep_72SA2S2S9K38HD9AD6QZ6G32BT",
  "worktree_root": "/absolute/path/to/repo",
  "plugin_dir": "<state-home>/data/claude-plugin/v5453bf8838b7c0b7",
  "claude_bin": "/path/to/claude",
  "exit_code": 0
}
```

### OpenCode

```sh
agent-jit opencode run \
  --repo /absolute/path/to/repo \
  -- \
  run --format json --model provider/model \
  "Read README.md and summarize it in one sentence."
```

The bridge is injected through `OPENCODE_CONFIG` together with `AGENT_JIT_HOOK_ADAPTER=opencode`;
the JSON report adds `sessions_ended` to the fields above. Use `--opencode-bin <path>` to select
an executable other than `opencode` on `PATH`.

### What gets recorded

While the session runs, each hook writes one JSON segment into
`cache/spool/segments/<session-id>/`. Six event kinds are recorded: `SessionStart`, `UserPrompt`,
`ToolCall`, `ToolResult`, `Stop`, `SessionEnd`. Payloads are normalized, redacted, and bounded
before they are written, and the first user prompt of the session becomes the trajectory's intent.
Hooks never write to the runtime's stdout, so recording cannot corrupt the session.

The recorder is **not a sandbox**. The launched runtime keeps its own permissions and can edit
files and reach the network. A read-only prompt is not enforcement — use a repository you are
willing to have modified.

## Turning sessions into trajectories

Spool segments become rows in the database only when you ask:

```sh
agent-jit store recover --json
```

```json
{
  "recovered": [
    {"session": "<session-id>", "trajectory_id": "trj_1PS1X3REKWP9DTRKPQX7GQRTP7", "events_written": 6, "created": true},
    {"session": "<session-id>", "trajectory_id": "trj_BXCQH38HXJF0T3YNN5RARR59E5", "events_written": 6, "created": true}
  ],
  "drained": [],
  "pending": [],
  "skipped": [],
  "quarantined_segments": 0
}
```

Rules worth knowing:

- **Only complete sessions are recovered.** A session is complete when it recorded a `SessionEnd`.
  Its segments are deleted once it is stored.
- **Interrupted sessions stay pending.** A session whose runtime was killed is reported under
  `pending` with its event and turn counts, every time you run recovery, and its segments stay in
  the spool. Delete that session's directory under `cache/spool/segments/` if you do not want it.
- **Leftover spool residue is drained.** If the trajectory for a session is already stored — the
  state a kill inside a previous recovery's deletion can leave, or a directory with no accepted
  segments at all — the residue is deleted and reported under `drained` rather than being counted
  forever as `pending` or `skipped`. A drain never adds events, trajectories, or metrics: it only
  clears residue whose evidence is final.
- **Recovery is idempotent.** Identifiers are derived from content, so a second run reports
  `recovered 0 session(s)` rather than creating duplicates.
- **Launches that share a session id merge.** Resuming a conversation before recovery yields one
  trajectory holding all the events, with the first prompt as its intent.
- **Recover once per session.** If a session id is reused *after* its trajectory was stored, the new
  events are discarded: recovery reports `"events_written": 0, "created": false` and clears the
  spool. Recover when you are done with a conversation, not between resumes.
- **Sessions outside a repository are skipped.** If the recorded working directory is no longer
  inside a Git repository, the session is listed under `skipped` with a code.
- `quarantined_segments` counts segments that could not be parsed; they are set aside rather than
  dropped silently.

## Inspecting what was recorded

```sh
agent-jit trace list --repo /absolute/path/to/repo
```

```text
rep_72SA2S2S9K38HD9AD6QZ6G32BT  2 trajectories
  trj_1PS1X3REKWP9DTRKPQX7GQRTP7        63 ms  Summarize README.md in one sentence.
  trj_BXCQH38HXJF0T3YNN5RARR59E5        54 ms  List the tracked files in this repository.
```

```sh
agent-jit trace show trj_1PS1X3REKWP9DTRKPQX7GQRTP7
```

```text
trj_1PS1X3REKWP9DTRKPQX7GQRTP7
  intent           Summarize README.md in one sentence.
  active_duration  63 ms
  events           6
  outcome          unannotated
```

`trace show --json` adds the session id, repository id, head commit, the event list with kinds,
tool names and payload sizes, the full annotation history, and the current annotation. The session
id (`ses_…`) is what `corpus hold` and `corpus release` take.

```sh
agent-jit trace metrics trj_1PS1X3REKWP9DTRKPQX7GQRTP7
```

```text
trj_1PS1X3REKWP9DTRKPQX7GQRTP7
  active_duration  63 (exact:stored_events)
  wall_duration    113 (exact:stored_events)
  turns            1
  tool_calls       1 (0 failed)
  observed_bytes   3601
  input_tokens     901 (estimated:utf8_bytes_div_4)
  exact_tokens     unknown (unknown:no_versioned_source)
  lossy            false
```

Every measurement carries its source. Durations come from stored events; token counts are an
estimate from observed bytes, and `exact_tokens` stays `unknown` because no runtime reports a
versioned count. `lossy` turns true when payloads were truncated, segments were quarantined, or
duplicates were collapsed. `trace metrics --json` also carries provenance: adapter schema,
runtime version, head commit, and event count.

## Recording outcomes

Metrics do not say whether the session did the job. That is a human judgement:

```sh
agent-jit trace annotate trj_1PS1X3REKWP9DTRKPQX7GQRTP7 \
  --outcome succeeded \
  --actor operator \
  --rationale "summary matched README" \
  --evidence README.md
```

```text
trj_1PS1X3REKWP9DTRKPQX7GQRTP7 revision 1: succeeded
```

`--outcome` takes `succeeded`, `failed`, `abandoned`, or `unknown`; anything else is refused with
`outcome_status_invalid`. Each annotation is appended as a new revision and the newest one becomes
current, so changing your mind keeps the history. `--evidence` may be repeated and must be a
normalized relative path inside the repository — an absolute path is refused with
`evidence_ref_invalid`.

## Corpus, holds, and retention

```sh
agent-jit corpus status --repo /absolute/path/to/repo
```

```text
2 of 50 trajectories (48 still needed)
```

Fifty is the accrual target used by the planned qualification gate; the count is simply how many
trajectories that repository has today.
`qualify` is the Phase 0 acceptance instrument. It re-derives the verdict from the store's own
validated export snapshot, never from client-supplied numbers:

```sh
agent-jit corpus qualify --repo /absolute/path/to/repo --min 50 --max 100 --json
```

A trajectory counts toward `qualifying_count` only when it comes from a complete main session
(`recorded` or `imported` provenance), carries derived trace metrics the recorder never lost data
on, has a current outcome annotation with a known outcome, and its session is unique in the corpus
— a replayed transcript under a new session id is reported as `duplicate_content` rather than
counted. Every below-bar trajectory is reported under a stable reason: `missing_session`,
`incomplete`, `provenance`, `missing_metrics`, `recorder_loss`, `missing_annotation`,
`outcome_unknown`, or `duplicate_content` — rejected and duplicate counts stay outside the
denominator.

Exit classes: in-range exits 0; below `--min` stops the gate with `corpus_insufficient` (exit 5),
above `--max` with `corpus_exceeds_max` (exit 5) — a documented stop, not a defect; argument
mistakes exit 2, and a migrated store is required (exit 4 `store_not_migrated` otherwise). Any
stored record that fails the store's own digest or ownership validation refuses with exit 4
`store_record_invalid` rather than counting or dropping it silently.

Protect a session you are still working with:

```sh
agent-jit corpus hold ses_VZXJH82FTKX7NC7HT6BQPMAPQW \
  --actor operator --rationale "keep while reviewing"
agent-jit corpus release ses_VZXJH82FTKX7NC7HT6BQPMAPQW \
  --actor operator --rationale "review finished"
```

Both are idempotent and print `held (no-op)` or `released (no-op)` when nothing changed. Every
hold, release, and deletion writes an audit row.

Retention is a dry run unless you ask for the deletion:

```sh
agent-jit corpus prune --json
```

```json
{"dry_run": true, "max_age_days": 30, "max_bytes": 1073741824, "selected_sessions": [], "deleted_sessions": []}
```

`--max-age-days` and `--max-bytes` override the defaults (30 days, 1 GiB). Add `--apply` to delete
the selected sessions, with their events, trajectories, and annotations, in one transaction.
**Deletion is permanent**: after an apply, `trace list` no longer shows those trajectories and the
corpus count drops.

If the limits cannot be met without deleting a protected session — one that is held, or still open
— the command refuses with `retention_limit_unmet_protected` (exit 4) instead of quietly skipping
it. That applies to the dry run too, so a refusal means "release something first", not "the dry run
failed".

## Exporting

```sh
agent-jit corpus export \
  --repo /absolute/path/to/repo \
  --output /absolute/path/to/empty-dir \
  --mode full-redacted
```

`--mode metadata-only` writes just `manifest.json`: counts per record type, per-type content
digests, the current annotation per trajectory, and each trajectory's metrics. `--mode
full-redacted` additionally writes one JSON file per record under `records/<type>/<id>.json`. The
manifest uses a fixed clock (`generated_at_unix_ms` is `0`) and carries its own digest, so two
exports of the same data compare byte for byte.

The export contains every recorded session for that repository, annotated or not. Output
directories are `0700`, files are `0600`, and the tree is published by atomic rename from a
staging directory (`.agent-jit-export-*`) beside it — delete a leftover staging directory only
after a crash.

Refusals, all exit 4:

| Code | Meaning |
| --- | --- |
| `export_path_not_absolute` | `--output` must be an absolute path |
| `export_path_symlink` | the path is a symbolic link |
| `export_path_in_repository` | the path is inside the observed repository |
| `export_path_not_empty` | the directory already has content |

Exported records are redacted, but they still describe your repository and prompts. Treat an export
as sensitive.

Any exported record can be checked on its own:

```sh
agent-jit schema validate /path/to/export/records/agent_jit_event/evt_….json
```

```text
agent_jit.event v1 evt_4HN9FNQAWHR90FTADB7GC8PQZK 0cbffd5aa51eef4e94a9298dbd43eca9cdd2db5c0a77243fa96541dab958921d
```

## Existing Claude Code history

```sh
agent-jit corpus import-claude \
  --repo /absolute/path/to/repo \
  --project-dir /path/to/claude/project/dir \
  --json
```

This **scans and reports only**. It reads the transcripts in a Claude Code project directory
against the `claude_code.v2_1.jsonl` profile and prints how many would be accepted or rejected and
why. It writes nothing to the store, and `--dry-run` only echoes back into the report. Imported
history is not a substitute for recording: build a corpus with `claude run` or `opencode run`.

## Schemas

```sh
agent-jit schema generate --out /path/to/dir
```

Writes the 13 versioned contracts, including ones for parts of the system that are not built yet
(`agent_jit.candidate_contract`, `agent_jit.capability_version`, `agent_jit.replay_result`,
`agent_jit.benchmark_record`, `agent_jit.group`, `agent_jit.workflow`, `agent_jit.invocation`,
`agent_jit.outcome`). The generated copies in `schemas/v1/` are the public contracts.

## Output and exit codes

Every command prints a compact human form by default and a stable JSON object with `--json`.
Machine-readable output goes to stdout; errors also print `error: <code>: <message>` on stderr.
In JSON mode the error itself is on stdout:

```json
{"error": {"code": "store_not_migrated", "message": "the database has not been migrated; run `agent-jit store migrate`", "retryable": false, "class": "safety_refusal"}}
```

| Exit | Class | Meaning |
| --- | --- | --- |
| 0 | — | Success |
| 2 | usage | Bad arguments, malformed identifier, or unknown record |
| 3 | unsupported | `not_implemented`: a reserved command |
| 4 | safety_refusal | A refusal: unmigrated store, bad state home, protected data, invalid export path |
| 70 | internal | The launched runtime failed, or an internal fault |

## Troubleshooting

| Symptom | Cause and fix |
| --- | --- |
| `store_not_migrated` from `trace`, `corpus`, or `store recover` | The state directory is new. Run `agent-jit store migrate`. `claude run` and `opencode run` do not need a migrated store; everything after a session does. |
| `home_inside_repository` or `home_not_absolute` | `AGENT_JIT_HOME` points inside a repository or is relative. Point it at an absolute path outside every repository you record. |
| Claude Code reports the `agent-jit` MCP server as failed | The plugin registers `agent-jit mcp serve`, which is reserved in this build. Recording is unaffected; ignore it. |
| `recorder_unhealthy` from `doctor --scope recorder`, with `hook_round_trip: recorder_health_fault` | A hook failed earlier and wrote to `cache/spool/health.jsonl`. Read that file, then move it aside; the check passes once it is gone. |
| `trace list` shows nothing after a session | Segments are not recovered automatically. Run `agent-jit store recover`, and check `pending` if the runtime was killed. |
| Recovery keeps reporting the same session as still open | That session never recorded a `SessionEnd`. Remove its directory under `cache/spool/segments/` to drop it. |
| `claude_exited_nonzero` or `claude_launch_failed` (exit 70) | The runtime itself failed or was not found at that path. Check `--claude-bin` / `--opencode-bin` and that the runtime is authenticated. |
| `repo_not_a_repository` | `--repo` is not inside a Git repository. |
| `unknown option: …` from `claude run` | Runtime flags were placed before `--`. Only `--repo`, `--claude-bin` / `--opencode-bin`, and `--json` belong to `agent-jit`. |
| `retention_limit_unmet_protected` | A held or still-open session is older than the cutoff. Release it, or widen `--max-age-days`. |
| Hooks stop working after a rebuild | The plugin stores the absolute path of the binary that launched the session. Re-run `claude materialize` / `opencode materialize`. |

## Removing the integration

```sh
agent-jit claude uninstall --json
agent-jit opencode uninstall --json
```

```json
{"removed": true, "plugin_root": "<state-home>/data/claude-plugin", "state_preserved": true}
```

Uninstall removes only the materialized plugin directories; recorded state is kept, and a later
`claude run` materializes the plugin again. To remove the recordings as well, delete the state
directory printed by `agent-jit paths`.

## Safety

- Record only work you own. A trajectory holds prompts, tool calls, and file excerpts from the
  repository, redacted but not anonymized.
- Keep the state directory and any export private. Never attach either to a public issue.
- Record genuine work. Trajectories produced by exercising the recorder are evidence about the
  recorder, not about how the repository is worked on.
- The recorder observes; it does not restrain. Everything the launched runtime is allowed to do, it
  can still do.
