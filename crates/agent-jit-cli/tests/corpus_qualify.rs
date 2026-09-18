#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
//! `corpus qualify` — Todo 14's acceptance instrument.
//!
//! The command volunteers a verdict only from fail-closed store validation; every injected fault
//! below must stay excluded with an explainable reason count.

mod corpus_support;

use std::path::Path;

use agent_jit_domain::canonical::{canonical_string, digest_projection};
use corpus_support::{SESSION, bin, init_repo, json};
use rusqlite::Connection;
use serde_json::{Value, json};

/// One recorded, recovered session with an explicit session id: its trajectory id.
fn record_named(home: &Path, repository: &Path, session: &str, prompt: &str) -> String {
    bin(home)
        .args(["store", "migrate", "--json"])
        .assert()
        .success();
    let mut payload = serde_json::json!({
        "session_id": session,
        "cwd": repository,
        "hook_event_name": "SessionStart",
        "transcript_path": repository.join("private-transcript.jsonl"),
        "source": "startup",
    });
    for (event, name) in [
        ("session-start", "SessionStart"),
        ("user-prompt-submit", "UserPromptSubmit"),
        ("stop", "Stop"),
        ("session-end", "SessionEnd"),
    ] {
        payload["hook_event_name"] = json!(name);
        match name {
            "UserPromptSubmit" => {
                payload.as_object_mut().unwrap().remove("source");
                payload["prompt"] = json!(prompt);
            }
            "Stop" => {
                payload.as_object_mut().unwrap().remove("source");
                payload["stop_hook_active"] = json!(false);
            }
            "SessionEnd" => {
                payload.as_object_mut().unwrap().remove("source");
                payload["reason"] = json!("exit");
            }
            _ => {}
        }
        bin(home)
            .args([
                "hook",
                "ingest",
                "--event",
                event,
                "--claude-version",
                "2.0.0",
            ])
            .write_stdin(payload.to_string())
            .assert()
            .success();
    }
    let recovered = bin(home)
        .args(["store", "recover", "--json"])
        .assert()
        .success();
    json(&recovered.get_output().stdout)["recovered"][0]["trajectory_id"]
        .as_str()
        .unwrap()
        .to_owned()
}

fn annotate(home: &Path, trajectory: &str, outcome: &str) {
    bin(home)
        .args([
            "trace",
            "annotate",
            trajectory,
            "--outcome",
            outcome,
            "--actor",
            "operator",
            "--rationale",
            "reviewed",
            "--json",
        ])
        .assert()
        .success();
}

/// Runs `corpus qualify --json` and returns `(exit code, stdout)`.
fn qualify(home: &Path, repo: &Path, min: u32, max: u32) -> (i32, String) {
    let outcome = bin(home)
        .args([
            "corpus",
            "qualify",
            "--repo",
            repo.to_str().unwrap(),
            "--min",
            &min.to_string(),
            "--max",
            &max.to_string(),
            "--json",
        ])
        .assert()
        .get_output()
        .to_owned();
    (
        outcome.status.code().unwrap_or(-1),
        String::from_utf8(outcome.stdout).unwrap(),
    )
}

fn qualify_report(home: &Path, repo: &Path, min: u32, max: u32) -> Value {
    serde_json::from_str(&qualify(home, repo, min, max).1).unwrap()
}

fn qualify_error(home: &Path, repo: &Path, min: u32, max: u32) -> Value {
    serde_json::from_str(&qualify(home, repo, min, max).1).unwrap()
}

/// Rewrites the newest record row of `table` in place, recomputing its semantic digest the way the
/// recorder does (over the canonical form minus the volatile paths).
fn plant(home: &Path, table: &str, volatile: &[&str], mutate: impl Fn(&mut Value)) {
    let connection = Connection::open(home.join("data").join("state.sqlite3")).unwrap();
    let text: String = connection
        .query_row(
            &format!("SELECT record_json FROM {table} ORDER BY written_at DESC LIMIT 1"),
            [],
            |row| row.get(0),
        )
        .unwrap();
    let mut document: Value = serde_json::from_str(&text).unwrap();
    mutate(&mut document);
    let digest = if table == "trace_metrics" {
        agent_jit_domain::canonical::digest_of(&document).unwrap()
    } else {
        digest_projection(&document, volatile).unwrap()
    };
    connection
        .execute(
            &format!("UPDATE {table} SET record_json=?1, digest=?2 WHERE record_json=?3"),
            [
                canonical_string(&document).unwrap(),
                digest.to_string(),
                text,
            ],
        )
        .unwrap();
}

