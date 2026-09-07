#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
//! Trace metrics are computed from STORED events, so a report can be recomputed from the database
//! rather than trusted because the recorder said so.

use agent_jit_domain::envelope::{Envelope, Provenance};
use agent_jit_domain::ids::{EventId, RepositoryId, SessionId};
use agent_jit_domain::metrics::Unknown;
use agent_jit_domain::trace::{Event, EventKind};
use agent_jit_engine::metrics::{MetricInputs, compute_trace_metrics};

fn session_id() -> SessionId {
    SessionId::from_body("01J0000000000000000000000S").unwrap()
}

fn repository_id() -> RepositoryId {
    RepositoryId::from_body("01J0000000000000000000000R").unwrap()
}

/// Builds a stored event at `sequence`, as the recorder would have written it.
fn event(sequence: u32, kind: EventKind, at: i64, bytes: u64) -> Envelope<Event> {
    let body = format!("{sequence:026}");
    Envelope::new(
        EventId::from_body(&body).unwrap(),
        Provenance::recorded_by("agent-jit/0.1.0"),
        Event {
            session_id: session_id(),
            sequence,
            kind,
            tool_name: matches!(kind, EventKind::ToolCall | EventKind::ToolResult)
                .then(|| "Bash".to_owned()),
            argv: Vec::new(),
            exit_code: None,
            payload_digest: None,
            payload_bytes: bytes,
            payload_truncated: false,
            started_at_unix_ms: at,
            duration_ms: 0,
        },
    )
}

fn inputs() -> MetricInputs {
    MetricInputs {
        repository_id: repository_id(),
        session_id: session_id(),
        head_commit: "0".repeat(40),
        adapter_schema: Some("agent_jit.claude_hooks.v1".to_owned()),
        runtime_version: Some("2.1.223".to_owned()),
        model_id: None,
        exact_input_tokens: None,
        quarantined_segments: 0,
        duplicates_collapsed: 0,
    }
}

/// Two turns, one hour of user-idle time between them.
fn two_turn_session() -> Vec<Envelope<Event>> {
    vec![
        event(0, EventKind::SessionStart, 10_000, 100),
        event(1, EventKind::UserPrompt, 10_000, 400),
        event(2, EventKind::ToolCall, 10_500, 200),
        event(3, EventKind::ToolResult, 10_800, 800),
        event(4, EventKind::Stop, 11_000, 100),
        event(5, EventKind::UserPrompt, 3_611_000, 400),
        event(6, EventKind::ToolCall, 3_612_000, 200),
        event(7, EventKind::Stop, 3_613_000, 100),
        event(8, EventKind::SessionEnd, 3_613_100, 100),
    ]
}

#[test]
fn active_duration_sums_turns_and_excludes_user_idle_time() {
    let metrics = compute_trace_metrics(&two_turn_session(), &inputs()).unwrap();

    // Turn one 10_000..11_000 = 1000 ms, turn two 3_611_000..3_613_000 = 2000 ms.
    assert_eq!(metrics.active_duration_ms.value(), Some(3_000));
    assert!(metrics.active_duration_ms.is_exact());
    // The hour the user spent away is in wall duration and nowhere else.
    assert_eq!(metrics.wall_duration_ms.value(), Some(3_603_100));
    assert_eq!(metrics.turns, 2);
}

#[test]
fn only_tool_calls_count_as_agent_visible_calls() {
    let metrics = compute_trace_metrics(&two_turn_session(), &inputs()).unwrap();
    // Two ToolCall events; the ToolResult is the same call seen from the other side.
    assert_eq!(metrics.agent_tool_calls, 2);
}

#[test]
fn a_session_with_no_stop_reports_unknown_duration_rather_than_zero() {
    let events = vec![
        event(0, EventKind::UserPrompt, 1_000, 400),
        event(1, EventKind::ToolCall, 1_500, 200),
    ];
    let metrics = compute_trace_metrics(&events, &inputs()).unwrap();

    assert_eq!(metrics.active_duration_ms.value(), None);
    assert_eq!(
        metrics.active_duration_ms,
        agent_jit_domain::metrics::Measured::unknown(Unknown::NoStopObserved)
    );
    // The work that WAS observed is still reported.
    assert_eq!(metrics.agent_tool_calls, 1);
    assert_eq!(metrics.turns, 1);
}

#[test]
fn a_backwards_timestamp_yields_unknown_rather_than_a_negative_interval() {
    let events = vec![
        event(0, EventKind::UserPrompt, 10_000, 400),
        event(1, EventKind::Stop, 9_000, 100),
        event(2, EventKind::SessionEnd, 10_500, 100),
    ];
    let metrics = compute_trace_metrics(&events, &inputs()).unwrap();
    assert_eq!(
        metrics.active_duration_ms,
        agent_jit_domain::metrics::Measured::unknown(Unknown::BackwardsTimestamp)
    );
}

#[test]
fn overlapping_turns_cannot_double_count() {
    // Two prompts before any stop: the turn opened once and closed once, so the interval is
    // measured from the FIRST prompt to the stop, not once per prompt.
    let events = vec![
        event(0, EventKind::UserPrompt, 1_000, 400),
        event(1, EventKind::UserPrompt, 1_200, 400),
        event(2, EventKind::Stop, 2_000, 100),
        event(3, EventKind::SessionEnd, 2_100, 100),
    ];
    let metrics = compute_trace_metrics(&events, &inputs()).unwrap();
    assert_eq!(metrics.active_duration_ms.value(), Some(1_000));
    assert_eq!(metrics.turns, 2, "both prompts are still counted as turns");
}

