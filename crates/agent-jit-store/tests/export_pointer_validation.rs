#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
//! Fail-closed validation for current annotation pointers.

mod support;

use agent_jit_domain::canonical::{canonical_string, digest_projection};
use agent_jit_domain::envelope::{Envelope, Provenance};
use agent_jit_domain::ids::{OutcomeAnnotationId, RepositoryId, TrajectoryId};
use agent_jit_domain::outcome::{AnnotationRevision, AnnotationStatus, OutcomeAnnotation};
use agent_jit_store::{Store, StorePath};
use rusqlite::Connection;
use serde_json::Value;

use support::{Fixture, NOW};

fn annotation(trajectory: TrajectoryId, suffix: char) -> Envelope<OutcomeAnnotation> {
    let id: OutcomeAnnotationId = format!("oan_01J0000000000000000000000{suffix}")
        .parse()
        .unwrap();
    let body = OutcomeAnnotation::new(
        trajectory,
        AnnotationRevision::new(1).unwrap(),
        AnnotationStatus::Succeeded,
        "operator",
        NOW,
        "reviewed",
        Vec::new(),
    )
    .unwrap();
    Envelope::new(id, Provenance::recorded_by("agent-jit/0.1.0"), body)
}

fn prepared() -> (
    tempfile::TempDir,
    RepositoryId,
    TrajectoryId,
    TrajectoryId,
    OutcomeAnnotationId,
) {
    let temp = tempfile::tempdir().unwrap();
    let mut fixture = Fixture::new(temp.path());
    let (_, first_trajectory) = fixture.add_session('A', NOW);
    let (_, second_trajectory) = fixture.add_session('B', NOW);
    let first = annotation(first_trajectory, 'A');
    let second = annotation(second_trajectory, 'B');
    let second_id = *second.id();
    fixture.store.append_outcome_annotation(&first).unwrap();
    fixture.store.append_outcome_annotation(&second).unwrap();
    let repository = fixture.repository_id;
    drop(fixture.store);
    (
        temp,
        repository,
        first_trajectory,
        second_trajectory,
        second_id,
    )
}

fn export_error(temp: &tempfile::TempDir, repository: RepositoryId) -> String {
    Store::open(&StorePath::new(&temp.path().join("state.sqlite3")).unwrap())
        .unwrap()
        .export_snapshot(&repository)
        .unwrap_err()
        .code()
        .to_owned()
}

fn corrupt_body(temp: &tempfile::TempDir, trajectory: TrajectoryId, field: &str, value: Value) {
    let connection = Connection::open(temp.path().join("state.sqlite3")).unwrap();
    let (annotation, text): (String, String) = connection
        .query_row(
            "SELECT annotation_id,record_json FROM outcome_annotations WHERE trajectory_id=?1",
            [trajectory.to_string()],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )
        .unwrap();
    let mut document: Value = serde_json::from_str(&text).unwrap();
    document["body"][field] = value;
    let digest = digest_projection(
        &document,
        &[
            "/provenance/recorded_at_unix_ms",
            "/body/annotated_at_unix_ms",
        ],
    )
    .unwrap();
    connection
        .execute(
            "UPDATE outcome_annotations SET record_json=?1,digest=?2 WHERE annotation_id=?3",
            [
                canonical_string(&document).unwrap(),
                digest.to_string(),
                annotation,
            ],
        )
        .unwrap();
}

#[test]
fn current_annotation_from_another_trajectory_is_refused_by_export() {
    let (temp, repository, trajectory, _, other_annotation) = prepared();
    let connection = Connection::open(temp.path().join("state.sqlite3")).unwrap();
    connection
        .execute(
            "DELETE FROM outcome_annotation_current WHERE annotation_id=?1",
            [other_annotation.to_string()],
        )
        .unwrap();
    connection
        .execute(
            "UPDATE outcome_annotation_current SET annotation_id=?1 WHERE trajectory_id=?2",
            [other_annotation.to_string(), trajectory.to_string()],
        )
        .unwrap();

    assert_eq!(export_error(&temp, repository), "store_record_invalid");
}

#[test]
fn current_annotation_with_wrong_revision_is_refused_by_export() {
    let (temp, repository, trajectory, _, _) = prepared();
    Connection::open(temp.path().join("state.sqlite3"))
        .unwrap()
        .execute(
            "UPDATE outcome_annotation_current SET revision=2 WHERE trajectory_id=?1",
            [trajectory.to_string()],
        )
        .unwrap();

    assert_eq!(export_error(&temp, repository), "store_record_invalid");
}

#[test]
fn current_annotation_with_wrong_digest_is_refused_by_export() {
    let (temp, repository, trajectory, _, _) = prepared();
    Connection::open(temp.path().join("state.sqlite3"))
        .unwrap()
        .execute(
            "UPDATE outcome_annotations SET digest=?1 WHERE trajectory_id=?2",
            ["0".repeat(64), trajectory.to_string()],
        )
        .unwrap();

    assert_eq!(export_error(&temp, repository), "store_record_invalid");
}

#[test]
fn annotation_body_from_another_trajectory_is_refused_when_columns_agree() {
    let (temp, repository, trajectory, other_trajectory, _) = prepared();
    corrupt_body(
        &temp,
        trajectory,
        "trajectory_id",
        serde_json::json!(other_trajectory),
    );

    assert_eq!(export_error(&temp, repository), "store_record_invalid");
}

#[test]
fn annotation_body_with_wrong_revision_is_refused_when_columns_agree() {
    let (temp, repository, trajectory, _, _) = prepared();
    corrupt_body(&temp, trajectory, "revision", serde_json::json!(2));

    assert_eq!(export_error(&temp, repository), "store_record_invalid");
}
