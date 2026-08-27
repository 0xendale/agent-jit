#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
//! Metric values carry how they were obtained. A number whose provenance is unknown is not a
//! number Phase 0 may divide by.

use agent_jit_domain::metrics::{Measured, Unknown, estimate_tokens_from_bytes};

#[test]
fn an_exact_value_names_the_versioned_source_that_supplied_it() {
    let measured = Measured::exact(1234_u64, "claude_code.usage.v1");
    assert_eq!(measured.value(), Some(1234));
    assert!(measured.is_exact());
    assert!(!measured.is_estimated());
    assert_eq!(measured.source(), "exact:claude_code.usage.v1");
}

#[test]
fn an_estimate_names_its_method_and_never_claims_to_be_exact() {
    let measured = Measured::estimated(1234_u64, "utf8_bytes_div_4");
    assert_eq!(measured.value(), Some(1234));
    assert!(!measured.is_exact());
    assert!(measured.is_estimated());
    assert_eq!(measured.source(), "estimated:utf8_bytes_div_4");
}

#[test]
fn a_missing_value_is_unknown_rather_than_zero() {
    let measured: Measured<u64> = Measured::unknown(Unknown::NoStopObserved);
    assert_eq!(measured.value(), None, "unknown must not read as a number");
    assert!(!measured.is_exact());
    assert!(!measured.is_estimated());
    assert_eq!(measured.source(), "unknown:no_stop_observed");
    // The distinction that matters: a real zero is a measurement, absence is not.
    assert_ne!(measured, Measured::exact(0_u64, "whatever"));
}

#[test]
fn every_unknown_reason_renders_a_stable_code() {
    for (reason, code) in [
        (Unknown::NoStopObserved, "no_stop_observed"),
        (Unknown::BackwardsTimestamp, "backwards_timestamp"),
        (Unknown::Overflow, "overflow"),
        (Unknown::NoVersionedSource, "no_versioned_source"),
        (Unknown::NoEventsObserved, "no_events_observed"),
    ] {
        assert_eq!(reason.as_str(), code);
    }
}

#[test]
fn exact_and_estimated_values_are_never_merged_into_one_field() {
    // There is no API that adds an exact value to an estimated one: the only combinator refuses.
    let exact = Measured::exact(10_u64, "source.v1");
    let estimated = Measured::estimated(5_u64, "method");

    assert_eq!(
        exact.checked_add(&estimated),
        Measured::unknown(Unknown::MixedMeasurementSources)
    );
    assert_eq!(
        exact.checked_add(&Measured::exact(5_u64, "source.v1")),
        Measured::exact(15_u64, "source.v1")
    );
    assert_eq!(
        estimated.checked_add(&Measured::estimated(5_u64, "method")),
        Measured::estimated(10_u64, "method")
    );
}

#[test]
fn adding_two_exact_values_from_different_sources_is_refused() {
    let left = Measured::exact(10_u64, "source.v1");
    let right = Measured::exact(5_u64, "other.v2");
    assert_eq!(
        left.checked_add(&right),
        Measured::unknown(Unknown::MixedMeasurementSources)
    );
}

#[test]
fn arithmetic_that_overflows_yields_unknown_rather_than_wrapping() {
    let huge = Measured::exact(u64::MAX, "source.v1");
    let one = Measured::exact(1_u64, "source.v1");
    assert_eq!(huge.checked_add(&one), Measured::unknown(Unknown::Overflow));
}

#[test]
fn adding_anything_to_an_unknown_stays_unknown() {
    let unknown: Measured<u64> = Measured::unknown(Unknown::NoStopObserved);
    let known = Measured::exact(5_u64, "source.v1");
    assert!(unknown.checked_add(&known).value().is_none());
    assert!(known.checked_add(&unknown).value().is_none());
}

#[test]
fn token_estimation_is_deterministic_and_rounds_up() {
    // ceil(bytes / 4), as specified: a partial token still costs a token.
    assert_eq!(estimate_tokens_from_bytes(0), 0);
    assert_eq!(estimate_tokens_from_bytes(1), 1);
    assert_eq!(estimate_tokens_from_bytes(4), 1);
    assert_eq!(estimate_tokens_from_bytes(5), 2);
    assert_eq!(estimate_tokens_from_bytes(8), 2);
    assert_eq!(estimate_tokens_from_bytes(4_000), 1_000);
}

#[test]
fn token_estimation_does_not_overflow_at_the_ceiling() {
    assert_eq!(estimate_tokens_from_bytes(u64::MAX), (u64::MAX / 4) + 1);
}

#[test]
fn measured_values_round_trip_through_json_with_their_provenance() {
    for measured in [
        Measured::exact(7_u64, "claude_code.usage.v1"),
        Measured::estimated(7_u64, "utf8_bytes_div_4"),
        Measured::unknown(Unknown::Overflow),
    ] {
        let json = serde_json::to_string(&measured).unwrap();
        let parsed: Measured<u64> = serde_json::from_str(&json).unwrap();
        assert_eq!(parsed, measured);
        // The rendered form always states the source, so an exported metric stays auditable.
        assert!(json.contains("measurement_source"), "{json}");
    }
}
