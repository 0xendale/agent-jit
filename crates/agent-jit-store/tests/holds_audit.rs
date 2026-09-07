#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
//! Active hold state and complete audit contracts.

mod support;

use std::sync::Arc;
use std::sync::atomic::{AtomicI64, Ordering};

use agent_jit_store::{Clock, PruneMode, RetentionPolicy, Store, StorePath};

use support::{DAY_MS, Fixture, NOW};

struct IncrementingClock(Arc<AtomicI64>);

impl Clock for IncrementingClock {
    fn now_unix_ms(&self) -> i64 {
        self.0.fetch_add(1, Ordering::SeqCst)
    }
}

#[test]
fn given_old_session_when_held_then_active_state_and_complete_audit_are_recorded() {
    // Given
    let temp = tempfile::tempdir().unwrap();
    let mut fixture = Fixture::new(temp.path());
    let (session, _) = fixture.add_session('A', NOW - 40 * DAY_MS);

    // When
    fixture
        .store
        .hold_session(&session, "operator", "phase0 corpus")
        .unwrap();

    // Then
    let hold = fixture.store.active_hold(&session).unwrap().unwrap();
    assert_eq!(hold.session_id, session);
    assert_eq!(hold.actor, "operator");
    assert_eq!(hold.rationale, "phase0 corpus");
    assert_eq!(hold.recorded_at_unix_ms, NOW);
    let audit = fixture.store.list_corpus_audit().unwrap();
    assert_eq!(audit.len(), 1);
    assert_eq!(audit[0].action.as_str(), "hold");
    assert_eq!(audit[0].session_id, Some(session));
    assert_eq!(audit[0].actor, "operator");
    assert_eq!(audit[0].rationale, "phase0 corpus");
    assert_eq!(audit[0].recorded_at_unix_ms, NOW);
}

#[test]
fn given_held_old_session_when_prune_applies_then_session_is_protected() {
    // Given
    let temp = tempfile::tempdir().unwrap();
    let mut fixture = Fixture::new(temp.path());
    let (session, _) = fixture.add_session('A', NOW - 40 * DAY_MS);
    fixture
        .store
        .hold_session(&session, "operator", "keep")
        .unwrap();

    // When
    let error = fixture
        .store
        .prune(
            RetentionPolicy::new(NOW, 30 * DAY_MS, u64::MAX).unwrap(),
            PruneMode::Apply,
        )
        .unwrap_err();

    // Then
    assert_eq!(error.code(), "retention_limit_unmet_protected");
    assert!(fixture.store.get_session(&session).unwrap().is_some());
}

#[test]
fn given_held_old_session_when_released_then_it_becomes_deletable_and_release_is_audited() {
    // Given
    let temp = tempfile::tempdir().unwrap();
    let mut fixture = Fixture::new(temp.path());
    let (session, _) = fixture.add_session('A', NOW - 40 * DAY_MS);
    fixture
        .store
        .hold_session(&session, "operator", "keep")
        .unwrap();

    // When
    fixture
        .store
        .release_session(&session, "reviewer", "complete")
        .unwrap();

    // Then
    assert!(fixture.store.active_hold(&session).unwrap().is_none());
    let audit = fixture.store.list_corpus_audit().unwrap();
    assert_eq!(audit.len(), 2);
    assert_eq!(audit[1].action.as_str(), "release");
    assert_eq!(audit[1].session_id, Some(session));
    assert_eq!(audit[1].actor, "reviewer");
    assert_eq!(audit[1].rationale, "complete");
    assert_eq!(audit[1].recorded_at_unix_ms, NOW);
}

#[test]
fn given_released_old_session_when_prune_applies_then_session_is_deleted() {
    // Given
    let temp = tempfile::tempdir().unwrap();
    let mut fixture = Fixture::new(temp.path());
    let (session, _) = fixture.add_session('A', NOW - 40 * DAY_MS);
    fixture
        .store
        .hold_session(&session, "operator", "keep")
        .unwrap();
    fixture
        .store
        .release_session(&session, "operator", "complete")
        .unwrap();

    // When
    let pruned = fixture
        .store
        .prune(
            RetentionPolicy::new(NOW, 30 * DAY_MS, u64::MAX).unwrap(),
            PruneMode::Apply,
        )
        .unwrap();

    // Then
    assert_eq!(pruned.deleted_sessions, vec![session]);
}

#[test]
fn given_transition_when_clock_advances_per_read_then_state_and_audit_share_one_timestamp() {
    let temp = tempfile::tempdir().unwrap();
    let path = temp.path().join("state.sqlite3");
    let mut fixture = Fixture::new(temp.path());
    let (session, _) = fixture.add_session('A', NOW);
    drop(fixture.store);
    let next = Arc::new(AtomicI64::new(NOW));
    let mut store = Store::open(&StorePath::new(&path).unwrap())
        .unwrap()
        .with_clock(IncrementingClock(Arc::clone(&next)));

    store.hold_session(&session, "operator", "keep").unwrap();

    let hold = store.active_hold(&session).unwrap().unwrap();
    let audit = store.list_corpus_audit().unwrap();
    assert_eq!(hold.recorded_at_unix_ms, audit[0].recorded_at_unix_ms);
    assert_eq!(next.load(Ordering::SeqCst), NOW + 1);
}

#[test]
fn given_identical_and_changed_holds_then_only_state_transitions_are_audited() {
    let temp = tempfile::tempdir().unwrap();
    let mut fixture = Fixture::new(temp.path());
    let (session, _) = fixture.add_session('A', NOW);

    fixture
        .store
        .hold_session(&session, "first", "keep")
        .unwrap();
    fixture
        .store
        .hold_session(&session, "first", "keep")
        .unwrap();
    assert_eq!(fixture.store.list_corpus_audit().unwrap().len(), 1);

    fixture
        .store
        .hold_session(&session, "second", "keep")
        .unwrap();
    assert_eq!(
        fixture.store.active_hold(&session).unwrap().unwrap().actor,
        "second"
    );
    assert_eq!(fixture.store.list_corpus_audit().unwrap().len(), 2);

    fixture
        .store
        .hold_session(&session, "second", "replace")
        .unwrap();
    let hold = fixture.store.active_hold(&session).unwrap().unwrap();
    assert_eq!(
        (hold.actor.as_str(), hold.rationale.as_str()),
        ("second", "replace")
    );
    let audit = fixture.store.list_corpus_audit().unwrap();
    assert_eq!(audit.len(), 3);
    assert_eq!(
        (audit[2].actor.as_str(), audit[2].rationale.as_str()),
        ("second", "replace")
    );

    fixture
        .store
        .release_session(&session, "second", "done")
        .unwrap();
    fixture
        .store
        .release_session(&session, "second", "again")
        .unwrap();
    assert_eq!(fixture.store.list_corpus_audit().unwrap().len(), 4);
}
