#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
//! Applied pruning protection from typed, persisted evidence references.

mod support;

use agent_jit_domain::benchmark::{BenchmarkCondition, BenchmarkRecord, CorrectnessVerdict};
use agent_jit_domain::candidate::{CandidateContract, Group, ProjectedValue};
use agent_jit_domain::canonical::digest_of;
use agent_jit_domain::capability::{ReplayResult, ReplayVerdict, Workflow};
use agent_jit_domain::envelope::{Envelope, Provenance};
use agent_jit_domain::ids::{
    BenchmarkId, CandidateId, FrozenCorpusId, GroupId, ReplayId, WorkflowId,
};
use agent_jit_domain::lifecycle::LifecycleState;
use agent_jit_domain::outcome::CostMetrics;
use agent_jit_store::{PruneMode, RetentionPolicy, StoreError};

use support::{DAY_MS, Fixture, NOW};

fn digest(label: &str) -> agent_jit_domain::canonical::Digest {
    digest_of(&serde_json::json!(label)).unwrap()
}

fn apply_prune(fixture: &mut Fixture) -> StoreError {
    fixture
        .store
        .prune(
            RetentionPolicy::new(NOW, 30 * DAY_MS, u64::MAX).unwrap(),
            PruneMode::Apply,
        )
        .unwrap_err()
}

fn put_empty_group(fixture: &mut Fixture, suffix: char) -> GroupId {
    let id: GroupId = format!("grp_01J0000000000000000000000{suffix}")
        .parse()
        .unwrap();
    fixture
        .store
        .put_group(&Envelope::new(
            id,
            Provenance::recorded_by("agent-jit/0.1.0"),
            Group {
                repository_id: fixture.repository_id,
                intent_label: "intent".to_owned(),
                members: Vec::new(),
                rationale: "frozen".to_owned(),
                manifest_digest: digest(&format!("group-{suffix}")),
            },
        ))
        .unwrap();
    id
}

fn put_candidate(fixture: &mut Fixture, group_id: GroupId) -> CandidateId {
    let id: CandidateId = "cnd_01J0000000000000000000000A".parse().unwrap();
    fixture
        .store
        .put_candidate(&Envelope::new(
            id,
            Provenance::recorded_by("agent-jit/0.1.0"),
            CandidateContract {
                group_id,
                name: "fixture".to_owned(),
                inputs: Vec::new(),
                outputs: Vec::new(),
                preconditions: Vec::new(),
                projected: ProjectedValue {
                    observed_uses: 1,
                    median_tool_calls: 1,
                    median_input_tokens: 1,
                    median_duration_ms: 1,
                    compile_cost_uses: 1,
                    break_even_uses: 1,
                },
                state: LifecycleState::Proposed,
                confirmed_by_human: true,
            },
        ))
        .unwrap();
    id
}

#[test]
fn given_frozen_corpus_reference_when_prune_applies_then_session_is_protected() {
    // Given
    let temp = tempfile::tempdir().unwrap();
    let mut fixture = Fixture::new(temp.path());
    let (session, trajectory) = fixture.add_session('A', NOW - 40 * DAY_MS);
    let corpus: FrozenCorpusId = "cor_01J0000000000000000000000A".parse().unwrap();
    fixture
        .store
        .add_frozen_corpus_reference(&corpus, &trajectory)
        .unwrap();

    // When
    let error = apply_prune(&mut fixture);

    // Then
    assert_eq!(error.code(), "retention_limit_unmet_protected");
    assert!(fixture.store.get_session(&session).unwrap().is_some());
}

#[test]
fn given_real_group_member_when_prune_applies_then_session_is_protected() {
    // Given
    let temp = tempfile::tempdir().unwrap();
    let mut fixture = Fixture::new(temp.path());
    let (session, trajectory) = fixture.add_session('A', NOW - 40 * DAY_MS);
    let group_id: GroupId = "grp_01J0000000000000000000000A".parse().unwrap();
    fixture
        .store
        .put_group(&Envelope::new(
            group_id,
            Provenance::recorded_by("agent-jit/0.1.0"),
            Group {
                repository_id: fixture.repository_id,
                intent_label: "intent".to_owned(),
                members: vec![trajectory],
                rationale: "frozen".to_owned(),
                manifest_digest: digest("group"),
            },
        ))
        .unwrap();

    // When
    let error = apply_prune(&mut fixture);

    // Then
    assert_eq!(error.code(), "retention_limit_unmet_protected");
    assert!(fixture.store.get_session(&session).unwrap().is_some());
}

