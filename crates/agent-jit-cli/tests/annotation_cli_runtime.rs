#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
//! Runtime-red trace annotation command contract.

mod corpus_support;

use std::path::Path;

use agent_jit_store::{Store, StorePath};
use corpus_support::{CANARY, bin, init_repo, json, record};

const TOKEN_MARKER: &str = "[redacted:token]";

fn store(home: &Path) -> Store {
    Store::open(&StorePath::new(&home.join("data/state.sqlite3")).unwrap()).unwrap()
}

#[test]
fn given_trace_when_valid_annotation_is_submitted_then_revision_one_is_returned() {
    // Given
    let home = tempfile::tempdir().unwrap();
    let repo = tempfile::tempdir().unwrap();
    init_repo(repo.path());
    let (trajectory, _) = record(home.path(), repo.path());

    // When
    let output = bin(home.path())
        .args([
            "trace",
            "annotate",
            &trajectory.to_string(),
            "--outcome",
            "succeeded",
            "--actor",
            "operator",
            "--rationale",
            "review passed",
            "--evidence",
            "artifacts/result.json",
            "--json",
        ])
        .assert()
        .success();

    // Then
    let report = json(&output.get_output().stdout);
    assert_eq!(report["revision"], 1);
    assert_eq!(report["status"], "succeeded");
}

#[test]
fn given_secret_annotation_fields_when_submitted_then_persisted_values_are_redacted() {
    // Given
    let home = tempfile::tempdir().unwrap();
    let repo = tempfile::tempdir().unwrap();
    init_repo(repo.path());
    let (trajectory, _) = record(home.path(), repo.path());
    let actor = format!("operator-{CANARY}");
    let rationale = format!("review {CANARY}");
    let evidence = format!("artifacts/{CANARY}.json");

    // When
    let shown = bin(home.path())
        .args([
            "trace",
            "annotate",
            &trajectory.to_string(),
            "--outcome",
            "succeeded",
            "--actor",
            &actor,
            "--rationale",
            &rationale,
            "--evidence",
            &evidence,
            "--json",
        ])
        .assert()
        .success();

    // Then
    assert!(!String::from_utf8_lossy(&shown.get_output().stdout).contains(CANARY));
    let records = store(home.path())
        .list_outcome_annotations(&trajectory)
        .unwrap();
    let annotation = records.first().unwrap().body();
    assert_eq!(annotation.actor(), format!("operator-{TOKEN_MARKER}"));
    assert_eq!(annotation.rationale(), format!("review {TOKEN_MARKER}"));
    assert_eq!(
        annotation.evidence()[0].as_str(),
        format!("artifacts/{TOKEN_MARKER}.json")
    );
}
