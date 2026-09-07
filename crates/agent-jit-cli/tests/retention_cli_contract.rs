#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
//! Hold, release, applied prune, audit, and semantic-state CLI contracts.

mod corpus_support;

use std::path::Path;

use agent_jit_domain::ids::SessionId;
use agent_jit_store::{Store, StorePath};
use corpus_support::{CANARY, bin, init_repo, json, record};

const TOKEN_MARKER: &str = "[redacted:token]";

fn store(home: &Path) -> Store {
    Store::open(&StorePath::new(&home.join("data/state.sqlite3")).unwrap()).unwrap()
}

fn session_id(home: &Path, trajectory: &str) -> SessionId {
    let shown = bin(home)
        .args(["trace", "show", trajectory, "--json"])
        .assert()
        .success();
    json(&shown.get_output().stdout)["session_id"]
        .as_str()
        .unwrap()
        .parse()
        .unwrap()
}

fn hold(home: &Path, session: &str) -> assert_cmd::assert::Assert {
    bin(home)
        .args([
            "corpus",
            "hold",
            session,
            "--actor",
            "operator",
            "--rationale",
            "phase0",
            "--json",
        ])
        .assert()
}

#[test]
fn given_session_when_held_then_cli_reports_active_hold_and_complete_audit_exists() {
    // Given
    let home = tempfile::tempdir().unwrap();
    let repo = tempfile::tempdir().unwrap();
    init_repo(repo.path());
    let (trajectory, _) = record(home.path(), repo.path());
    let session = session_id(home.path(), &trajectory.to_string());

    // When
    let output = hold(home.path(), &session.to_string()).success();

    // Then
    assert_eq!(json(&output.get_output().stdout)["active"], true);
    let store = store(home.path());
    let active = store.active_hold(&session).unwrap().unwrap();
    assert_eq!(active.actor, "operator");
    assert_eq!(active.rationale, "phase0");
    let audit = store.list_corpus_audit().unwrap();
    assert_eq!(audit.last().unwrap().action.as_str(), "hold");
    assert_eq!(audit.last().unwrap().session_id, Some(session));
}

#[test]
fn given_active_hold_when_released_then_cli_clears_hold_and_audits_release() {
    // Given
    let home = tempfile::tempdir().unwrap();
    let repo = tempfile::tempdir().unwrap();
    init_repo(repo.path());
    let (trajectory, _) = record(home.path(), repo.path());
    let session = session_id(home.path(), &trajectory.to_string());
    hold(home.path(), &session.to_string()).success();

    // When
    let output = bin(home.path())
        .args([
            "corpus",
            "release",
            &session.to_string(),
            "--actor",
            "operator",
            "--rationale",
            "complete",
            "--json",
        ])
        .assert()
        .success();

    // Then
    assert_eq!(json(&output.get_output().stdout)["active"], false);
    let store = store(home.path());
    assert!(store.active_hold(&session).unwrap().is_none());
    let audit = store.list_corpus_audit().unwrap();
    assert_eq!(audit.last().unwrap().action.as_str(), "release");
    assert_eq!(audit.last().unwrap().actor, "operator");
    assert_eq!(audit.last().unwrap().rationale, "complete");
}

#[test]
fn given_unprotected_trace_when_prune_applies_then_delete_and_audit_commit_together() {
    // Given
    let home = tempfile::tempdir().unwrap();
    let repo = tempfile::tempdir().unwrap();
    init_repo(repo.path());
    let (trajectory, _) = record(home.path(), repo.path());
    let session = session_id(home.path(), &trajectory.to_string());
    let before = store(home.path()).evidence_digest().unwrap();

    // When
    let output = bin(home.path())
        .args([
            "corpus",
            "prune",
            "--max-age-days",
            "0",
            "--apply",
            "--json",
        ])
        .assert()
        .success();

    // Then
    assert_eq!(
        json(&output.get_output().stdout)["deleted_sessions"][0],
        session.to_string()
    );
    let store = store(home.path());
    assert_ne!(store.evidence_digest().unwrap(), before);
    assert!(store.get_session(&session).unwrap().is_none());
    assert_eq!(
        store
            .list_corpus_audit()
            .unwrap()
            .last()
            .unwrap()
            .action
            .as_str(),
        "prune"
    );
}

#[test]
fn given_secret_hold_fields_when_held_then_hold_and_audit_are_redacted() {
    // Given
    let home = tempfile::tempdir().unwrap();
    let repo = tempfile::tempdir().unwrap();
    init_repo(repo.path());
    let (trajectory, _) = record(home.path(), repo.path());
    let session = session_id(home.path(), &trajectory.to_string());
    let actor = format!("operator-{CANARY}");
    let rationale = format!("retain {CANARY}");

    // When
    bin(home.path())
        .args([
            "corpus",
            "hold",
            &session.to_string(),
            "--actor",
            &actor,
            "--rationale",
            &rationale,
            "--json",
        ])
        .assert()
        .success();

    // Then
    let store = store(home.path());
    let active = store.active_hold(&session).unwrap().unwrap();
    assert_eq!(active.actor, format!("operator-{TOKEN_MARKER}"));
    assert_eq!(active.rationale, format!("retain {TOKEN_MARKER}"));
    let audit = store.list_corpus_audit().unwrap();
    assert_eq!(audit.last().unwrap().actor, active.actor);
    assert_eq!(audit.last().unwrap().rationale, active.rationale);
}

#[test]
fn given_secret_release_fields_when_released_then_audit_is_redacted() {
    // Given
    let home = tempfile::tempdir().unwrap();
    let repo = tempfile::tempdir().unwrap();
    init_repo(repo.path());
    let (trajectory, _) = record(home.path(), repo.path());
    let session = session_id(home.path(), &trajectory.to_string());
    hold(home.path(), &session.to_string()).success();
    let actor = format!("reviewer-{CANARY}");
    let rationale = format!("release {CANARY}");

    // When
    bin(home.path())
        .args([
            "corpus",
            "release",
            &session.to_string(),
            "--actor",
            &actor,
            "--rationale",
            &rationale,
            "--json",
        ])
        .assert()
        .success();

    // Then
    let audit = store(home.path()).list_corpus_audit().unwrap();
    assert_eq!(
        audit.last().unwrap().actor,
        format!("reviewer-{TOKEN_MARKER}")
    );
    assert_eq!(
        audit.last().unwrap().rationale,
        format!("release {TOKEN_MARKER}")
    );
}
