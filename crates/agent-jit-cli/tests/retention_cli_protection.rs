#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
//! Referenced refusal and released-hold pruning paths.

mod corpus_support;

use std::path::Path;

use agent_jit_domain::candidate::Group;
use agent_jit_domain::canonical::digest_of;
use agent_jit_domain::envelope::{Envelope, Provenance};
use agent_jit_domain::ids::{GroupId, SessionId, TrajectoryId};
use agent_jit_store::{Store, StorePath};
use corpus_support::{bin, init_repo, json, record};

fn open(home: &Path) -> Store {
    Store::open(&StorePath::new(&home.join("data/state.sqlite3")).unwrap()).unwrap()
}

fn session(home: &Path, trajectory: TrajectoryId) -> SessionId {
    let shown = bin(home)
        .args(["trace", "show", &trajectory.to_string(), "--json"])
        .assert()
        .success();
    json(&shown.get_output().stdout)["session_id"]
        .as_str()
        .unwrap()
        .parse()
        .unwrap()
}

#[test]
fn given_group_referenced_trace_when_prune_applies_then_typed_refusal_keeps_logical_state() {
    // Given
    let home = tempfile::tempdir().unwrap();
    let repo = tempfile::tempdir().unwrap();
    init_repo(repo.path());
    let (trajectory, repository) = record(home.path(), repo.path());
    let manifest = digest_of(&serde_json::json!([trajectory])).unwrap();
    let group: GroupId = "grp_01J0000000000000000000000A".parse().unwrap();
    let mut store = open(home.path());
    store
        .put_group(&Envelope::new(
            group,
            Provenance::recorded_by("agent-jit/0.1.0"),
            Group {
                repository_id: repository,
                intent_label: "frozen".to_owned(),
                members: vec![trajectory],
                rationale: "frozen corpus".to_owned(),
                manifest_digest: manifest,
            },
        ))
        .unwrap();
    let before = store.evidence_digest().unwrap();
    drop(store);

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
        .code(4);

    // Then
    assert_eq!(
        json(&output.get_output().stdout)["error"]["code"],
        "retention_limit_unmet_protected"
    );
    assert_eq!(open(home.path()).evidence_digest().unwrap(), before);
}

#[test]
fn given_released_hold_when_prune_applies_then_session_is_deleted() {
    // Given
    let home = tempfile::tempdir().unwrap();
    let repo = tempfile::tempdir().unwrap();
    init_repo(repo.path());
    let (trajectory, _) = record(home.path(), repo.path());
    let session = session(home.path(), trajectory);
    bin(home.path())
        .args([
            "corpus",
            "hold",
            &session.to_string(),
            "--actor",
            "operator",
            "--rationale",
            "temporary",
            "--json",
        ])
        .assert()
        .success();
    bin(home.path())
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
    assert!(open(home.path()).get_session(&session).unwrap().is_none());
}
