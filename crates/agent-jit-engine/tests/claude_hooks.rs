#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
//! The Claude Code adapter reads documented hook payloads and nothing else.
//!
//! Every test here is named `claude_hook_*` so the acceptance command
//! `cargo test -p agent-jit-engine claude_hook_` selects exactly this suite.

use agent_jit_domain::redaction::Redactor;
use agent_jit_engine::adapters::claude_hooks::{
    ADAPTER_SCHEMA, HookError, HookEventKind, HookPayload, normalize,
};

fn fixture(name: &str) -> String {
    let path = format!(
        "{}/tests/fixtures/claude-hooks/{name}",
        env!("CARGO_MANIFEST_DIR")
    );
    std::fs::read_to_string(&path).unwrap_or_else(|error| panic!("{path}: {error}"))
}

fn normalize_fixture(
    kind: HookEventKind,
    name: &str,
) -> agent_jit_engine::adapters::claude_hooks::NormalizedHook {
    normalize(kind, &fixture(name), &Redactor::new(), Some("2.0.0")).unwrap()
}

#[test]
fn claude_hook_session_start_is_normalized() {
    let hook = normalize_fixture(HookEventKind::SessionStart, "session-start.json");

    assert_eq!(hook.adapter_schema, ADAPTER_SCHEMA);
    assert_eq!(hook.kind, HookEventKind::SessionStart);
    assert_eq!(hook.session_key, "b2c3d4e5-1111-2222-3333-444455556666");
    assert_eq!(hook.cwd, "/Users/someone/code/pipeline-viz");
    assert_eq!(hook.claude_version.as_deref(), Some("2.0.0"));
    assert!(
        matches!(hook.payload, HookPayload::SessionStart { ref source } if source == "startup")
    );
}

#[test]
fn claude_hook_user_prompt_carries_the_intent() {
    let hook = normalize_fixture(HookEventKind::UserPromptSubmit, "user-prompt-submit.json");
    match &hook.payload {
        HookPayload::UserPrompt { prompt } => {
            assert!(prompt.value().contains("validate this change"));
        }
        other => panic!("unexpected payload: {other:?}"),
    }
}

#[test]
fn claude_hook_pre_tool_use_keeps_the_tool_and_its_input() {
    let hook = normalize_fixture(HookEventKind::PreToolUse, "pre-tool-use.json");
    match &hook.payload {
        HookPayload::PreToolUse {
            tool_name,
            tool_input,
        } => {
            assert_eq!(tool_name, "Bash");
            assert!(tool_input.value().contains("cargo test --workspace"));
        }
        other => panic!("unexpected payload: {other:?}"),
    }
}

#[test]
fn claude_hook_post_tool_use_keeps_the_response() {
    let hook = normalize_fixture(HookEventKind::PostToolUse, "post-tool-use.json");
    match &hook.payload {
        HookPayload::PostToolUse(result) => {
            assert_eq!(result.tool_name, "Bash");
            assert!(result.tool_response.value().contains("42 passed"));
            assert!(result.error.is_none());
        }
        other => panic!("unexpected payload: {other:?}"),
    }
}

#[test]
fn claude_hook_post_tool_use_failure_records_the_error() {
    let hook = normalize_fixture(
        HookEventKind::PostToolUseFailure,
        "post-tool-use-failure.json",
    );
    match &hook.payload {
        HookPayload::PostToolUse(result) => {
            let error = result.error.as_ref().expect("a failure carries an error");
            assert!(error.value().contains("exited with code 101"));
        }
        other => panic!("unexpected payload: {other:?}"),
    }
}

#[test]
fn claude_hook_stop_and_session_end_are_normalized() {
    let stop = normalize_fixture(HookEventKind::Stop, "stop.json");
    assert!(matches!(
        stop.payload,
        HookPayload::Stop {
            stop_hook_active: false
        }
    ));

    let end = normalize_fixture(HookEventKind::SessionEnd, "session-end.json");
    assert!(matches!(end.payload, HookPayload::SessionEnd { ref reason } if reason == "exit"));
}

#[test]
fn claude_hook_field_order_does_not_change_the_canonical_form() {
    let ordered = normalize_fixture(HookEventKind::UserPromptSubmit, "user-prompt-submit.json");
    let shuffled_with_extras =
        normalize_fixture(HookEventKind::UserPromptSubmit, "additive-fields.json");

    assert_eq!(
        ordered.canonical_digest().unwrap(),
        shuffled_with_extras.canonical_digest().unwrap(),
        "reordering fields, or adding new ones, must not change the normalized event"
    );
}

