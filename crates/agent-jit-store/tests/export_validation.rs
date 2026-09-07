#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
//! Fail-closed validation for metrics attached to export snapshots.

mod support;

use agent_jit_domain::canonical::{canonical_string, digest_of};
use agent_jit_domain::ids::{RepositoryId, SessionId};
use agent_jit_store::{Store, StorePath};
use rusqlite::Connection;

use support::{Fixture, NOW};

fn prepared() -> (tempfile::TempDir, RepositoryId, String) {
    let temp = tempfile::tempdir().unwrap();
    let mut fixture = Fixture::new(temp.path());
    let (session, trajectory) = fixture.add_session('A', NOW);
    fixture.add_children('A', session, trajectory);
    let repository = fixture.repository_id;
    drop(fixture.store);
    (temp, repository, trajectory.to_string())
}

fn export_error(temp: &tempfile::TempDir, repository: RepositoryId) -> String {
    let path = StorePath::new(&temp.path().join("state.sqlite3")).unwrap();
    Store::open(&path)
        .unwrap()
        .export_snapshot(&repository)
        .unwrap_err()
        .code()
        .to_owned()
}

#[test]
fn malformed_metrics_json_is_refused_by_export() {
    // Given
    let (temp, repository, trajectory) = prepared();
    Connection::open(temp.path().join("state.sqlite3"))
        .unwrap()
        .execute(
            "UPDATE trace_metrics SET record_json='{}' WHERE trajectory_id=?1",
            [trajectory],
        )
        .unwrap();

    // When / Then
    assert_eq!(export_error(&temp, repository), "store_record_invalid");
}

#[test]
fn metrics_digest_mismatch_is_refused_by_export() {
    // Given
    let (temp, repository, trajectory) = prepared();
    Connection::open(temp.path().join("state.sqlite3"))
        .unwrap()
        .execute(
            "UPDATE trace_metrics SET digest=?1 WHERE trajectory_id=?2",
            ["0".repeat(64), trajectory],
        )
        .unwrap();

    // When / Then
    assert_eq!(export_error(&temp, repository), "store_record_invalid");
}

#[test]
fn cross_session_metrics_are_refused_even_with_matching_digest() {
    // Given
    let (temp, repository, trajectory) = prepared();
    let connection = Connection::open(temp.path().join("state.sqlite3")).unwrap();
    let text: String = connection
        .query_row(
            "SELECT record_json FROM trace_metrics WHERE trajectory_id=?1",
            [&trajectory],
            |row| row.get(0),
        )
        .unwrap();
    let mut value: serde_json::Value = serde_json::from_str(&text).unwrap();
    let other: SessionId = "ses_01J0000000000000000000000B".parse().unwrap();
    value["provenance"]["session_id"] = serde_json::json!(other);
    let digest = digest_of(&value).unwrap().to_string();
    connection
        .execute(
            "UPDATE trace_metrics SET record_json=?1,digest=?2 WHERE trajectory_id=?3",
            [canonical_string(&value).unwrap(), digest, trajectory],
        )
        .unwrap();

    // When / Then
    assert_eq!(export_error(&temp, repository), "store_record_invalid");
}

#[test]
fn cross_repository_metrics_are_refused_even_with_matching_digest() {
    // Given
    let (temp, repository, trajectory) = prepared();
    let connection = Connection::open(temp.path().join("state.sqlite3")).unwrap();
    let text: String = connection
        .query_row(
            "SELECT record_json FROM trace_metrics WHERE trajectory_id=?1",
            [&trajectory],
            |row| row.get(0),
        )
        .unwrap();
    let mut value: serde_json::Value = serde_json::from_str(&text).unwrap();
    let identity = digest_of(&serde_json::json!({"repo": "other"})).unwrap();
    value["provenance"]["repository_id"] = serde_json::json!(RepositoryId::derived(&identity));
    let digest = digest_of(&value).unwrap().to_string();
    connection
        .execute(
            "UPDATE trace_metrics SET record_json=?1,digest=?2 WHERE trajectory_id=?3",
            [canonical_string(&value).unwrap(), digest, trajectory],
        )
        .unwrap();

    // When / Then
    assert_eq!(export_error(&temp, repository), "store_record_invalid");
}
