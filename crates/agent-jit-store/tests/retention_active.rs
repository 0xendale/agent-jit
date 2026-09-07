#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
//! Active-session retention protection.

mod support;

use agent_jit_store::{PruneMode, RetentionPolicy};

use support::{DAY_MS, Fixture, NOW};

#[test]
fn active_session_older_than_age_limit_is_never_selected() {
    // Given
    let temp = tempfile::tempdir().unwrap();
    let mut fixture = Fixture::new(temp.path());
    let (active, _) = fixture.add_active_session('A', NOW - 40 * DAY_MS);
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
    assert!(report.deleted_sessions.is_empty());
    assert!(fixture.store.get_session(&active).unwrap().is_some());
    assert_eq!(fixture.store.evidence_digest().unwrap(), before);
    assert!(fixture.store.list_corpus_audit().unwrap().is_empty());
}

#[test]
fn active_session_bytes_over_cap_cause_typed_refusal_without_mutation_or_audit() {
    // Given
    let temp = tempfile::tempdir().unwrap();
    let mut fixture = Fixture::new(temp.path());
    let (active, _) = fixture.add_active_session('A', NOW - 40 * DAY_MS);
    let usage = fixture.store.retention_usage().unwrap().logical_bytes;
    let before = fixture.store.evidence_digest().unwrap();

    // When
    let error = fixture
        .store
        .prune(
            RetentionPolicy::new(NOW, NOW, usage - 1).unwrap(),
            PruneMode::Apply,
        )
        .unwrap_err();

    // Then
    assert_eq!(error.code(), "retention_limit_unmet_protected");
    assert!(fixture.store.get_session(&active).unwrap().is_some());
    assert_eq!(fixture.store.evidence_digest().unwrap(), before);
    assert!(fixture.store.list_corpus_audit().unwrap().is_empty());
}
