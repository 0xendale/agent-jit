#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
//! Actor and rationale bounds for manual annotations.

use agent_jit_domain::ids::TrajectoryId;
use agent_jit_domain::outcome::{
    AnnotationRevision, AnnotationStatus, MAX_ANNOTATION_ACTOR_BYTES,
    MAX_ANNOTATION_RATIONALE_BYTES, OutcomeAnnotation, OutcomeAnnotationError,
};

fn build(actor: &str, rationale: &str) -> Result<OutcomeAnnotation, OutcomeAnnotationError> {
    let trajectory: TrajectoryId = "trj_01J0000000000000000000000A".parse().unwrap();
    OutcomeAnnotation::new(
        trajectory,
        AnnotationRevision::new(1).unwrap(),
        AnnotationStatus::Unknown,
        actor,
        1,
        rationale,
        Vec::new(),
    )
}

fn value() -> serde_json::Value {
    serde_json::to_value(build("actor", "rationale").unwrap()).unwrap()
}

#[test]
fn given_actor_at_limit_when_constructed_then_it_succeeds() {
    // Given
    let actor = "a".repeat(MAX_ANNOTATION_ACTOR_BYTES);

    // When
    let result = build(&actor, "r");

    // Then
    assert!(result.is_ok());
}

#[test]
fn given_actor_over_limit_when_constructed_then_it_is_refused() {
    // Given
    let actor = "a".repeat(MAX_ANNOTATION_ACTOR_BYTES + 1);

    // When
    let error = build(&actor, "r").unwrap_err();

    // Then
    assert_eq!(error.code(), "outcome_annotation_actor_too_long");
}

#[test]
fn given_empty_actor_when_constructed_then_it_is_refused() {
    // Given / When
    let error = build("", "r").unwrap_err();

    // Then
    assert_eq!(error.code(), "outcome_annotation_actor_empty");
}

#[test]
fn given_rationale_at_limit_when_constructed_then_it_succeeds() {
    // Given
    let rationale = "r".repeat(MAX_ANNOTATION_RATIONALE_BYTES);

    // When
    let result = build("a", &rationale);

    // Then
    assert!(result.is_ok());
}

#[test]
fn given_rationale_over_limit_when_constructed_then_it_is_refused() {
    // Given
    let rationale = "r".repeat(MAX_ANNOTATION_RATIONALE_BYTES + 1);

    // When
    let error = build("a", &rationale).unwrap_err();

    // Then
    assert_eq!(error.code(), "outcome_annotation_rationale_too_long");
}

#[test]
fn given_empty_rationale_when_constructed_then_it_is_refused() {
    // Given / When
    let error = build("a", "").unwrap_err();

    // Then
    assert_eq!(error.code(), "outcome_annotation_rationale_empty");
}

#[test]
fn given_empty_actor_inside_annotation_when_deserialized_then_aggregate_is_rejected() {
    // Given
    let mut annotation = value();
    annotation["actor"] = serde_json::json!("");

    // When
    let result = serde_json::from_value::<OutcomeAnnotation>(annotation);

    // Then
    assert!(result.is_err());
}

#[test]
fn given_oversized_actor_inside_annotation_when_deserialized_then_aggregate_is_rejected() {
    // Given
    let mut annotation = value();
    annotation["actor"] = serde_json::json!("a".repeat(MAX_ANNOTATION_ACTOR_BYTES + 1));

    // When
    let result = serde_json::from_value::<OutcomeAnnotation>(annotation);

    // Then
    assert!(result.is_err());
}

#[test]
fn given_empty_rationale_inside_annotation_when_deserialized_then_aggregate_is_rejected() {
    // Given
    let mut annotation = value();
    annotation["rationale"] = serde_json::json!("");

    // When
    let result = serde_json::from_value::<OutcomeAnnotation>(annotation);

    // Then
    assert!(result.is_err());
}

#[test]
fn given_oversized_rationale_inside_annotation_when_deserialized_then_aggregate_is_rejected() {
    // Given
    let mut annotation = value();
    annotation["rationale"] = serde_json::json!("r".repeat(MAX_ANNOTATION_RATIONALE_BYTES + 1));

    // When
    let result = serde_json::from_value::<OutcomeAnnotation>(annotation);

    // Then
    assert!(result.is_err());
}

#[test]
fn given_multibyte_actor_at_and_over_byte_limit_then_only_at_limit_succeeds() {
    let at_limit = "é".repeat(MAX_ANNOTATION_ACTOR_BYTES / 2);
    let over_limit = "é".repeat(MAX_ANNOTATION_ACTOR_BYTES / 2 + 1);

    assert!(build(&at_limit, "r").is_ok());
    assert_eq!(
        build(&over_limit, "r").unwrap_err(),
        OutcomeAnnotationError::ActorTooLong
    );

    let mut annotation = value();
    annotation["actor"] = serde_json::json!(over_limit);
    assert!(serde_json::from_value::<OutcomeAnnotation>(annotation).is_err());
}

#[test]
fn given_multibyte_rationale_at_and_over_byte_limit_then_only_at_limit_succeeds() {
    let at_limit = "é".repeat(MAX_ANNOTATION_RATIONALE_BYTES / 2);
    let over_limit = "é".repeat(MAX_ANNOTATION_RATIONALE_BYTES / 2 + 1);

    assert!(build("a", &at_limit).is_ok());
    assert_eq!(
        build("a", &over_limit).unwrap_err(),
        OutcomeAnnotationError::RationaleTooLong
    );

    let mut annotation = value();
    annotation["rationale"] = serde_json::json!(over_limit);
    assert!(serde_json::from_value::<OutcomeAnnotation>(annotation).is_err());
}
