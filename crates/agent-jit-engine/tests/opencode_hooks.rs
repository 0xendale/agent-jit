#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
//! The `OpenCode` plugin bridge is the only place the product knows what the materialized plugin
//! forwards. These tests pin that contract: documented plugin-hook facts only, additive fields
//! bounded, secrets gone before an event exists.

use agent_jit_domain::redaction::{Redactor, SecretClass};
use agent_jit_engine::adapters::claude_hooks::HookEventKind;
use agent_jit_engine::adapters::opencode_hooks::{self, ADAPTER_SCHEMA, NormalizedHook, normalize};

fn fixture(name: &str) -> String {
    let path = format!(
        "{}/tests/fixtures/opencode-hooks/{name}",
        env!("CARGO_MANIFEST_DIR")
    );
    std::fs::read_to_string(&path).unwrap_or_else(|error| panic!("{path}: {error}"))
}

fn normalize_fixture(kind: HookEventKind, name: &str) -> NormalizedHook {
    normalize(kind, &fixture(name), &Redactor::new(), Some("1.18.29")).unwrap()
}

#[test]
fn opencode_hook_events_are_stamped_with_the_opencode_adapter_schema() {
    let hook = normalize_fixture(HookEventKind::SessionStart, "session-start.json");
    assert_eq!(hook.adapter_schema, ADAPTER_SCHEMA);
    assert_ne!(hook.adapter_schema, "agent_jit.claude_hooks.v1");
}

#[test]
fn opencode_hook_session_start_is_normalized() {
    let hook = normalize_fixture(HookEventKind::SessionStart, "session-start.json");

    assert_eq!(hook.kind, HookEventKind::SessionStart);
    assert_eq!(hook.session_key, "b2c3d4e5-1111-2222-3333-444455556666");
    assert_eq!(hook.cwd, "/Users/someone/code/pipeline-viz");
    assert_eq!(hook.claude_version.as_deref(), Some("1.18.29"));
    assert!(
        matches!(hook.payload, opencode_hooks::HookPayload::SessionStart { ref source } if source == "created")
    );
    // The `OpenCode` bridge carries no transcript path: there is nothing to open.
    assert_eq!(hook.transcript_path, None);
}

#[test]
fn opencode_hook_user_prompt_carries_the_intent() {
    let hook = normalize_fixture(HookEventKind::UserPromptSubmit, "user-prompt-submit.json");
    match &hook.payload {
        opencode_hooks::HookPayload::UserPrompt { prompt } => {
            assert!(prompt.value().contains("validate this change"));
        }
        other => panic!("unexpected payload: {other:?}"),
    }
}

#[test]
fn opencode_hook_pre_tool_use_keeps_the_tool_and_its_input() {
    let hook = normalize_fixture(HookEventKind::PreToolUse, "pre-tool-use.json");
    match &hook.payload {
        opencode_hooks::HookPayload::PreToolUse {
            tool_name,
            tool_input,
        } => {
            assert_eq!(tool_name, "bash");
            assert!(tool_input.value().contains("cargo test --workspace"));
        }
        other => panic!("unexpected payload: {other:?}"),
    }
}

#[test]
fn opencode_hook_post_tool_use_keeps_the_response() {
    let hook = normalize_fixture(HookEventKind::PostToolUse, "post-tool-use.json");
    match &hook.payload {
        opencode_hooks::HookPayload::PostToolUse(result) => {
            assert_eq!(result.tool_name, "bash");
            assert!(result.tool_response.value().contains("42 passed"));
            assert!(result.error.is_none());
        }
        other => panic!("unexpected payload: {other:?}"),
    }
}

#[test]
fn opencode_hook_post_tool_use_failure_records_the_error() {
    let hook = normalize_fixture(
        HookEventKind::PostToolUseFailure,
        "post-tool-use-failure.json",
    );
    match &hook.payload {
        opencode_hooks::HookPayload::PostToolUse(result) => {
            let error = result.error.as_ref().expect("a failure carries an error");
            assert!(error.value().contains("exited with code 101"));
        }
        other => panic!("unexpected payload: {other:?}"),
    }
}

#[test]
fn opencode_hook_stop_and_session_end_are_normalized() {
    let stop = normalize_fixture(HookEventKind::Stop, "stop.json");
    assert!(matches!(
        stop.payload,
        opencode_hooks::HookPayload::Stop {
            stop_hook_active: false
        }
    ));

    let end = normalize_fixture(HookEventKind::SessionEnd, "session-end.json");
    assert!(matches!(hook_end(&end), Some("process_exit")));
}

/// Extracts the session-end reason without re-matching in two tests.
fn hook_end(hook: &NormalizedHook) -> Option<&str> {
    match &hook.payload {
        opencode_hooks::HookPayload::SessionEnd { reason } => Some(reason.as_str()),
        _ => None,
    }
}

#[test]
fn opencode_hook_field_order_and_additive_fields_do_not_change_the_canonical_form() {
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
fn opencode_hook_additive_fields_are_kept_as_bounded_extension_metadata() {
    let hook = normalize_fixture(HookEventKind::UserPromptSubmit, "additive-fields.json");

    assert!(hook.extensions.contains_key("some_future_field"));
    assert!(hook.extensions.contains_key("another_future_field"));
    for value in hook.extensions.values() {
        assert!(value.len() <= 1024, "extension metadata must be bounded");
    }
    for known in ["session_id", "directory", "event", "prompt"] {
        assert!(
            !hook.extensions.contains_key(known),
            "{known} is a known field"
        );
    }
}

#[test]
fn opencode_hook_secrets_are_redacted_before_the_event_exists() {
    let hook = normalize(
        HookEventKind::PostToolUse,
        &fixture("with-secrets.json"),
        &Redactor::new().with_canaries(vec![]),
        None,
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
fn opencode_hook_a_missing_required_field_produces_no_event() {
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
            opencode_hooks::HookError::MissingField {
                field: "session_id"
            }
        ),
        "{error:?}"
    );
}

#[test]
fn opencode_hook_a_wrongly_typed_field_produces_no_event() {
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
fn opencode_hook_malformed_json_produces_no_event() {
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
fn opencode_hook_an_event_name_mismatch_is_refused() {
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
fn opencode_hook_normalization_is_deterministic() {
    let first = normalize_fixture(HookEventKind::PreToolUse, "pre-tool-use.json");
    let second = normalize_fixture(HookEventKind::PreToolUse, "pre-tool-use.json");
    assert_eq!(first, second);
    assert_eq!(
        first.canonical_digest().unwrap(),
        second.canonical_digest().unwrap()
    );
}

#[test]
fn opencode_hook_canary_class_is_reported_when_present() {
    let redactor = Redactor::new().with_canaries(vec!["CANARY-header-9c1f".to_owned()]);
    let hook = normalize(
        HookEventKind::PostToolUse,
        &fixture("with-secrets.json"),
        &redactor,
        None,
    )
    .unwrap();
    assert!(matches!(
        hook.payload,
        opencode_hooks::HookPayload::PostToolUse(_)
    ));
    assert!(
        serde_json::to_string(&hook)
            .unwrap()
            .contains(&SecretClass::Canary.marker())
    );
}
