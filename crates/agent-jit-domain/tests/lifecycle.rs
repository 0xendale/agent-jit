#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
//! Lifecycle transitions are a contract, not a convention: illegal moves are typed errors.

use agent_jit_domain::lifecycle::{LifecycleError, LifecycleState};

#[test]
fn the_happy_path_walks_observed_to_active() {
    let mut state = LifecycleState::Observed;
    for next in [
        LifecycleState::Proposed,
        LifecycleState::Validating,
        LifecycleState::Approved,
        LifecycleState::Active,
    ] {
        state = state.transition_to(next).unwrap();
    }
    assert_eq!(state, LifecycleState::Active);
}

#[test]
fn skipping_validation_is_rejected() {
    let error = LifecycleState::Proposed
        .transition_to(LifecycleState::Active)
        .unwrap_err();
    assert_eq!(error.code(), "lifecycle_invalid_transition");
    assert!(
        matches!(
            error,
            LifecycleError::IllegalTransition {
                from: LifecycleState::Proposed,
                to: LifecycleState::Active
            }
        ),
        "{error:?}"
    );
}

#[test]
fn a_degraded_capability_may_recover_or_retire_but_not_reset() {
    assert!(
        LifecycleState::Degraded
            .transition_to(LifecycleState::Active)
            .is_ok()
    );
    assert!(
        LifecycleState::Degraded
            .transition_to(LifecycleState::Retired)
            .is_ok()
    );
    assert!(
        LifecycleState::Degraded
            .transition_to(LifecycleState::Observed)
            .is_err()
    );
}

#[test]
fn retirement_is_terminal() {
    for next in [
        LifecycleState::Observed,
        LifecycleState::Proposed,
        LifecycleState::Validating,
        LifecycleState::Approved,
        LifecycleState::Active,
        LifecycleState::Degraded,
        LifecycleState::Retired,
    ] {
        assert!(
            LifecycleState::Retired.transition_to(next).is_err(),
            "retired must not move to {next:?}"
        );
    }
}

#[test]
fn a_state_never_transitions_to_itself() {
    for state in LifecycleState::ALL {
        assert!(state.transition_to(state).is_err(), "{state:?}");
    }
}

#[test]
fn states_render_and_parse_in_snake_case() {
    assert_eq!(LifecycleState::Validating.to_string(), "validating");
    assert_eq!(
        "degraded".parse::<LifecycleState>().unwrap(),
        LifecycleState::Degraded
    );
    let error = "enabled".parse::<LifecycleState>().unwrap_err();
    assert_eq!(error.code(), "lifecycle_unknown_state");
}

#[test]
fn every_state_serializes_as_its_rendered_form() {
    for state in LifecycleState::ALL {
        let json = serde_json::to_string(&state).unwrap();
        assert_eq!(json, format!("\"{state}\""));
        assert_eq!(
            serde_json::from_str::<LifecycleState>(&json).unwrap(),
            state
        );
    }
}
