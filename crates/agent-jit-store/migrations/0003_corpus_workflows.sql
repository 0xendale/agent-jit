-- 0003_corpus_workflows: append-only annotations, retention protection, and audit evidence.

CREATE TABLE outcome_annotations (
    annotation_id       TEXT    PRIMARY KEY,
    trajectory_id       TEXT    NOT NULL REFERENCES trajectories(trajectory_id),
    revision            INTEGER NOT NULL CHECK (revision BETWEEN 1 AND 4294967295),
    status              TEXT    NOT NULL,
    actor               TEXT    NOT NULL,
    annotated_at_unix_ms INTEGER NOT NULL,
    rationale           TEXT    NOT NULL,
    record_json         TEXT    NOT NULL,
    digest              TEXT    NOT NULL,
    written_at          INTEGER NOT NULL,
    UNIQUE (trajectory_id, revision)
) STRICT;

CREATE INDEX outcome_annotations_by_trajectory
    ON outcome_annotations(trajectory_id, revision, annotation_id);

CREATE TABLE outcome_annotation_current (
    trajectory_id       TEXT    PRIMARY KEY REFERENCES trajectories(trajectory_id),
    annotation_id       TEXT    NOT NULL UNIQUE REFERENCES outcome_annotations(annotation_id),
    revision            INTEGER NOT NULL CHECK (revision BETWEEN 1 AND 4294967295)
) STRICT;

CREATE TABLE session_holds (
    session_id          TEXT    PRIMARY KEY REFERENCES sessions(session_id),
    actor               TEXT    NOT NULL CHECK (length(actor) > 0),
    rationale           TEXT    NOT NULL CHECK (length(rationale) > 0),
    recorded_at_unix_ms INTEGER NOT NULL
) STRICT;

CREATE TABLE corpus_audit (
    audit_id            INTEGER PRIMARY KEY AUTOINCREMENT,
    action              TEXT    NOT NULL CHECK (action IN ('hold', 'release', 'prune')),
    session_id          TEXT,
    actor               TEXT    NOT NULL CHECK (length(actor) > 0),
    rationale           TEXT    NOT NULL CHECK (length(rationale) > 0),
    recorded_at_unix_ms INTEGER NOT NULL
) STRICT;

CREATE TABLE frozen_corpora (
    corpus_id           TEXT    PRIMARY KEY
) STRICT;

CREATE TABLE frozen_corpus_trajectories (
    corpus_id           TEXT    NOT NULL REFERENCES frozen_corpora(corpus_id),
    trajectory_id       TEXT    NOT NULL REFERENCES trajectories(trajectory_id),
    PRIMARY KEY (corpus_id, trajectory_id)
) STRICT;

CREATE INDEX frozen_corpus_trajectories_by_trajectory
    ON frozen_corpus_trajectories(trajectory_id, corpus_id);

CREATE TABLE candidate_trajectories (
    candidate_id        TEXT    NOT NULL REFERENCES candidate_versions(candidate_id),
    trajectory_id       TEXT    NOT NULL REFERENCES trajectories(trajectory_id),
    PRIMARY KEY (candidate_id, trajectory_id)
) STRICT;

CREATE INDEX candidate_trajectories_by_trajectory
    ON candidate_trajectories(trajectory_id, candidate_id);

CREATE TABLE benchmark_trajectories (
    benchmark_id        TEXT    NOT NULL REFERENCES benchmark_records(benchmark_id),
    trajectory_id       TEXT    NOT NULL REFERENCES trajectories(trajectory_id),
    PRIMARY KEY (benchmark_id, trajectory_id)
) STRICT;

CREATE INDEX benchmark_trajectories_by_trajectory
    ON benchmark_trajectories(trajectory_id, benchmark_id);