#[test]
fn a_repeated_stop_does_not_add_a_second_interval() {
    let events = vec![
        event(0, EventKind::UserPrompt, 1_000, 400),
        event(1, EventKind::Stop, 2_000, 100),
        event(2, EventKind::Stop, 2_500, 100),
        event(3, EventKind::SessionEnd, 2_600, 100),
    ];
    let metrics = compute_trace_metrics(&events, &inputs()).unwrap();
    assert_eq!(
        metrics.active_duration_ms.value(),
        Some(1_000),
        "a stop with no open turn closes nothing"
    );
}

#[test]
fn tokens_are_estimated_from_bytes_when_no_exact_usage_exists() {
    let metrics = compute_trace_metrics(&two_turn_session(), &inputs()).unwrap();

    let total_bytes: u64 = two_turn_session()
        .iter()
        .map(|event| event.body().payload_bytes)
        .sum();
    assert_eq!(metrics.observed_bytes, total_bytes);
    assert!(metrics.estimated_input_tokens.is_estimated());
    assert_eq!(
        metrics.estimated_input_tokens.value(),
        Some(total_bytes.div_ceil(4))
    );
    assert_eq!(
        metrics.estimated_input_tokens.source(),
        "estimated:utf8_bytes_div_4"
    );
}

#[test]
fn an_exact_token_count_from_a_versioned_source_stays_separate_from_the_estimate() {
    let mut with_usage = inputs();
    with_usage.exact_input_tokens = Some((4_321, "claude_code.usage.v1".to_owned()));

    let metrics = compute_trace_metrics(&two_turn_session(), &with_usage).unwrap();

    assert!(metrics.exact_input_tokens.is_exact());
    assert_eq!(metrics.exact_input_tokens.value(), Some(4_321));
    assert_eq!(
        metrics.exact_input_tokens.source(),
        "exact:claude_code.usage.v1"
    );
    // The estimate is still reported, in its own field, unmerged.
    assert!(metrics.estimated_input_tokens.is_estimated());
    assert_ne!(
        metrics.estimated_input_tokens.value(),
        metrics.exact_input_tokens.value()
    );
}

#[test]
fn without_a_versioned_source_the_exact_field_is_unknown_not_the_estimate() {
    let metrics = compute_trace_metrics(&two_turn_session(), &inputs()).unwrap();
    assert_eq!(
        metrics.exact_input_tokens,
        agent_jit_domain::metrics::Measured::unknown(Unknown::NoVersionedSource)
    );
}

#[test]
fn an_empty_event_list_is_unknown_rather_than_a_zero_cost_trajectory() {
    let error = compute_trace_metrics(&[], &inputs()).unwrap_err();
    assert_eq!(error.code(), "metrics_no_events");
}

#[test]
fn provenance_records_what_produced_every_number() {
    let metrics = compute_trace_metrics(&two_turn_session(), &inputs()).unwrap();

    assert_eq!(metrics.provenance.computed_from, "stored_events");
    assert_eq!(metrics.provenance.event_count, 9);
    assert_eq!(
        metrics.provenance.adapter_schema.as_deref(),
        Some("agent_jit.claude_hooks.v1")
    );
    assert_eq!(
        metrics.provenance.runtime_version.as_deref(),
        Some("2.1.223")
    );
    assert_eq!(metrics.provenance.head_commit.len(), 40);
    assert_eq!(metrics.provenance.repository_id, repository_id());
    // The model is not in any hook payload, so it is absent rather than invented.
    assert!(metrics.provenance.model_id.is_none());
}

#[test]
fn recorder_health_travels_with_the_metrics() {
    let mut lossy = inputs();
    lossy.quarantined_segments = 2;
    lossy.duplicates_collapsed = 3;

    let mut events = two_turn_session();
    if let Some(first) = events.first_mut() {
        first.body_mut().payload_truncated = true;
    }

    let metrics = compute_trace_metrics(&events, &lossy).unwrap();
    assert_eq!(metrics.health.quarantined_segments, 2);
    assert_eq!(metrics.health.duplicates_collapsed, 3);
    assert_eq!(metrics.health.truncated_payloads, 1);
    assert!(
        metrics.health.is_lossy(),
        "a trajectory that lost data must say so"
    );
}

#[test]
fn a_clean_session_reports_no_loss() {
    let metrics = compute_trace_metrics(&two_turn_session(), &inputs()).unwrap();
    assert!(!metrics.health.is_lossy());
}

#[test]
fn byte_totals_that_would_overflow_report_unknown_rather_than_wrapping() {
    let events = vec![
        event(0, EventKind::UserPrompt, 1_000, u64::MAX),
        event(1, EventKind::ToolCall, 1_500, 1),
        event(2, EventKind::Stop, 2_000, 1),
        event(3, EventKind::SessionEnd, 2_100, 1),
    ];
    let metrics = compute_trace_metrics(&events, &inputs()).unwrap();
    assert_eq!(
        metrics.estimated_input_tokens,
        agent_jit_domain::metrics::Measured::unknown(Unknown::Overflow)
    );
}

#[test]
fn metrics_are_deterministic_for_the_same_stored_events() {
    let first = compute_trace_metrics(&two_turn_session(), &inputs()).unwrap();
    let second = compute_trace_metrics(&two_turn_session(), &inputs()).unwrap();
    assert_eq!(first, second);
}

#[test]
fn event_order_in_the_slice_does_not_change_the_result() {
    // Metrics sort by stored sequence, so a query that returned rows in another order is harmless.
    let mut shuffled = two_turn_session();
    shuffled.reverse();

    assert_eq!(
        compute_trace_metrics(&shuffled, &inputs()).unwrap(),
        compute_trace_metrics(&two_turn_session(), &inputs()).unwrap()
    );
}
