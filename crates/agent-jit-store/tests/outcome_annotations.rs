#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
//! Append-only annotation history and current-pointer contracts.

mod support;

use agent_jit_domain::envelope::{Envelope, Provenance};
use agent_jit_domain::ids::{OutcomeAnnotationId, TrajectoryId};
use agent_jit_domain::outcome::{
    AnnotationRevision, AnnotationStatus, EvidenceRef, OutcomeAnnotation,
};

use agent_jit_store::{FixedClock, Store, StorePath};
use rusqlite::{Connection, params};
use std::sync::{Arc, Barrier};
use support::{Fixture, NOW};

fn annotation(
    trajectory: TrajectoryId,
    revision: u32,
    suffix: char,
) -> Envelope<OutcomeAnnotation> {
    let id: OutcomeAnnotationId = format!("oan_01J0000000000000000000000{suffix}")
        .parse()
        .unwrap();
    let body = OutcomeAnnotation::new(
        trajectory,
        AnnotationRevision::new(revision).unwrap(),
        AnnotationStatus::Succeeded,
        "operator",
        NOW,
        "reviewed",
        vec![EvidenceRef::new("artifacts/result.json").unwrap()],
    )
    .unwrap();
    Envelope::new(id, Provenance::recorded_by("agent-jit/0.1.0"), body)
}

#[test]
fn given_two_sequential_annotations_when_second_is_appended_then_history_and_current_advance() {
    // Given
    let temp = tempfile::tempdir().unwrap();
    let mut fixture = Fixture::new(temp.path());
    let (_, trajectory) = fixture.add_session('A', NOW);
    let first = annotation(trajectory, 1, 'A');
    let second = annotation(trajectory, 2, 'B');
    fixture.store.append_outcome_annotation(&first).unwrap();

    // When
    fixture.store.append_outcome_annotation(&second).unwrap();

    // Then
    assert_eq!(
        fixture.store.list_outcome_annotations(&trajectory).unwrap(),
        vec![first, second.clone()]
    );
    assert_eq!(
        fixture
            .store
            .current_outcome_annotation(&trajectory)
            .unwrap(),
        Some(second)
    );
}

#[test]
fn given_no_history_when_revision_two_is_appended_then_initial_revision_is_refused_atomically() {
    // Given
    let temp = tempfile::tempdir().unwrap();
    let mut fixture = Fixture::new(temp.path());
    let (_, trajectory) = fixture.add_session('A', NOW);
    let before = fixture.store.evidence_digest().unwrap();

    // When
    let error = fixture
        .store
        .append_outcome_annotation(&annotation(trajectory, 2, 'B'))
        .unwrap_err();

    // Then
    assert_eq!(error.code(), "outcome_annotation_revision_invalid");
    assert_eq!(fixture.store.evidence_digest().unwrap(), before);
    assert!(
        fixture
            .store
            .list_outcome_annotations(&trajectory)
            .unwrap()
            .is_empty()
    );
    assert!(
        fixture
            .store
            .current_outcome_annotation(&trajectory)
            .unwrap()
            .is_none()
    );
}

#[test]
fn given_revision_one_when_revision_three_is_appended_then_gap_is_refused_atomically() {
    // Given
    let temp = tempfile::tempdir().unwrap();
    let mut fixture = Fixture::new(temp.path());
    let (_, trajectory) = fixture.add_session('A', NOW);
    let first = annotation(trajectory, 1, 'A');
    fixture.store.append_outcome_annotation(&first).unwrap();
    let before = fixture.store.evidence_digest().unwrap();

    // When
    let error = fixture
        .store
        .append_outcome_annotation(&annotation(trajectory, 3, 'C'))
        .unwrap_err();

    // Then
    assert_eq!(error.code(), "outcome_annotation_revision_invalid");
    assert_eq!(fixture.store.evidence_digest().unwrap(), before);
    assert_eq!(
        fixture
            .store
            .current_outcome_annotation(&trajectory)
            .unwrap(),
        Some(first)
    );
}

