#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
//! Semantic trace annotation CLI behavior.

mod corpus_support;

use std::path::Path;

use agent_jit_store::{Store, StorePath};
use corpus_support::{bin, init_repo, json, record};

fn evidence_digest(home: &Path) -> String {
    let path = StorePath::new(&home.join("data/state.sqlite3")).unwrap();
    Store::open(&path)
        .unwrap()
        .evidence_digest()
        .unwrap()
        .to_string()
}

fn annotate(
    home: &Path,
    trajectory: &str,
    status: &str,
    evidence: &str,
) -> assert_cmd::assert::Assert {
    bin(home)
        .args([
            "trace",
            "annotate",
            trajectory,
            "--outcome",
            status,
            "--actor",
            "operator",
            "--rationale",
            "reviewed",
            "--evidence",
            evidence,
            "--json",
        ])
        .assert()
}

#[test]
fn given_revision_one_when_second_annotation_is_added_then_history_is_append_only_and_current_moves()
 {
    // Given
    let home = tempfile::tempdir().unwrap();
    let repo = tempfile::tempdir().unwrap();
    init_repo(repo.path());
    let (trajectory, _) = record(home.path(), repo.path());
    annotate(
        home.path(),
        &trajectory.to_string(),
        "unknown",
        "evidence/first.json",
    )
    .success();

    // When
    let output = annotate(
        home.path(),
        &trajectory.to_string(),
        "succeeded",
        "evidence/second.json",
    )
    .success();

    // Then
    assert_eq!(json(&output.get_output().stdout)["revision"], 2);
    let shown = bin(home.path())
        .args(["trace", "show", &trajectory.to_string(), "--json"])
        .assert()
        .success();
    let report = json(&shown.get_output().stdout);
    assert_eq!(report["annotation_history"].as_array().unwrap().len(), 2);
    assert_eq!(report["annotation_history"][0]["status"], "unknown");
    assert_eq!(report["current_annotation"]["revision"], 2);
    assert_eq!(report["current_annotation"]["status"], "succeeded");
}

#[test]
fn given_trace_when_invalid_outcome_is_submitted_then_refusal_leaves_logical_state_unchanged() {
    // Given
    let home = tempfile::tempdir().unwrap();
    let repo = tempfile::tempdir().unwrap();
    init_repo(repo.path());
    let (trajectory, _) = record(home.path(), repo.path());
    let before = evidence_digest(home.path());

    // When
    let output = annotate(
        home.path(),
        &trajectory.to_string(),
        "solved",
        "evidence/x.json",
    )
    .code(4);

    // Then
    assert_eq!(
        json(&output.get_output().stdout)["error"]["code"],
        "outcome_status_invalid"
    );
    assert_eq!(evidence_digest(home.path()), before);
}

#[test]
fn given_trace_when_traversal_evidence_is_submitted_then_refusal_leaves_logical_state_unchanged() {
    // Given
    let home = tempfile::tempdir().unwrap();
    let repo = tempfile::tempdir().unwrap();
    init_repo(repo.path());
    let (trajectory, _) = record(home.path(), repo.path());
    let before = evidence_digest(home.path());

    // When
    let output = annotate(home.path(), &trajectory.to_string(), "failed", "../secret").code(4);

    // Then
    assert_eq!(
        json(&output.get_output().stdout)["error"]["code"],
        "evidence_ref_invalid"
    );
    assert_eq!(evidence_digest(home.path()), before);
}

#[test]
fn given_current_annotation_when_traces_are_listed_then_trajectory_is_annotated() {
    // Given
    let home = tempfile::tempdir().unwrap();
    let repo = tempfile::tempdir().unwrap();
    init_repo(repo.path());
    let (trajectory, _) = record(home.path(), repo.path());
    annotate(
        home.path(),
        &trajectory.to_string(),
        "succeeded",
        "evidence/result.json",
    )
    .success();

    // When
    let output = bin(home.path())
        .args([
            "trace",
            "list",
            "--repo",
            repo.path().to_str().unwrap(),
            "--json",
        ])
        .assert()
        .success();

    // Then
    assert_eq!(
        json(&output.get_output().stdout)["trajectories"][0]["annotated"],
        true
    );
}

#[test]
fn given_current_annotation_when_trace_is_shown_then_status_and_revision_are_rendered() {
    // Given
    let home = tempfile::tempdir().unwrap();
    let repo = tempfile::tempdir().unwrap();
    init_repo(repo.path());
    let (trajectory, _) = record(home.path(), repo.path());
    annotate(
        home.path(),
        &trajectory.to_string(),
        "succeeded",
        "evidence/result.json",
    )
    .success();

    // When
    let output = bin(home.path())
        .args(["trace", "show", &trajectory.to_string()])
        .assert()
        .success();

    // Then
    let rendered = String::from_utf8(output.get_output().stdout.clone()).unwrap();
    assert!(rendered.contains("outcome          succeeded (annotation revision 1)"));
}
