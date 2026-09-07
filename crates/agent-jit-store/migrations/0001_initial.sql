-- 0001_initial: the record tables.
--
-- Two rules shape this schema. Records are immutable: a row is written once, addressed by its
-- branded identifier, and never updated — mutable state lives in explicit pointer tables instead.
-- And the canonical record travels whole, as `record_json`, with indexed columns beside it for
-- querying: the Rust contracts stay authoritative, while SQL still enforces references and
-- uniqueness so a bug in one layer cannot corrupt the other.

CREATE TABLE repositories (
    repo_id          TEXT    PRIMARY KEY,
    git_common_dir   TEXT    NOT NULL,
    identity_digest  TEXT    NOT NULL UNIQUE,
    label            TEXT    NOT NULL,
    record_json      TEXT    NOT NULL,
    digest           TEXT    NOT NULL,
    written_at       INTEGER NOT NULL
) STRICT;

CREATE TABLE sessions (
    session_id       TEXT    PRIMARY KEY,
    repo_id          TEXT    NOT NULL REFERENCES repositories(repo_id),
    worktree_root    TEXT    NOT NULL,
    head_commit      TEXT    NOT NULL,
    runtime          TEXT    NOT NULL,
    runtime_version  TEXT    NOT NULL,
    model_id         TEXT    NOT NULL,
    started_at_unix_ms INTEGER NOT NULL,
    ended_at_unix_ms   INTEGER,
    record_json      TEXT    NOT NULL,
    digest           TEXT    NOT NULL,
    written_at       INTEGER NOT NULL
) STRICT;

CREATE INDEX sessions_by_repo ON sessions(repo_id, session_id);

CREATE TABLE events (
    event_id         TEXT    PRIMARY KEY,
    session_id       TEXT    NOT NULL REFERENCES sessions(session_id),
    sequence         INTEGER NOT NULL,
    kind             TEXT    NOT NULL,
    tool_name        TEXT,
    exit_code        INTEGER,
    payload_digest   TEXT,
    payload_bytes    INTEGER NOT NULL,
    payload_truncated INTEGER NOT NULL,
    record_json      TEXT    NOT NULL,
    digest           TEXT    NOT NULL,
    written_at       INTEGER NOT NULL,
    UNIQUE (session_id, sequence)
) STRICT;

CREATE TABLE trajectories (
    trajectory_id    TEXT    PRIMARY KEY,
    session_id       TEXT    NOT NULL REFERENCES sessions(session_id),
    repo_id          TEXT    NOT NULL REFERENCES repositories(repo_id),
    head_commit      TEXT    NOT NULL,
    intent           TEXT    NOT NULL,
    record_json      TEXT    NOT NULL,
    digest           TEXT    NOT NULL,
    written_at       INTEGER NOT NULL
) STRICT;

CREATE INDEX trajectories_by_repo ON trajectories(repo_id, trajectory_id);

CREATE TABLE outcomes (
    outcome_id       TEXT    PRIMARY KEY,
    trajectory_id    TEXT    NOT NULL UNIQUE REFERENCES trajectories(trajectory_id),
    status           TEXT    NOT NULL,
    confirmed_by_human INTEGER NOT NULL,
    record_json      TEXT    NOT NULL,
    digest           TEXT    NOT NULL,
    written_at       INTEGER NOT NULL
) STRICT;

CREATE TABLE groups (
    group_id         TEXT    PRIMARY KEY,
    repo_id          TEXT    NOT NULL REFERENCES repositories(repo_id),
    intent_label     TEXT    NOT NULL,
    manifest_digest  TEXT    NOT NULL UNIQUE,
    record_json      TEXT    NOT NULL,
    digest           TEXT    NOT NULL,
    written_at       INTEGER NOT NULL
) STRICT;

CREATE TABLE group_members (
    group_id         TEXT    NOT NULL REFERENCES groups(group_id),
    trajectory_id    TEXT    NOT NULL REFERENCES trajectories(trajectory_id),
    PRIMARY KEY (group_id, trajectory_id)
) STRICT;

CREATE TABLE candidate_versions (
    candidate_id     TEXT    PRIMARY KEY,
    group_id         TEXT    NOT NULL REFERENCES groups(group_id),
    name             TEXT    NOT NULL,
    state            TEXT    NOT NULL,
    break_even_uses  INTEGER NOT NULL,
    record_json      TEXT    NOT NULL,
    digest           TEXT    NOT NULL,
    written_at       INTEGER NOT NULL
) STRICT;

CREATE TABLE workflows (
    workflow_id      TEXT    PRIMARY KEY,
    candidate_id     TEXT    NOT NULL REFERENCES candidate_versions(candidate_id),
    ir_version       INTEGER NOT NULL,
    ir_digest        TEXT    NOT NULL UNIQUE,
    node_count       INTEGER NOT NULL,
    record_json      TEXT    NOT NULL,
    digest           TEXT    NOT NULL,
    written_at       INTEGER NOT NULL
) STRICT;

CREATE TABLE replays (
    replay_id        TEXT    PRIMARY KEY,
    workflow_id      TEXT    NOT NULL REFERENCES workflows(workflow_id),
    fixture_trajectory_id TEXT NOT NULL REFERENCES trajectories(trajectory_id),
    verdict          TEXT    NOT NULL,
    deterministic    INTEGER NOT NULL,
    record_json      TEXT    NOT NULL,
    digest           TEXT    NOT NULL,
    written_at       INTEGER NOT NULL
) STRICT;

CREATE TABLE capability_versions (
    capability_id    TEXT    PRIMARY KEY,
    workflow_id      TEXT    NOT NULL REFERENCES workflows(workflow_id),
    name             TEXT    NOT NULL,
    version          INTEGER NOT NULL,
    state            TEXT    NOT NULL,
    fingerprint_digest TEXT  NOT NULL,
    record_json      TEXT    NOT NULL,
    digest           TEXT    NOT NULL,
    written_at       INTEGER NOT NULL,
    UNIQUE (name, version)
) STRICT;

-- The one mutable table: which immutable capability version is currently enabled.
CREATE TABLE capability_active (
    name             TEXT    PRIMARY KEY,
    capability_id    TEXT    NOT NULL REFERENCES capability_versions(capability_id),
    approved_by      TEXT,
    approved_at      INTEGER NOT NULL
) STRICT;

CREATE TABLE invocations (
    invocation_id    TEXT    PRIMARY KEY,
    capability_id    TEXT    REFERENCES capability_versions(capability_id),
    result           TEXT    NOT NULL,
    record_json      TEXT    NOT NULL,
    digest           TEXT    NOT NULL,
    written_at       INTEGER NOT NULL
) STRICT;

CREATE TABLE benchmark_records (
    benchmark_id     TEXT    PRIMARY KEY,
    repo_id          TEXT    NOT NULL REFERENCES repositories(repo_id),
    task_id          TEXT    NOT NULL,
    condition        TEXT    NOT NULL,
    order_index      INTEGER NOT NULL,
    correctness      TEXT    NOT NULL,
    record_json      TEXT    NOT NULL,
    digest           TEXT    NOT NULL,
    written_at       INTEGER NOT NULL,
    UNIQUE (task_id, condition, order_index)
) STRICT;

-- Recorder health counters. Malformed input is counted here and never persisted as a record.
CREATE TABLE health_events (
    health_id        INTEGER PRIMARY KEY AUTOINCREMENT,
    code             TEXT    NOT NULL,
    detail           TEXT    NOT NULL,
    observed_at      INTEGER NOT NULL
) STRICT;