const SESSION_VOLATILE: &[&str] = &[
    "/provenance/recorded_at_unix_ms",
    "/body/started_at_unix_ms",
    "/body/ended_at_unix_ms",
];
const TRAJECTORY_VOLATILE: &[&str] = &[
    "/provenance/recorded_at_unix_ms",
    "/body/started_at_unix_ms",
    "/body/duration_ms",
];
const METRICS_VOLATILE: &[&str] = &["/provenance/recorded_at_unix_ms"];

fn first_session(home: &Path) -> String {
    connections(home)
        .query_row("SELECT session_id FROM sessions LIMIT 1", [], |row| {
            row.get(0)
        })
        .unwrap()
}

fn connections(home: &Path) -> Connection {
    Connection::open(home.join("data").join("state.sqlite3")).unwrap()
}

#[test]
fn given_two_annotated_complete_sessions_when_qualify_at_min_then_it_exits_zero() {
    // Given
    let home = tempfile::tempdir().unwrap();
    let repo = tempfile::tempdir().unwrap();
    init_repo(repo.path());
    let first = record_named(home.path(), repo.path(), SESSION, "first prompt");
    annotate(home.path(), &first, "succeeded");
    let second = record_named(
        home.path(),
        repo.path(),
        "cccccccc-2222-3333-4444-555566667777",
        "second prompt",
    );
    annotate(home.path(), &second, "failed");

    // When
    let code = qualify(home.path(), repo.path(), 2, 100).0;

    // Then
    assert_eq!(code, 0);
    let report = qualify_report(home.path(), repo.path(), 2, 100);
    assert_eq!(report["qualifying_count"], 2);
    assert_eq!(report["main_session_count"], 2);
    assert_eq!(report["recorded_trajectories"], 2);
    assert_eq!(report["duplicate_count"], 0);
    assert_eq!(report["exclusion_counts"], json!({}));
    assert_eq!(report["sufficient"], true);
}

#[test]
fn given_one_known_and_one_unknown_outcome_when_qualify_below_min_then_unknown_is_excluded() {
    // Given
    let home = tempfile::tempdir().unwrap();
    let repo = tempfile::tempdir().unwrap();
    init_repo(repo.path());
    let first = record_named(home.path(), repo.path(), SESSION, "first prompt");
    annotate(home.path(), &first, "succeeded");
    let second = record_named(
        home.path(),
        repo.path(),
        "cccccccc-2222-3333-4444-555566667777",
        "second prompt",
    );
    annotate(home.path(), &second, "unknown");

    // When
    let error = qualify_error(home.path(), repo.path(), 2, 100);

    // Then
    assert_eq!(error["error"]["code"], "corpus_insufficient");
    assert_eq!(error["error"]["class"], "gate_stop");
    assert_eq!(error["error"]["details"]["qualifying_count"], 1);
    assert_eq!(
        error["error"]["details"]["exclusion_counts"]["outcome_unknown"],
        1
    );
}

#[test]
fn given_missing_annotation_when_qualify_below_min_then_the_trajectory_is_excluded() {
    // Given
    let home = tempfile::tempdir().unwrap();
    let repo = tempfile::tempdir().unwrap();
    init_repo(repo.path());
    record_named(home.path(), repo.path(), SESSION, "prompt");

    // When
    let error = qualify_error(home.path(), repo.path(), 1, 100);

    // Then
    assert_eq!(error["error"]["code"], "corpus_insufficient");
    assert_eq!(
        error["error"]["details"]["exclusion_counts"]["missing_annotation"],
        1
    );
}

#[test]
fn given_an_incomplete_session_when_qualify_below_min_then_it_is_excluded() {
    // Given: an annotated session whose end was never recorded.
    let home = tempfile::tempdir().unwrap();
    let repo = tempfile::tempdir().unwrap();
    init_repo(repo.path());
    let trajectory = record_named(home.path(), repo.path(), SESSION, "prompt");
    let session = first_session(home.path());
    plant(home.path(), "sessions", SESSION_VOLATILE, |document| {
        document["body"]["ended_at_unix_ms"] = json!(Value::Null);
    });
    annotate(home.path(), &trajectory, "succeeded");

    // When
    let error = qualify_error(home.path(), repo.path(), 1, 100);

    // Then
    assert_eq!(
        error["error"]["details"]["exclusion_counts"]["incomplete"],
        1
    );
    assert_eq!(
        error["error"]["details"]["excluded"][0]["session_id"],
        session
    );
}

