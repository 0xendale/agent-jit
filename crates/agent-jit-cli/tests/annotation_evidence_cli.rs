#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
//! Raw evidence-path rejection at the CLI persistence boundary.

mod corpus_support;

use std::path::Path;

use agent_jit_store::{Store, StorePath};
use corpus_support::{bin, init_repo, json, record};

fn evidence_digest(home: &Path) -> String {
    Store::open(&StorePath::new(&home.join("data/state.sqlite3")).unwrap())
        .unwrap()
        .evidence_digest()
        .unwrap()
        .to_string()
}

fn assert_refused(evidence: impl FnOnce(&Path, &Path) -> String) {
    let home = tempfile::tempdir().unwrap();
    let repo = tempfile::tempdir().unwrap();
    init_repo(repo.path());
    let (trajectory, _) = record(home.path(), repo.path());
    let before = evidence_digest(home.path());
    let evidence = evidence(home.path(), repo.path());

    let mut command = bin(home.path());
    command.env("HOME", home.path().parent().unwrap());
    let output = command
        .args([
            "trace",
            "annotate",
            &trajectory.to_string(),
            "--outcome",
            "failed",
            "--actor",
            "operator",
            "--rationale",
            "reviewed",
            "--evidence",
            &evidence,
            "--json",
        ])
        .assert()
        .code(4);

    assert_eq!(
        json(&output.get_output().stdout)["error"]["code"],
        "evidence_ref_invalid"
    );
    assert_eq!(evidence_digest(home.path()), before);
}

#[test]
fn absolute_repository_evidence_is_refused_before_aliasing() {
    assert_refused(|_, repo| repo.join("secret.txt").display().to_string());
}

#[test]
fn absolute_home_evidence_is_refused_before_aliasing() {
    assert_refused(|home, _| home.join("secret.txt").display().to_string());
}

#[test]
fn traversing_evidence_is_refused_before_redaction() {
    assert_refused(|_, _| "../secret.txt".to_owned());
}

#[test]
fn backslash_evidence_is_refused_before_redaction() {
    assert_refused(|_, _| "private\\secret.txt".to_owned());
}

#[test]
fn valid_relative_secret_bearing_evidence_is_redacted_and_persisted() {
    let home = tempfile::tempdir().unwrap();
    let repo = tempfile::tempdir().unwrap();
    init_repo(repo.path());
    let (trajectory, _) = record(home.path(), repo.path());
    let evidence = "evidence/ghp_0123456789abcdefghijklmnopqrstuvwxyzAB.json";

    bin(home.path())
        .args([
            "trace",
            "annotate",
            &trajectory.to_string(),
            "--outcome",
            "succeeded",
            "--actor",
            "operator",
            "--rationale",
            "reviewed",
            "--evidence",
            evidence,
            "--json",
        ])
        .assert()
        .success();

    let persisted: Vec<String> =
        Store::open(&StorePath::new(&home.path().join("data/state.sqlite3")).unwrap())
            .unwrap()
            .current_outcome_annotation(&trajectory)
            .unwrap()
            .unwrap()
            .body()
            .evidence()
            .iter()
            .map(|value| value.as_str().to_owned())
            .collect();
    assert_eq!(persisted.len(), 1);
    assert_ne!(persisted[0], evidence);
    assert!(!persisted[0].contains("ghp_"));
}