#[test]
fn claude_hook_additive_fields_are_kept_as_bounded_extension_metadata() {
    let hook = normalize_fixture(HookEventKind::UserPromptSubmit, "additive-fields.json");

    assert!(hook.extensions.contains_key("some_future_field"));
    assert!(hook.extensions.contains_key("another_future_field"));
    for value in hook.extensions.values() {
        assert!(value.len() <= 1024, "extension metadata must be bounded");
    }
    // Known fields never leak into the extension bag.
    for known in [
        "session_id",
        "cwd",
        "prompt",
        "hook_event_name",
        "transcript_path",
    ] {
        assert!(
            !hook.extensions.contains_key(known),
            "{known} is a known field"
        );
    }
}

#[test]
fn claude_hook_secrets_are_redacted_before_the_event_exists() {
    let hook = normalize(
        HookEventKind::PostToolUse,
        &fixture("with-secrets.json"),
        &Redactor::new().with_canaries(vec![]),
        Some("2.0.0"),
    )
    .unwrap();

    let rendered = serde_json::to_string(&hook).unwrap();
    for canary in [
        "CANARY-header-9c1f",
        "CANARY-url-9c1f",
        "CANARY-query-9c1f",
        "CANARY-env-9c1f",
        "CANARY-pem-9c1f",
    ] {
        assert!(!rendered.contains(canary), "{canary} survived: {rendered}");
    }
}

#[test]
fn claude_hook_a_missing_required_field_produces_no_event() {
    let error = normalize(
        HookEventKind::UserPromptSubmit,
        &fixture("missing-session-id.json"),
        &Redactor::new(),
        None,
    )
    .unwrap_err();

    assert_eq!(error.code(), "hook_missing_field");
    assert!(
        matches!(
            error,
            HookError::MissingField {
                field: "session_id"
            }
        ),
        "{error:?}"
    );
}

#[test]
fn claude_hook_a_wrongly_typed_field_produces_no_event() {
    let error = normalize(
        HookEventKind::UserPromptSubmit,
        &fixture("wrong-types.json"),
        &Redactor::new(),
        None,
    )
    .unwrap_err();
    assert_eq!(error.code(), "hook_field_type");
}

#[test]
fn claude_hook_malformed_json_produces_no_event() {
    let error = normalize(
        HookEventKind::UserPromptSubmit,
        "{not json at all",
        &Redactor::new(),
        None,
    )
    .unwrap_err();
    assert_eq!(error.code(), "hook_malformed_json");
}

#[test]
fn claude_hook_an_event_name_mismatch_is_refused() {
    let error = normalize(
        HookEventKind::PreToolUse,
        &fixture("user-prompt-submit.json"),
        &Redactor::new(),
        None,
    )
    .unwrap_err();
    assert_eq!(error.code(), "hook_event_mismatch");
}

#[test]
fn claude_hook_transcript_path_is_provenance_and_is_never_opened() {
    // The path is deliberately unreadable: normalization must still succeed, because the adapter
    // records the path and never reads the file. Claude's transcript JSONL is not a live contract.
    let payload = serde_json::json!({
        "session_id": "b2c3d4e5-1111-2222-3333-444455556666",
        "cwd": "/Users/someone/code/pipeline-viz",
        "hook_event_name": "Stop",
        "stop_hook_active": false,
        "transcript_path": "/proc/nonexistent/definitely-not-here.jsonl"
    })
    .to_string();

    let hook = normalize(HookEventKind::Stop, &payload, &Redactor::new(), None).unwrap();
    assert_eq!(
        hook.transcript_path.as_deref(),
        Some("/proc/nonexistent/definitely-not-here.jsonl")
    );
}

#[test]
fn claude_hook_a_directory_as_transcript_path_still_normalizes() {
    let directory = tempfile::tempdir().unwrap();
    let payload = serde_json::json!({
        "session_id": "b2c3d4e5-1111-2222-3333-444455556666",
        "cwd": "/Users/someone/code/pipeline-viz",
        "hook_event_name": "Stop",
        "stop_hook_active": false,
        "transcript_path": directory.path().to_string_lossy()
    })
    .to_string();

    // Opening a directory as a file fails; succeeding proves nothing was opened.
    assert!(normalize(HookEventKind::Stop, &payload, &Redactor::new(), None).is_ok());
}

#[test]
fn claude_hook_normalization_is_deterministic() {
    let first = normalize_fixture(HookEventKind::PostToolUse, "post-tool-use.json");
    let second = normalize_fixture(HookEventKind::PostToolUse, "post-tool-use.json");
    assert_eq!(
        first.canonical_digest().unwrap(),
        second.canonical_digest().unwrap()
    );
}

#[test]
fn claude_hook_event_kinds_round_trip_through_their_names() {
    for kind in HookEventKind::ALL {
        assert_eq!(kind.as_str().parse::<HookEventKind>().unwrap(), kind);
    }
    assert!("NotAHook".parse::<HookEventKind>().is_err());
}
