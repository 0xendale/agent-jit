#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
//! Identifiers are branded and envelopes are exact-version: neither may be stringly typed.

use agent_jit_domain::envelope::{Envelope, Provenance, Record, SchemaName, SchemaVersion};
use agent_jit_domain::ids::{Id, IdError, RepositoryId, SessionId, TrajectoryId};
use agent_jit_domain::trace::Trajectory;

const VALID: &str = "01J0000000000000000000000A";

#[test]
fn an_id_round_trips_through_its_rendered_form() {
    let rendered = format!("trj_{VALID}");
    let id: TrajectoryId = rendered.parse().unwrap();
    assert_eq!(id.to_string(), rendered);
}

#[test]
fn an_id_of_the_wrong_kind_is_rejected_even_though_the_body_is_valid() {
    let error = format!("ses_{VALID}").parse::<TrajectoryId>().unwrap_err();
    assert_eq!(error.code(), "id_prefix_mismatch");
    assert!(format!("ses_{VALID}").parse::<SessionId>().is_ok());
}

#[test]
fn crockford_ambiguous_characters_are_rejected() {
    for body in ["01J000000000000000000000IA", "01J000000000000000000000la"] {
        let error = format!("trj_{body}").parse::<TrajectoryId>().unwrap_err();
        assert_eq!(error.code(), "id_malformed", "{body}");
    }
}

#[test]
fn an_id_of_the_wrong_length_is_rejected() {
    let error = "trj_01J".parse::<TrajectoryId>().unwrap_err();
    assert!(matches!(error, IdError::Malformed { .. }), "{error:?}");
}

#[test]
fn ids_serialize_as_plain_strings() {
    let id: RepositoryId = format!("rep_{VALID}").parse().unwrap();
    assert_eq!(
        serde_json::to_string(&id).unwrap(),
        format!("\"rep_{VALID}\"")
    );
    assert_eq!(
        serde_json::from_str::<RepositoryId>(&format!("\"rep_{VALID}\"")).unwrap(),
        id
    );
}

fn sample_envelope() -> Envelope<Trajectory> {
    Envelope::new(
        format!("trj_{VALID}").parse().unwrap(),
        Provenance::recorded_by("agent-jit/0.1.0"),
        Trajectory::sample(),
    )
}

#[test]
fn an_envelope_carries_its_schema_name_and_exact_version() {
    let envelope = sample_envelope();
    assert_eq!(envelope.schema(), SchemaName::new("agent_jit.trajectory"));
    assert_eq!(envelope.version(), SchemaVersion::new(1));
    assert_eq!(Trajectory::SCHEMA, "agent_jit.trajectory");
}

#[test]
fn an_envelope_round_trips_through_json() {
    let envelope = sample_envelope();
    let json = serde_json::to_string(&envelope).unwrap();
    let parsed: Envelope<Trajectory> = serde_json::from_str(&json).unwrap();
    assert_eq!(parsed, envelope);
}

#[test]
fn an_unknown_schema_version_is_rejected() {
    let mut value = serde_json::to_value(sample_envelope()).unwrap();
    value["version"] = serde_json::json!(2);
    let error = serde_json::from_value::<Envelope<Trajectory>>(value).unwrap_err();
    assert!(error.to_string().contains("schema_unsupported"), "{error}");
}

#[test]
fn an_unknown_schema_name_is_rejected() {
    let mut value = serde_json::to_value(sample_envelope()).unwrap();
    value["schema"] = serde_json::json!("agent_jit.not_a_thing");
    let error = serde_json::from_value::<Envelope<Trajectory>>(value).unwrap_err();
    assert!(error.to_string().contains("schema_unsupported"), "{error}");
}

#[test]
fn an_id_whose_prefix_does_not_match_the_schema_is_rejected() {
    let mut value = serde_json::to_value(sample_envelope()).unwrap();
    value["id"] = serde_json::json!(format!("ses_{VALID}"));
    let error = serde_json::from_value::<Envelope<Trajectory>>(value).unwrap_err();
    assert!(error.to_string().contains("id_prefix_mismatch"), "{error}");
}

#[test]
fn the_envelope_digest_ignores_declared_volatile_fields() {
    let mut envelope = sample_envelope();
    let before = envelope.digest().unwrap();
    envelope.provenance_mut().recorded_at_unix_ms = 1_800_000_000_000;
    assert_eq!(envelope.digest().unwrap(), before);

    envelope.body_mut().intent = "a different intent".to_owned();
    assert_ne!(envelope.digest().unwrap(), before);
}

#[test]
fn every_declared_volatile_pointer_exists_in_the_serialized_envelope() {
    // A stale volatile pointer would silently start digesting a wall-clock field.
    let envelope = sample_envelope();
    let value = serde_json::to_value(&envelope).unwrap();
    for pointer in Envelope::<Trajectory>::volatile_pointers() {
        assert!(
            value.pointer(&pointer).is_some(),
            "volatile pointer {pointer} is not present in the serialized envelope"
        );
    }
}

#[test]
fn ids_are_ordered_by_their_rendered_form() {
    let first: Id<agent_jit_domain::ids::kind::Trajectory> =
        format!("trj_{VALID}").parse().unwrap();
    let second: TrajectoryId = "trj_01J0000000000000000000000B".parse().unwrap();
    assert!(first < second);
}
