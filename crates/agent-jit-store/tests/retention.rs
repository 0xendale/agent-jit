#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
//! Logical-byte, age, dry-run, cleanup, and transactional retention contracts.

mod support;

use agent_jit_domain::envelope::{Envelope, Provenance};
use agent_jit_domain::ids::{OutcomeAnnotationId, OutcomeId, TrajectoryId};
use agent_jit_domain::outcome::{
    AnnotationRevision, AnnotationStatus, EvidenceRef, OutcomeAnnotation,
};
use agent_jit_store::{FixedClock, PruneMode, RetentionPolicy, Store, StorePath};
use rusqlite::Connection;

use support::{DAY_MS, Fixture, NOW};

fn annotation(trajectory: TrajectoryId) -> Envelope<OutcomeAnnotation> {
    let id: OutcomeAnnotationId = "oan_01J0000000000000000000000A".parse().unwrap();
    let body = OutcomeAnnotation::new(
        trajectory,
        AnnotationRevision::new(1).unwrap(),
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
fn given_default_policy_when_inspected_then_limits_are_thirty_days_and_one_gibibyte() {
    // Given
    let policy = RetentionPolicy::default_at(NOW);

    // When
    let limits = (policy.max_age_ms(), policy.max_bytes());

    // Then
    assert_eq!(limits, (30 * DAY_MS, 1_073_741_824));
}

#[test]
fn given_overflowing_age_arithmetic_when_policy_is_checked_then_it_is_refused() {
    // Given / When
    let error = RetentionPolicy::checked(i64::MIN, 1, u64::MAX).unwrap_err();

    // Then
    assert_eq!(error.code(), "retention_age_overflow");
}

#[test]
fn given_negative_and_zero_age_when_constructed_then_only_negative_is_refused() {
    assert_eq!(
        RetentionPolicy::new(NOW, -1, u64::MAX).unwrap_err().code(),
        "retention_age_invalid"
    );
    assert_eq!(
        RetentionPolicy::new(NOW, 0, u64::MAX).unwrap().max_age_ms(),
        0
    );
}

#[test]
fn given_age_excess_when_dry_run_then_oldest_is_selected_without_logical_mutation() {
    // Given
    let temp = tempfile::tempdir().unwrap();
    let mut fixture = Fixture::new(temp.path());
    let (old, _) = fixture.add_session('A', NOW - 40 * DAY_MS);
    fixture.add_session('B', NOW - DAY_MS);
    let before = fixture.store.evidence_digest().unwrap();

    // When
    let report = fixture
        .store
        .prune(
            RetentionPolicy::new(NOW, 30 * DAY_MS, u64::MAX).unwrap(),
            PruneMode::DryRun,
        )
        .unwrap();

    // Then
    assert_eq!(report.selected_sessions, vec![old]);
    assert!(report.deleted_sessions.is_empty());
    assert!(fixture.store.list_corpus_audit().unwrap().is_empty());
    assert_eq!(fixture.store.evidence_digest().unwrap(), before);
}

#[test]
fn given_equal_age_sessions_when_bytes_require_one_deletion_then_id_breaks_the_tie() {
    // Given
    let temp = tempfile::tempdir().unwrap();
    let mut fixture = Fixture::new(temp.path());
    let (higher_id, _) = fixture.add_session('B', NOW - DAY_MS);
    let (lower_id, _) = fixture.add_session('A', NOW - DAY_MS);
    let usage = fixture.store.retention_usage().unwrap().logical_bytes;
    let cap = usage - fixture.store.session_logical_bytes(&lower_id).unwrap();

    // When
    let report = fixture
        .store
        .prune(
            RetentionPolicy::new(NOW, NOW, cap).unwrap(),
            PruneMode::DryRun,
        )
        .unwrap();

    // Then
    assert_eq!(report.selected_sessions, vec![lower_id]);
    assert!(fixture.store.get_session(&higher_id).unwrap().is_some());
}

#[test]
fn given_equivalent_json_formatting_when_accounted_then_logical_size_and_digest_are_stable() {
    // Given
    let temp = tempfile::tempdir().unwrap();
    let path = temp.path().join("state.sqlite3");
    let mut fixture = Fixture::new(temp.path());
    let (session, _) = fixture.add_session('A', NOW);
    let size = fixture.store.session_logical_bytes(&session).unwrap();
    let digest = fixture.store.evidence_digest().unwrap();
    drop(fixture.store);
    let connection = Connection::open(&path).unwrap();
    let json: String = connection
        .query_row(
            "SELECT record_json FROM sessions WHERE session_id=?1",
            [session.to_string()],
            |row| row.get(0),
        )
        .unwrap();
    let value: serde_json::Value = serde_json::from_str(&json).unwrap();
    connection
        .execute(
            "UPDATE sessions SET record_json=?1 WHERE session_id=?2",
            [
                serde_json::to_string_pretty(&value).unwrap(),
                session.to_string(),
            ],
        )
        .unwrap();
    drop(connection);
    let store = Store::open(&StorePath::new(&path).unwrap()).unwrap();

    // When / Then
    assert_eq!(store.session_logical_bytes(&session).unwrap(), size);
    assert_eq!(store.evidence_digest().unwrap(), digest);
}

#[test]
fn given_byte_excess_when_applied_then_oldest_is_deleted_and_usage_meets_cap() {
    // Given
    let temp = tempfile::tempdir().unwrap();
    let mut fixture = Fixture::new(temp.path());
    let (old, _) = fixture.add_session('A', NOW - 20 * DAY_MS);
    let (newest, _) = fixture.add_session('B', NOW - DAY_MS);
    let usage = fixture.store.retention_usage().unwrap().logical_bytes;
    let cap = usage - fixture.store.session_logical_bytes(&old).unwrap();

    // When
    let report = fixture
        .store
        .prune(
            RetentionPolicy::new(NOW, NOW, cap).unwrap(),
            PruneMode::Apply,
        )
        .unwrap();

    // Then
    assert_eq!(report.deleted_sessions, vec![old]);
    assert!(fixture.store.get_session(&newest).unwrap().is_some());
    assert!(fixture.store.retention_usage().unwrap().logical_bytes <= cap);
}

#[test]
fn given_joint_age_and_byte_excess_when_applied_then_each_limit_contributes_oldest_first() {
    // Given
    let temp = tempfile::tempdir().unwrap();
    let mut fixture = Fixture::new(temp.path());
    let (age_old, _) = fixture.add_session('A', NOW - 40 * DAY_MS);
    let (byte_old, _) = fixture.add_session('B', NOW - 20 * DAY_MS);
    let (newest, _) = fixture.add_session('C', NOW - DAY_MS);
    let usage = fixture.store.retention_usage().unwrap().logical_bytes;
    let cap = usage
        - fixture.store.session_logical_bytes(&age_old).unwrap()
        - fixture.store.session_logical_bytes(&byte_old).unwrap();

    // When
    let report = fixture
        .store
        .prune(
            RetentionPolicy::new(NOW, 30 * DAY_MS, cap).unwrap(),
            PruneMode::Apply,
        )
        .unwrap();

    // Then
    assert_eq!(report.deleted_sessions, vec![age_old, byte_old]);
    assert!(fixture.store.get_session(&newest).unwrap().is_some());
    assert!(fixture.store.retention_usage().unwrap().logical_bytes <= cap);
}

#[test]
fn given_full_session_unit_when_applied_then_every_child_record_is_removed() {
    // Given
    let temp = tempfile::tempdir().unwrap();
    let mut fixture = Fixture::new(temp.path());
    let (session, trajectory) = fixture.add_session('A', NOW - 40 * DAY_MS);
    fixture.add_children('A', session, trajectory);
    fixture
        .store
        .append_outcome_annotation(&annotation(trajectory))
        .unwrap();
    let outcome: OutcomeId = "out_01J0000000000000000000000A".parse().unwrap();

    // When
    let report = fixture
        .store
        .prune(
            RetentionPolicy::new(NOW, 30 * DAY_MS, u64::MAX).unwrap(),
            PruneMode::Apply,
        )
        .unwrap();

    // Then
    assert_eq!(report.deleted_sessions, vec![session]);
    assert!(fixture.store.get_session(&session).unwrap().is_none());
    assert!(fixture.store.list_events(&session).unwrap().is_empty());
    assert!(fixture.store.get_trajectory(&trajectory).unwrap().is_none());
    assert!(fixture.store.get_outcome(&outcome).unwrap().is_none());
    assert!(
        fixture
            .store
            .get_trace_metrics(&trajectory)
            .unwrap()
            .is_none()
    );
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
fn given_invalid_size_accounting_when_applied_then_evidence_is_kept() {
    // Given
    let temp = tempfile::tempdir().unwrap();
    let path = temp.path().join("state.sqlite3");
    let mut fixture = Fixture::new(temp.path());
    let (session, trajectory) = fixture.add_session('A', NOW - 40 * DAY_MS);
    fixture.add_children('A', session, trajectory);
    drop(fixture.store);
    let connection = Connection::open(&path).unwrap();
    connection
        .execute("UPDATE events SET payload_bytes=-1", [])
        .unwrap();
    drop(connection);
    let mut store = Store::open(&StorePath::new(&path).unwrap())
        .unwrap()
        .with_clock(FixedClock::at(NOW));
    let before = store.evidence_digest().unwrap();

    // When
    let error = store
        .prune(
            RetentionPolicy::new(NOW, 30 * DAY_MS, u64::MAX).unwrap(),
            PruneMode::Apply,
        )
        .unwrap_err();

    // Then
    assert_eq!(error.code(), "retention_size_unavailable");
    assert_eq!(store.evidence_digest().unwrap(), before);
    assert!(store.get_session(&session).unwrap().is_some());
}

#[test]
fn given_positive_payload_index_mismatch_when_applied_then_evidence_is_kept() {
    let temp = tempfile::tempdir().unwrap();
    let path = temp.path().join("state.sqlite3");
    let mut fixture = Fixture::new(temp.path());
    let (session, trajectory) = fixture.add_session('A', NOW - 40 * DAY_MS);
    fixture.add_children('A', session, trajectory);
    drop(fixture.store);
    let connection = Connection::open(&path).unwrap();
    connection
        .execute("UPDATE events SET payload_bytes=11", [])
        .unwrap();
    drop(connection);
    let mut store = Store::open(&StorePath::new(&path).unwrap())
        .unwrap()
        .with_clock(FixedClock::at(NOW));
    let before = store.evidence_digest().unwrap();

    let error = store
        .prune(
            RetentionPolicy::new(NOW, 30 * DAY_MS, u64::MAX).unwrap(),
            PruneMode::Apply,
        )
        .unwrap_err();

    assert_eq!(error.code(), "retention_size_unavailable");
    assert_eq!(store.evidence_digest().unwrap(), before);
    assert!(store.get_session(&session).unwrap().is_some());
}

#[test]
fn given_dry_run_plan_when_hold_arrives_then_apply_recomputes_and_keeps_session() {
    let temp = tempfile::tempdir().unwrap();
    let mut fixture = Fixture::new(temp.path());
    let (session, _) = fixture.add_session('A', NOW - 40 * DAY_MS);
    let policy = RetentionPolicy::new(NOW, 30 * DAY_MS, u64::MAX).unwrap();
    let plan = fixture.store.prune(policy, PruneMode::DryRun).unwrap();
    assert_eq!(plan.selected_sessions, vec![session]);
    fixture
        .store
        .hold_session(&session, "operator", "race")
        .unwrap();
    let digest = fixture.store.evidence_digest().unwrap();
    let audit = fixture.store.list_corpus_audit().unwrap();

    let error = fixture.store.prune(policy, PruneMode::Apply).unwrap_err();

    assert_eq!(error.code(), "retention_limit_unmet_protected");
    assert_eq!(fixture.store.evidence_digest().unwrap(), digest);
    assert_eq!(fixture.store.list_corpus_audit().unwrap(), audit);
    assert!(fixture.store.get_session(&session).unwrap().is_some());
}

#[test]
fn given_protected_only_byte_excess_when_applied_then_typed_refusal_keeps_evidence() {
    let temp = tempfile::tempdir().unwrap();
    let mut fixture = Fixture::new(temp.path());
    let (session, _) = fixture.add_session('A', NOW - DAY_MS);
    fixture
        .store
        .hold_session(&session, "operator", "keep")
        .unwrap();
    let usage = fixture.store.retention_usage().unwrap().logical_bytes;
    let digest = fixture.store.evidence_digest().unwrap();
    let audit = fixture.store.list_corpus_audit().unwrap();

    let error = fixture
        .store
        .prune(
            RetentionPolicy::new(NOW, 30 * DAY_MS, usage - 1).unwrap(),
            PruneMode::Apply,
        )
        .unwrap_err();

    assert_eq!(error.code(), "retention_limit_unmet_protected");
    assert_eq!(fixture.store.evidence_digest().unwrap(), digest);
    assert_eq!(fixture.store.list_corpus_audit().unwrap(), audit);
}

#[test]
fn given_malformed_record_row_when_digesting_then_validation_fails_closed() {
    let temp = tempfile::tempdir().unwrap();
    let path = temp.path().join("state.sqlite3");
    let mut fixture = Fixture::new(temp.path());
    fixture.add_session('A', NOW);
    drop(fixture.store);
    let connection = Connection::open(&path).unwrap();
    connection
        .execute("UPDATE sessions SET record_json='not-json'", [])
        .unwrap();
    drop(connection);
    let store = Store::open(&StorePath::new(&path).unwrap()).unwrap();

    assert_eq!(
        store.evidence_digest().unwrap_err().code(),
        "store_record_invalid"
    );
}

#[test]
fn given_prune_audit_insert_failure_when_applied_then_audit_and_all_deletes_roll_back() {
    // Given
    let temp = tempfile::tempdir().unwrap();
    let path = temp.path().join("state.sqlite3");
    let mut fixture = Fixture::new(temp.path());
    let (session, trajectory) = fixture.add_session('A', NOW - 40 * DAY_MS);
    fixture.add_children('A', session, trajectory);
    let before = fixture.store.evidence_digest().unwrap();
    let audit_before = fixture.store.list_corpus_audit().unwrap();
    drop(fixture.store);
    let connection = Connection::open(&path).unwrap();
    connection
        .execute_batch(
            "CREATE TABLE prune_test_observation (deletes INTEGER NOT NULL); \
             INSERT INTO prune_test_observation VALUES (0); \
             CREATE TRIGGER observe_trajectory_delete AFTER DELETE ON trajectories \
             BEGIN UPDATE prune_test_observation SET deletes = 1; END; \
             CREATE TRIGGER reject_prune_audit BEFORE INSERT ON corpus_audit \
             WHEN NEW.action = 'prune' \
              AND (SELECT deletes FROM prune_test_observation) = 1 \
             BEGIN SELECT RAISE(ABORT, 'injected audit failure'); END;",
        )
        .unwrap();
    drop(connection);
    let mut store = Store::open(&StorePath::new(&path).unwrap())
        .unwrap()
        .with_clock(FixedClock::at(NOW));

    // When
    let result = store.prune(
        RetentionPolicy::new(NOW, 30 * DAY_MS, u64::MAX).unwrap(),
        PruneMode::Apply,
    );

    // Then
    assert!(result.is_err());
    assert_eq!(store.evidence_digest().unwrap(), before);
    assert_eq!(store.list_corpus_audit().unwrap(), audit_before);
    assert!(store.get_session(&session).unwrap().is_some());
    assert!(store.get_trajectory(&trajectory).unwrap().is_some());
    drop(store);
    let connection = Connection::open(path).unwrap();
    let rolled_back_deletes: i64 = connection
        .query_row("SELECT deletes FROM prune_test_observation", [], |row| {
            row.get(0)
        })
        .unwrap();
    assert_eq!(rolled_back_deletes, 0);
}
