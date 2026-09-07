#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
//! Frozen wire characterization for legacy Outcome v1.

use agent_jit_domain::envelope::Envelope;
use agent_jit_domain::outcome::{Outcome, OutcomeStatus};

const JSON: &str = r#"{"schema":"agent_jit.outcome","version":1,"id":"out_01J0000000000000000000000A","provenance":{"produced_by":"agent-jit/0.1.0","source":"confirmed","recorded_at_unix_ms":1756000000000,"parents":[]},"body":{"trajectory_id":"trj_01J0000000000000000000000A","status":"solved","note":null,"metrics":{"tool_calls":3,"agent_turns":2,"estimated_input_tokens":100,"estimated_output_tokens":20,"duration_ms":42000},"confirmed_by_human":true}}"#;
const DIGEST: &str = "84bcfbddde2d80e258a1baade4264103248ce1fbef01478ad6b2bf0e060ea77d";

#[test]
fn given_outcome_v1_when_round_tripped_then_wire_contract_is_unchanged() {
    // Given
    let outcome: Envelope<Outcome> = serde_json::from_str(JSON).unwrap();

    // When
    let encoded = serde_json::to_string(&outcome).unwrap();

    // Then
    assert_eq!(outcome.schema().as_str(), "agent_jit.outcome");
    assert_eq!(outcome.version().get(), 1);
    assert_eq!(outcome.body().status, OutcomeStatus::Solved);
    assert_eq!(encoded, JSON);
    assert_eq!(outcome.digest().unwrap().to_string(), DIGEST);
}

#[test]
fn given_legacy_outcome_statuses_then_only_exact_v1_vocabulary_is_accepted() {
    let statuses = [
        (OutcomeStatus::Solved, "solved"),
        (OutcomeStatus::Failed, "failed"),
        (OutcomeStatus::Abandoned, "abandoned"),
    ];

    for (status, wire) in statuses {
        assert_eq!(
            serde_json::to_value(status).unwrap(),
            serde_json::json!(wire)
        );
        assert_eq!(
            serde_json::from_value::<OutcomeStatus>(serde_json::json!(wire)).unwrap(),
            status
        );
    }

    for alias in ["succeeded", "success", "Solved", "FAILED", "cancelled", ""] {
        assert!(
            serde_json::from_value::<OutcomeStatus>(serde_json::json!(alias)).is_err(),
            "accepted {alias:?}"
        );
    }
}
