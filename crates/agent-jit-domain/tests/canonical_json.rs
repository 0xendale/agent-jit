#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
//! Canonical JSON is the substrate for every reproducible digest in the product.

use agent_jit_domain::canonical::{
    CanonicalError, Digest, canonical_string, digest_of, digest_projection,
};
use serde_json::json;

#[test]
fn object_keys_are_sorted_and_whitespace_is_removed() {
    let value = json!({"b": 1, "a": {"d": 2, "c": [3, 1, 2]}});
    assert_eq!(
        canonical_string(&value).unwrap(),
        r#"{"a":{"c":[3,1,2],"d":2},"b":1}"#
    );
}

#[test]
fn key_order_does_not_change_the_digest() {
    let left = json!({"alpha": 1, "beta": {"x": true, "y": null}});
    let right = json!({"beta": {"y": null, "x": true}, "alpha": 1});
    assert_eq!(digest_of(&left).unwrap(), digest_of(&right).unwrap());
}

#[test]
fn array_order_does_change_the_digest() {
    let left = json!({"items": [1, 2]});
    let right = json!({"items": [2, 1]});
    assert_ne!(digest_of(&left).unwrap(), digest_of(&right).unwrap());
}

#[test]
fn floats_are_rejected_with_their_pointer() {
    let error = canonical_string(&json!({"metrics": {"latency_ms": 12.5}})).unwrap_err();
    assert!(
        matches!(&error, CanonicalError::NonIntegerNumber { pointer } if pointer == "/metrics/latency_ms"),
        "{error:?}"
    );
    assert_eq!(error.code(), "canonical_non_integer_number");
}

#[test]
fn strings_and_keys_are_normalized_to_nfc() {
    // "é" as U+0065 U+0301 (decomposed) must canonicalize like U+00E9 (composed).
    let decomposed = json!({"cafe\u{301}": "cafe\u{301}"});
    let composed = json!({"caf\u{e9}": "caf\u{e9}"});
    assert_eq!(
        canonical_string(&decomposed).unwrap(),
        canonical_string(&composed).unwrap()
    );
}

#[test]
fn keys_colliding_after_normalization_are_rejected() {
    let value = json!({"cafe\u{301}": 1, "caf\u{e9}": 2});
    let error = canonical_string(&value).unwrap_err();
    assert_eq!(error.code(), "canonical_duplicate_key");
}

#[test]
fn canonicalization_is_idempotent() {
    let value = json!({"z": [{"b": 1, "a": 2}], "a": "x"});
    let once = canonical_string(&value).unwrap();
    let reparsed: serde_json::Value = serde_json::from_str(&once).unwrap();
    assert_eq!(canonical_string(&reparsed).unwrap(), once);
}

#[test]
fn volatile_pointers_are_excluded_from_the_digest_projection() {
    let noon = json!({
        "id": "trj_01J0000000000000000000000A",
        "recorded_at": "2026-08-26T12:00:00Z",
        "duration_ms": 1200,
        "body": {"intent": "validate change"}
    });
    let night = json!({
        "id": "trj_01J0000000000000000000000A",
        "recorded_at": "2026-08-26T23:59:59Z",
        "duration_ms": 999,
        "body": {"intent": "validate change"}
    });
    let volatile = ["/recorded_at", "/duration_ms"];

    assert_eq!(
        digest_projection(&noon, &volatile).unwrap(),
        digest_projection(&night, &volatile).unwrap()
    );
    // Volatile fields are excluded from the projection only, never from the record itself.
    assert_ne!(digest_of(&noon).unwrap(), digest_of(&night).unwrap());
}

#[test]
fn a_volatile_pointer_that_matches_nothing_is_an_error() {
    let value = json!({"a": 1});
    let error = digest_projection(&value, &["/missing"]).unwrap_err();
    assert_eq!(error.code(), "canonical_unknown_pointer");
}

#[test]
fn digests_render_as_lowercase_hex_and_round_trip() {
    let digest = digest_of(&json!({"a": 1})).unwrap();
    let rendered = digest.to_string();
    assert_eq!(rendered.len(), 64);
    assert!(
        rendered
            .chars()
            .all(|c| c.is_ascii_hexdigit() && !c.is_ascii_uppercase())
    );
    assert_eq!(rendered.parse::<Digest>().unwrap(), digest);
}

proptest::proptest! {
    /// Shuffling object keys at every depth must never change the digest.
    #[test]
    fn digests_ignore_object_key_order(value in arbitrary_record()) {
        let shuffled = shuffle_keys(&value);
        proptest::prop_assert_eq!(digest_of(&value).unwrap(), digest_of(&shuffled).unwrap());
    }

    /// Canonicalization is a fixed point: canonicalizing twice changes nothing.
    #[test]
    fn canonicalization_is_a_fixed_point(value in arbitrary_record()) {
        let once = canonical_string(&value).unwrap();
        let reparsed: serde_json::Value = serde_json::from_str(&once).unwrap();
        proptest::prop_assert_eq!(canonical_string(&reparsed).unwrap(), once);
    }
}

/// Generates records with integer numbers only, mirroring the product's own contracts.
fn arbitrary_record() -> impl proptest::strategy::Strategy<Value = serde_json::Value> {
    use proptest::prelude::*;

    let leaf = prop_oneof![
        Just(serde_json::Value::Null),
        any::<bool>().prop_map(serde_json::Value::from),
        any::<i64>().prop_map(serde_json::Value::from),
        "[a-z ]{0,12}".prop_map(serde_json::Value::from),
    ];

    leaf.prop_recursive(4, 32, 4, |inner| {
        prop_oneof![
            proptest::collection::vec(inner.clone(), 0..4).prop_map(serde_json::Value::from),
            proptest::collection::hash_map("[a-z]{1,6}", inner, 0..4)
                .prop_map(|map| { serde_json::Value::Object(map.into_iter().collect()) }),
        ]
    })
}

/// Rebuilds `value` with every object's keys in reverse order.
fn shuffle_keys(value: &serde_json::Value) -> serde_json::Value {
    match value {
        serde_json::Value::Object(members) => serde_json::Value::Object(
            members
                .iter()
                .rev()
                .map(|(key, member)| (key.clone(), shuffle_keys(member)))
                .collect(),
        ),
        serde_json::Value::Array(items) => {
            serde_json::Value::Array(items.iter().map(shuffle_keys).collect())
        }
        other => other.clone(),
    }
}