#[test]
fn given_real_candidate_reference_when_prune_applies_then_session_is_protected() {
    // Given
    let temp = tempfile::tempdir().unwrap();
    let mut fixture = Fixture::new(temp.path());
    let (session, trajectory) = fixture.add_session('A', NOW - 40 * DAY_MS);
    let group = put_empty_group(&mut fixture, 'A');
    let candidate = put_candidate(&mut fixture, group);
    fixture
        .store
        .link_candidate_trajectory(&candidate, &trajectory)
        .unwrap();

    // When
    let error = apply_prune(&mut fixture);

    // Then
    assert_eq!(error.code(), "retention_limit_unmet_protected");
    assert!(fixture.store.get_session(&session).unwrap().is_some());
}

#[test]
fn given_real_replay_fixture_when_prune_applies_then_session_is_protected() {
    // Given
    let temp = tempfile::tempdir().unwrap();
    let mut fixture = Fixture::new(temp.path());
    let (session, trajectory) = fixture.add_session('A', NOW - 40 * DAY_MS);
    let group = put_empty_group(&mut fixture, 'A');
    let candidate = put_candidate(&mut fixture, group);
    let workflow_id: WorkflowId = "wfl_01J0000000000000000000000A".parse().unwrap();
    fixture
        .store
        .put_workflow(&Envelope::new(
            workflow_id,
            Provenance::recorded_by("agent-jit/0.1.0"),
            Workflow {
                candidate_id: candidate,
                ir_version: 1,
                ir_digest: digest("ir"),
                primitives: Vec::new(),
                node_count: 0,
            },
        ))
        .unwrap();
    let replay_id: ReplayId = "rpl_01J0000000000000000000000A".parse().unwrap();
    fixture
        .store
        .put_replay(&Envelope::new(
            replay_id,
            Provenance::recorded_by("agent-jit/0.1.0"),
            ReplayResult {
                workflow_id,
                fixture_trajectory_id: trajectory,
                verdict: ReplayVerdict::Match,
                comparator: "exact".to_owned(),
                reason: None,
                observed_digest: Some(digest("observed")),
                expected_digest: Some(digest("observed")),
                deterministic: true,
            },
        ))
        .unwrap();

    // When
    let error = apply_prune(&mut fixture);

    // Then
    assert_eq!(error.code(), "retention_limit_unmet_protected");
    assert!(fixture.store.get_session(&session).unwrap().is_some());
}

#[test]
fn given_real_benchmark_reference_when_prune_applies_then_session_is_protected() {
    // Given
    let temp = tempfile::tempdir().unwrap();
    let mut fixture = Fixture::new(temp.path());
    let (session, trajectory) = fixture.add_session('A', NOW - 40 * DAY_MS);
    let benchmark: BenchmarkId = "bmk_01J0000000000000000000000A".parse().unwrap();
    fixture
        .store
        .put_benchmark(&Envelope::new(
            benchmark,
            Provenance::recorded_by("agent-jit/0.1.0"),
            BenchmarkRecord {
                repository_id: fixture.repository_id,
                task_id: "task".to_owned(),
                condition: BenchmarkCondition::Baseline,
                order_index: 0,
                head_commit: "0".repeat(40),
                model_id: "model".to_owned(),
                metrics: CostMetrics {
                    tool_calls: 1,
                    agent_turns: 1,
                    estimated_input_tokens: 1,
                    estimated_output_tokens: 1,
                    duration_ms: 1,
                },
                correctness: CorrectnessVerdict::Correct,
                capability_id: None,
                capability_executed: false,
            },
        ))
        .unwrap();
    fixture
        .store
        .link_benchmark_trajectory(&benchmark, &trajectory)
        .unwrap();

    // When
    let error = apply_prune(&mut fixture);

    // Then
    assert_eq!(error.code(), "retention_limit_unmet_protected");
    assert!(fixture.store.get_session(&session).unwrap().is_some());
}