#[test]
fn given_stored_annotation_id_when_same_id_has_changed_body_then_mutation_is_refused() {
    // Given
    let temp = tempfile::tempdir().unwrap();
    let mut fixture = Fixture::new(temp.path());
    let (_, trajectory) = fixture.add_session('A', NOW);
    let first = annotation(trajectory, 1, 'A');
    fixture.store.append_outcome_annotation(&first).unwrap();
    let changed = Envelope::new(
        *first.id(),
        first.provenance().clone(),
        OutcomeAnnotation::new(
            trajectory,
            AnnotationRevision::new(2).unwrap(),
            AnnotationStatus::Failed,
            "operator",
            NOW,
            "changed",
            Vec::new(),
        )
        .unwrap(),
    );
    let before = fixture.store.evidence_digest().unwrap();

    // When
    let error = fixture
        .store
        .append_outcome_annotation(&changed)
        .unwrap_err();

    // Then
    assert_eq!(error.code(), "store_already_exists");
    assert_eq!(fixture.store.evidence_digest().unwrap(), before);
    assert_eq!(
        fixture.store.list_outcome_annotations(&trajectory).unwrap(),
        vec![first]
    );
}

#[test]
fn given_current_revision_at_max_when_append_is_attempted_then_overflow_is_typed_and_atomic() {
    // Given
    let temp = tempfile::tempdir().unwrap();
    let path = temp.path().join("state.sqlite3");
    let mut fixture = Fixture::new(temp.path());
    let (_, trajectory) = fixture.add_session('A', NOW);
    let first = annotation(trajectory, 1, 'A');
    fixture.store.append_outcome_annotation(&first).unwrap();
    let max = annotation(trajectory, u32::MAX, 'A');
    let record_json = agent_jit_domain::canonical::canonical_string_of(&max).unwrap();
    let digest = max.digest().unwrap().to_string();
    drop(fixture.store);
    let connection = Connection::open(&path).unwrap();
    connection
        .execute(
            "UPDATE outcome_annotations SET revision=?1,record_json=?2,digest=?3 \
             WHERE annotation_id=?4",
            params![u32::MAX, record_json, digest, max.id().to_string()],
        )
        .unwrap();
    drop(connection);
    let mut store = Store::open(&StorePath::new(&path).unwrap())
        .unwrap()
        .with_clock(FixedClock::at(NOW));
    let before = store.evidence_digest().unwrap();

    // When
    let error = store
        .append_outcome_annotation(&annotation(trajectory, 1, 'B'))
        .unwrap_err();

    // Then
    assert_eq!(error.code(), "outcome_annotation_revision_overflow");
    assert_eq!(store.evidence_digest().unwrap(), before);
    assert_eq!(
        store.list_outcome_annotations(&trajectory).unwrap(),
        vec![max.clone()]
    );
    assert_eq!(
        store.current_outcome_annotation(&trajectory).unwrap(),
        Some(max)
    );
}

#[test]
fn given_two_file_connections_when_appending_concurrently_then_history_remains_consecutive() {
    let temp = tempfile::tempdir().unwrap();
    let path = temp.path().join("state.sqlite3");
    let mut fixture = Fixture::new(temp.path());
    let (_, trajectory) = fixture.add_session('A', NOW);
    drop(fixture.store);
    let barrier = Arc::new(Barrier::new(2));

    let workers: Vec<_> = ['B', 'C']
        .into_iter()
        .map(|suffix| {
            let path = path.clone();
            let barrier = Arc::clone(&barrier);
            std::thread::spawn(move || -> Result<(), String> {
                let mut store = Store::open(&StorePath::new(&path).map_err(|e| e.to_string())?)
                    .map_err(|e| e.to_string())?;
                barrier.wait();
                match store.append_outcome_annotation(&annotation(trajectory, 1, suffix)) {
                    Ok(()) => Ok(()),
                    Err(error) if error.code() == "outcome_annotation_revision_invalid" => store
                        .append_outcome_annotation(&annotation(trajectory, 2, suffix))
                        .map_err(|error| error.to_string()),
                    Err(error) => Err(error.to_string()),
                }
            })
        })
        .collect();
    for worker in workers {
        worker.join().unwrap().unwrap();
    }

    let store = Store::open(&StorePath::new(&path).unwrap()).unwrap();
    let history = store.list_outcome_annotations(&trajectory).unwrap();
    assert_eq!(
        history
            .iter()
            .map(|record| record.body().revision().get())
            .collect::<Vec<_>>(),
        vec![1, 2]
    );
    assert_ne!(history[0].id(), history[1].id());
    assert_eq!(
        store
            .current_outcome_annotation(&trajectory)
            .unwrap()
            .unwrap()
            .body()
            .revision()
            .get(),
        2
    );
}