#[test]
fn given_duplicated_content_when_qualify_below_min_then_the_copy_is_excluded() {
    // Given: two sessions whose content is identical apart from the session identity, as a copied
    // subagent transcript replayed as its own session would be.
    let home = tempfile::tempdir().unwrap();
    let repo = tempfile::tempdir().unwrap();
    init_repo(repo.path());
    let first = record_named(home.path(), repo.path(), SESSION, "prompt");
    annotate(home.path(), &first, "succeeded");
    let second = record_named(
        home.path(),
        repo.path(),
        "cccccccc-2222-3333-4444-555566667777",
        "prompt",
    );
    annotate(home.path(), &second, "succeeded");

    // When
    let report = qualify_report(home.path(), repo.path(), 1, 100);

    // Then
    assert_eq!(report["qualifying_count"], 1);
    assert_eq!(report["duplicate_count"], 1);
    assert_eq!(report["exclusion_counts"]["duplicate_content"], 1);
}

#[test]
fn given_a_foreign_repository_when_qualify_then_only_that_repository_is_counted() {
    // Given
    let home = tempfile::tempdir().unwrap();
    let here = tempfile::tempdir().unwrap();
    let other = tempfile::tempdir().unwrap();
    init_repo(here.path());
    init_repo(other.path());
    let trajectory = record_named(
        home.path(),
        here.path(),
        SESSION,
        "in the counted repository",
    );
    record_named(
        home.path(),
        other.path(),
        "cccccccc-2222-3333-4444-555566667777",
        "in another repository",
    );
    annotate(home.path(), &trajectory, "succeeded");

    // When
    let error = qualify_error(home.path(), here.path(), 2, 100);

    // Then: the foreign trajectory never enters this repository's snapshot.
    assert_eq!(error["error"]["details"]["qualifying_count"], 1);
    assert_eq!(error["error"]["details"]["exclusion_counts"], json!({}));
}

#[test]
fn given_a_corrupted_record_when_qualify_then_export_validation_refuses() {
    // Given: a trajectory whose declared intent no longer matches its schema.
    let home = tempfile::tempdir().unwrap();
    let repo = tempfile::tempdir().unwrap();
    init_repo(repo.path());
    let trajectory = record_named(home.path(), repo.path(), SESSION, "prompt");
    annotate(home.path(), &trajectory, "succeeded");
    let text = connections(home.path())
        .query_row(
            "SELECT record_json FROM trajectories WHERE trajectory_id=?1",
            [&trajectory],
            |row| row.get::<_, String>(0),
        )
        .unwrap();
    let mut document: Value = serde_json::from_str(&text).unwrap();
    document["body"]["intent"] = json!(42);
    let digest = digest_projection(&document, TRAJECTORY_VOLATILE).unwrap();
    Connection::open(home.path().join("data").join("state.sqlite3"))
        .unwrap()
        .execute(
            "UPDATE trajectories SET record_json=?1, digest=?2 WHERE trajectory_id=?3",
            [
                canonical_string(&document).unwrap(),
                digest.to_string(),
                trajectory,
            ],
        )
        .unwrap();

    // When
    let (code, stdout) = qualify(home.path(), repo.path(), 1, 100);

    // Then
    assert_eq!(code, 4);
    let error: Value = serde_json::from_str(&stdout).unwrap();
    assert_eq!(error["error"]["code"], "store_record_invalid");
}

#[test]
fn given_a_lossy_session_when_qualify_below_min_then_recorder_loss_excludes_it() {
    // Given
    let home = tempfile::tempdir().unwrap();
    let repo = tempfile::tempdir().unwrap();
    init_repo(repo.path());
    let trajectory = record_named(home.path(), repo.path(), SESSION, "prompt");
    annotate(home.path(), &trajectory, "succeeded");
    plant(home.path(), "trace_metrics", METRICS_VOLATILE, |document| {
        document["health"]["quarantined_segments"] = json!(3);
    });

    // When
    let error = qualify_error(home.path(), repo.path(), 1, 100);

    // Then
    assert_eq!(
        error["error"]["details"]["exclusion_counts"]["recorder_loss"],
        1
    );
}

#[test]
fn given_a_session_with_derived_provenance_when_qualify_below_min_then_it_is_excluded() {
    // Given: a trajectory that was derived from other records rather than captured or imported.
    let home = tempfile::tempdir().unwrap();
    let repo = tempfile::tempdir().unwrap();
    init_repo(repo.path());
    let trajectory = record_named(home.path(), repo.path(), SESSION, "prompt");
    annotate(home.path(), &trajectory, "succeeded");
    plant(
        home.path(),
        "trajectories",
        TRAJECTORY_VOLATILE,
        |document| {
            document["provenance"]["source"] = json!("derived");
        },
    );

    // When
    let error = qualify_error(home.path(), repo.path(), 1, 100);

    // Then
    assert_eq!(
        error["error"]["details"]["exclusion_counts"]["provenance"],
        1
    );
}

