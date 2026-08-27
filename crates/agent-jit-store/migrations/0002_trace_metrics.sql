-- 0002_trace_metrics: computed cost metrics, with the provenance of every number.
--
-- Metrics are derived from the events table, so this is a cache rather than a source of truth: it
-- can be dropped and recomputed. It exists because Phase 0 reads these numbers repeatedly, and
-- because storing them alongside `computed_from` and the measurement sources makes a report
-- auditable after the fact.
--
-- Every value that can be unknown is stored as a nullable value plus a NOT NULL source string, so
-- "we did not measure this" and "we measured zero" can never be confused in SQL either.

CREATE TABLE trace_metrics (
    trajectory_id            TEXT    PRIMARY KEY REFERENCES trajectories(trajectory_id),
    session_id               TEXT    NOT NULL REFERENCES sessions(session_id),

    active_duration_ms       INTEGER,
    active_duration_source   TEXT    NOT NULL,
    wall_duration_ms         INTEGER,
    wall_duration_source     TEXT    NOT NULL,

    turns                    INTEGER NOT NULL,
    agent_tool_calls         INTEGER NOT NULL,
    failed_tool_calls        INTEGER NOT NULL,
    observed_bytes           INTEGER NOT NULL,

    estimated_input_tokens   INTEGER,
    estimated_tokens_source  TEXT    NOT NULL,
    exact_input_tokens       INTEGER,
    exact_tokens_source      TEXT    NOT NULL,

    truncated_payloads       INTEGER NOT NULL,
    quarantined_segments     INTEGER NOT NULL,
    duplicates_collapsed     INTEGER NOT NULL,

    computed_from            TEXT    NOT NULL,
    event_count              INTEGER NOT NULL,
    adapter_schema           TEXT,
    runtime_version          TEXT,
    model_id                 TEXT,
    head_commit              TEXT    NOT NULL,

    record_json              TEXT    NOT NULL,
    digest                   TEXT    NOT NULL,
    written_at               INTEGER NOT NULL
) STRICT;

CREATE INDEX trace_metrics_by_session ON trace_metrics(session_id);
