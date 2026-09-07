#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
//! Future append-only `OutcomeAnnotation` v1 wire contract.

use agent_jit_domain::envelope::{Envelope, Provenance};
use agent_jit_domain::ids::{OutcomeAnnotationId, TrajectoryId};
use agent_jit_domain::outcome::{
    AnnotationRevision, AnnotationStatus, EvidenceRef, OutcomeAnnotation,
};

fn trajectory_id() -> TrajectoryId {
    "trj_01J0000000000000000000000A".parse().unwrap()
}

#[test]
fn given_supported_statuses_when_serialized_then_vocabulary_is_exact() {
    // Given
    let statuses = [
        AnnotationStatus::Succeeded,
        AnnotationStatus::Failed,
        AnnotationStatus::Abandoned,
        AnnotationStatus::Unknown,
    ];

    // When
    let values: Vec<String> = statuses
        .iter()
        .map(|status| serde_json::to_string(status).unwrap())
        .collect();

    // Then
    assert_eq!(
        values,
        [
            "\"succeeded\"",
            "\"failed\"",
            "\"abandoned\"",
            "\"unknown\""
        ]
    );
}

#[test]
fn given_unsupported_status_spellings_when_deserialized_then_each_is_rejected() {
    // Given
    let invalid = [
        "solved",
        "success",
        "successful",
        "succeed",
        "Succeeded",
        "FAILED",
        "Unknown",
        "cancelled",
        "",
        "anything",
    ];

    // When / Then
    for value in invalid {
        assert!(
            serde_json::from_value::<AnnotationStatus>(serde_json::json!(value)).is_err(),
            "accepted {value:?}"
        );
    }
}

#[test]
fn given_annotation_when_enveloped_then_every_v1_field_is_serialized() {
    // Given
    let annotation = OutcomeAnnotation::new(
        trajectory_id(),
        AnnotationRevision::new(1).unwrap(),
        AnnotationStatus::Succeeded,
        "operator",
        1_756_000_000_000,
        "review passed",
        vec![EvidenceRef::new("artifacts/result.json").unwrap()],
    )
    .unwrap();
    let id: OutcomeAnnotationId = "oan_01J0000000000000000000000A".parse().unwrap();
    let envelope = Envelope::new(id, Provenance::recorded_by("agent-jit/0.1.0"), annotation);

    // When
    let value = serde_json::to_value(envelope).unwrap();

    // Then
    assert_eq!(
        value,
        serde_json::json!({
            "schema": "agent_jit.outcome_annotation",
            "version": 1,
            "id": "oan_01J0000000000000000000000A",
            "provenance": {
                "produced_by": "agent-jit/0.1.0",
                "source": "recorded",
                "recorded_at_unix_ms": 0,
                "parents": []
            },
            "body": {
                "trajectory_id": "trj_01J0000000000000000000000A",
                "revision": 1,
                "status": "succeeded",
                "actor": "operator",
                "annotated_at_unix_ms": 1_756_000_000_000_i64,
                "rationale": "review passed",
                "evidence": ["artifacts/result.json"]
            }
        })
    );
}

#[test]
fn given_revision_before_max_when_advanced_then_max_is_returned() {
    // Given
    let revision = AnnotationRevision::new(u32::MAX - 1).unwrap();

    // When
    let next = revision.next().unwrap();

    // Then
    assert_eq!(next.get(), u32::MAX);
}

#[test]
fn given_max_revision_when_advanced_then_overflow_is_refused() {
    // Given
    let revision = AnnotationRevision::new(u32::MAX).unwrap();

    // When
    let error = revision.next().unwrap_err();

    // Then
    assert_eq!(error.code(), "outcome_annotation_revision_overflow");
}

#[test]
fn given_zero_revision_when_constructed_then_it_is_refused() {
    // Given / When
    let error = AnnotationRevision::new(0).unwrap_err();

    // Then
    assert_eq!(error.code(), "outcome_annotation_revision_zero");
}

#[test]
fn given_out_of_range_revisions_when_deserialized_then_they_are_refused() {
    for value in [
        serde_json::json!(0),
        serde_json::json!(u64::from(u32::MAX) + 1),
    ] {
        assert!(
            serde_json::from_value::<AnnotationRevision>(value.clone()).is_err(),
            "accepted {value}"
        );

        let mut annotation = serde_json::json!({
            "trajectory_id": "trj_01J0000000000000000000000A",
            "revision": 1,
            "status": "unknown",
            "actor": "operator",
            "annotated_at_unix_ms": 1,
            "rationale": "reviewed",
            "evidence": []
        });
        annotation["revision"] = value.clone();
        assert!(
            serde_json::from_value::<OutcomeAnnotation>(annotation).is_err(),
            "aggregate accepted {value}"
        );
    }
}

#[test]
fn given_annotation_when_read_then_getters_expose_every_field() {
    let annotation = OutcomeAnnotation::new(
        trajectory_id(),
        AnnotationRevision::new(2).unwrap(),
        AnnotationStatus::Failed,
        "operator",
        42,
        "reviewed",
        vec![EvidenceRef::new("evidence/result.json").unwrap()],
    )
    .unwrap();

    assert_eq!(annotation.trajectory_id(), trajectory_id());
    assert_eq!(annotation.revision().get(), 2);
    assert_eq!(annotation.status(), AnnotationStatus::Failed);
    assert_eq!(annotation.actor(), "operator");
    assert_eq!(annotation.annotated_at_unix_ms(), 42);
    assert_eq!(annotation.rationale(), "reviewed");
    assert_eq!(annotation.evidence()[0].as_str(), "evidence/result.json");
}

#[test]
fn given_only_annotation_time_changes_then_digest_is_stable_but_raw_json_retains_time() {
    let build = |annotated_at_unix_ms| {
        Envelope::new(
            "oan_01J0000000000000000000000A".parse().unwrap(),
            Provenance::recorded_by("agent-jit/0.1.0"),
            OutcomeAnnotation::new(
                trajectory_id(),
                AnnotationRevision::new(1).unwrap(),
                AnnotationStatus::Succeeded,
                "operator",
                annotated_at_unix_ms,
                "reviewed",
                Vec::new(),
            )
            .unwrap(),
        )
    };
    let first = build(1);
    let second = build(2);

    assert_eq!(first.digest().unwrap(), second.digest().unwrap());
    assert_ne!(
        serde_json::to_string(&first).unwrap(),
        serde_json::to_string(&second).unwrap()
    );
    assert_eq!(
        serde_json::to_value(second).unwrap()["body"]["annotated_at_unix_ms"],
        2
    );
}
