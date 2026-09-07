#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
//! `OutcomeAnnotation` validation at constructor, builder, and serde boundaries.

use agent_jit_domain::ids::TrajectoryId;
use agent_jit_domain::outcome::{
    AnnotationRevision, AnnotationStatus, EvidenceRef, MAX_EVIDENCE_REFS, OutcomeAnnotation,
};

fn trajectory() -> TrajectoryId {
    "trj_01J0000000000000000000000A".parse().unwrap()
}

fn refs(count: usize) -> Vec<EvidenceRef> {
    (0..count)
        .map(|index| EvidenceRef::new(&format!("evidence/{index}.json")).unwrap())
        .collect()
}

fn annotation() -> OutcomeAnnotation {
    OutcomeAnnotation::new(
        trajectory(),
        AnnotationRevision::new(1).unwrap(),
        AnnotationStatus::Succeeded,
        "operator",
        1,
        "reviewed",
        Vec::new(),
    )
    .unwrap()
}

#[test]
fn given_exact_max_evidence_when_constructed_then_it_succeeds() {
    // Given
    let evidence = refs(MAX_EVIDENCE_REFS);

    // When
    let result = OutcomeAnnotation::new(
        trajectory(),
        AnnotationRevision::new(1).unwrap(),
        AnnotationStatus::Succeeded,
        "operator",
        1,
        "reviewed",
        evidence,
    );

    // Then
    assert_eq!(result.unwrap().evidence().len(), MAX_EVIDENCE_REFS);
}

#[test]
fn given_max_plus_one_evidence_when_constructed_then_it_is_refused() {
    // Given
    let evidence = refs(MAX_EVIDENCE_REFS + 1);

    // When
    let error = OutcomeAnnotation::new(
        trajectory(),
        AnnotationRevision::new(1).unwrap(),
        AnnotationStatus::Succeeded,
        "operator",
        1,
        "reviewed",
        evidence,
    )
    .unwrap_err();

    // Then
    assert_eq!(error.code(), "outcome_annotation_too_many_evidence");
}

#[test]
fn given_exact_max_evidence_when_added_by_builder_then_it_succeeds() {
    // Given
    let evidence = refs(MAX_EVIDENCE_REFS);

    // When
    let result = annotation().with_evidence(evidence).unwrap();

    // Then
    assert_eq!(result.evidence().len(), MAX_EVIDENCE_REFS);
}

#[test]
fn given_max_plus_one_evidence_when_added_by_builder_then_it_is_refused() {
    // Given
    let evidence = refs(MAX_EVIDENCE_REFS + 1);

    // When
    let error = annotation().with_evidence(evidence).unwrap_err();

    // Then
    assert_eq!(error.code(), "outcome_annotation_too_many_evidence");
}

#[test]
fn given_unsafe_evidence_when_constructed_or_deserialized_then_each_is_rejected() {
    // Given
    let invalid = [
        "", "/tmp/x", "../x", "a/../x", ".", "./x", "a/./x", "a//x", "a/", "a\\x", "a/\0x",
    ];

    // When / Then
    for value in invalid {
        assert!(
            EvidenceRef::new(value).is_err(),
            "constructor accepted {value:?}"
        );
        assert!(
            serde_json::from_value::<EvidenceRef>(serde_json::json!(value)).is_err(),
            "deserializer accepted {value:?}"
        );
    }
}

#[test]
fn given_invalid_evidence_inside_annotation_when_deserialized_then_aggregate_is_rejected() {
    // Given
    let mut value = serde_json::to_value(annotation()).unwrap();
    value["evidence"] = serde_json::json!(["../secret"]);

    // When
    let result = serde_json::from_value::<OutcomeAnnotation>(value);

    // Then
    assert!(result.is_err());
}

#[test]
fn given_oversized_evidence_array_when_deserialized_then_aggregate_is_rejected() {
    // Given
    let mut value = serde_json::to_value(annotation()).unwrap();
    value["evidence"] = serde_json::json!(
        (0..=MAX_EVIDENCE_REFS)
            .map(|index| format!("evidence/{index}.json"))
            .collect::<Vec<_>>()
    );

    // When
    let result = serde_json::from_value::<OutcomeAnnotation>(value);

    // Then
    assert!(result.is_err());
}