#[test]
fn given_no_store_when_qualify_then_it_refuses_to_migrate_first() {
    // Given
    let home = tempfile::tempdir().unwrap();
    let repo = tempfile::tempdir().unwrap();
    init_repo(repo.path());

    // When
    let (code, stdout) = qualify(home.path(), repo.path(), 1, 100);

    // Then
    assert_eq!(code, 4);
    let error: Value = serde_json::from_str(&stdout).unwrap();
    assert_eq!(error["error"]["code"], "store_not_migrated");
}

#[test]
fn given_no_bounds_when_qualify_then_the_spec_defaults_apply_to_an_empty_store() {
    // Given
    let home = tempfile::tempdir().unwrap();
    let repo = tempfile::tempdir().unwrap();
    init_repo(repo.path());
    bin(home.path())
        .args(["store", "migrate", "--json"])
        .assert()
        .success();

    // When
    let outcome = bin(home.path())
        .args([
            "corpus",
            "qualify",
            "--repo",
            repo.path().to_str().unwrap(),
            "--json",
        ])
        .assert()
        .get_output()
        .to_owned();

    // Then: the defaults are the Phase 0 window, so an empty store stops below 50.
    assert_eq!(outcome.status.code(), Some(5));
    let error: Value = serde_json::from_slice(&outcome.stdout).unwrap();
    assert_eq!(error["error"]["code"], "corpus_insufficient");
    assert_eq!(error["error"]["details"]["qualifying_count"], 0);
    assert_eq!(error["error"]["details"]["min"], 50);
    assert_eq!(error["error"]["details"]["max"], 100);
}

#[test]
fn given_min_above_max_when_qualify_then_it_is_a_usage_error() {
    // Given
    let home = tempfile::tempdir().unwrap();
    let repo = tempfile::tempdir().unwrap();
    init_repo(repo.path());
    bin(home.path())
        .args(["store", "migrate", "--json"])
        .assert()
        .success();

    // When
    let outcome = bin(home.path())
        .args([
            "corpus",
            "qualify",
            "--repo",
            repo.path().to_str().unwrap(),
            "--min",
            "2",
            "--max",
            "1",
            "--json",
        ])
        .assert()
        .get_output()
        .to_owned();

    // Then
    assert_eq!(outcome.status.code(), Some(2));
    let error: Value = serde_json::from_slice(&outcome.stdout).unwrap();
    assert_eq!(error["error"]["code"], "usage");
}

#[test]
fn given_count_above_max_when_qualify_then_the_gate_stops_as_overfull() {
    // Given: two qualifying trajectories and a range that accepts only one.
    let home = tempfile::tempdir().unwrap();
    let repo = tempfile::tempdir().unwrap();
    init_repo(repo.path());
    let first = record_named(home.path(), repo.path(), SESSION, "first prompt");
    annotate(home.path(), &first, "succeeded");
    let second = record_named(
        home.path(),
        repo.path(),
        "cccccccc-2222-3333-4444-555566667777",
        "second prompt",
    );
    annotate(home.path(), &second, "succeeded");

    // When
    let error = qualify_error(home.path(), repo.path(), 1, 1);

    // Then
    assert_eq!(error["error"]["code"], "corpus_exceeds_max");
    assert_eq!(error["error"]["class"], "gate_stop");
    assert_eq!(error["error"]["details"]["qualifying_count"], 2);
}

#[test]
fn the_plant_helper_only_touches_the_named_row() {
    // Guards the plant helper against silently corrupting the wrong row.
    let home = tempfile::tempdir().unwrap();
    let repo = tempfile::tempdir().unwrap();
    init_repo(repo.path());
    let second = record_named(
        home.path(),
        repo.path(),
        "cccccccc-2222-3333-4444-555566667777",
        "second prompt",
    );
    plant(
        home.path(),
        "trajectories",
        TRAJECTORY_VOLATILE,
        |document| document["body"]["intent"] = json!(42),
    );
    let connection = connections(home.path());
    let text: String = connection
        .query_row(
            "SELECT record_json FROM trajectories WHERE trajectory_id=?1",
            [&second],
            |row| row.get(0),
        )
        .unwrap();
    let second_document: Value = serde_json::from_str(&text).unwrap();
    assert_ne!(
        SECOND_INTENT_GUARD,
        second_document["body"]["intent"]
            .as_str()
            .unwrap_or_default()
    );
}

const SECOND_INTENT_GUARD: &str = "second prompt";
