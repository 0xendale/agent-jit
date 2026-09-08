//! The `OpenCode` plugin bridge adapter.
//!
//! This is the only place the product knows what the materialized `OpenCode` plugin forwards. The
//! plugin subscribes to `OpenCode`'s *documented* TypeScript plugin hooks (`tool.execute.before`,
//! `tool.execute.after`) and its event bus (`session.created`, `session.idle`, user messages),
//! then writes a bridge payload to the recorder's stdin (see
//! `docs/adr/0009-opencode-second-runtime.md`). `OpenCode`'s internal state files are never read:
//! they are not a stable contract, exactly like Claude's transcript JSONL.
//!
//! The shared event model lives in [`super::claude_hooks`]; only the payload vocabulary differs.
//! The `claude_version` slot on [`NormalizedHook`] carries the instrumented runtime's version —
//! for events from this adapter, that is the `OpenCode` version.
//!
//! Unknown fields are kept — bounded and redacted — as extension metadata rather than dropped: a
//! later `OpenCode` release adding a field should show up in the evidence, not vanish. They are not
//! part of the canonical form, so their arrival cannot change a digest computed before they
//! existed.

// Re-exported so callers of this adapter never need to know where the shared model lives.
pub use super::claude_hooks::{HookError, HookEventKind, HookPayload, NormalizedHook, ToolResult};

use std::collections::BTreeMap;

use agent_jit_domain::redaction::{Redacted, Redactor};
use serde_json::Value;

/// Identifier of this adapter's normalization contract. Stamped on every event.
pub const ADAPTER_SCHEMA: &str = "agent_jit.opencode_hooks.v1";

/// Maximum bytes kept per extension metadata value.
const MAX_EXTENSION_BYTES: usize = 1024;

/// Maximum number of unknown fields kept per event.
const MAX_EXTENSIONS: usize = 16;

/// The `OpenCode` event name each bridge kind carries.
///
/// Real `OpenCode` names are used where `OpenCode` emits a distinct event. `user.prompt` and
/// `session.ended` are bridge names for facts `OpenCode`'s bus does not name distinctly: the plugin
/// derives the prompt from user messages, and the launcher synthesizes session end on process
/// exit, because a process cannot observe its own termination from inside.
#[must_use]
pub fn opencode_event_name(kind: HookEventKind) -> &'static str {
    match kind {
        HookEventKind::SessionStart => "session.created",
        HookEventKind::UserPromptSubmit => "user.prompt",
        HookEventKind::PreToolUse => "tool.execute.before",
        // A failure arrives as a tool result carrying an error, so both share one name.
        HookEventKind::PostToolUse | HookEventKind::PostToolUseFailure => "tool.execute.after",
        HookEventKind::Stop => "session.idle",
        HookEventKind::SessionEnd => "session.ended",
    }
}

/// Fields the bridge understands. Anything else becomes extension metadata.
const KNOWN_FIELDS: &[&str] = &[
    "session_id",
    "directory",
    "event",
    "source",
    "prompt",
    "tool_name",
    "tool_input",
    "tool_response",
    "error",
    "reason",
];

/// Normalizes one `OpenCode` bridge payload.
///
/// # Errors
///
/// Returns [`HookError`] when the payload is not JSON, is missing a required field, has a field
/// of the wrong type, or describes a different event. No event is produced in any of those cases.
pub fn normalize(
    kind: HookEventKind,
    payload: &str,
    redactor: &Redactor,
    opencode_version: Option<&str>,
) -> Result<NormalizedHook, HookError> {
    let document: Value =
        serde_json::from_str(payload).map_err(|error| HookError::MalformedJson {
            // `serde_json`'s message names the position, not the content.
            reason: format!("line {}, column {}", error.line(), error.column()),
        })?;

    let event_name = string_field(&document, "event")?;
    if event_name != opencode_event_name(kind) {
        return Err(HookError::EventMismatch {
            found: event_name.to_owned(),
            expected: opencode_event_name(kind),
        });
    }

    let session_key = string_field(&document, "session_id")?.to_owned();
    let cwd = string_field(&document, "directory")?.to_owned();

    let payload = build_payload(kind, &document, redactor)?;
    let extensions = extension_metadata(&document, redactor);

    Ok(NormalizedHook {
        adapter_schema: ADAPTER_SCHEMA.to_owned(),
        kind,
        session_key,
        cwd,
        // The slot carries whichever instrumented runtime produced the event.
        claude_version: opencode_version.map(str::to_owned),
        transcript_path: None,
        payload,
        extensions,
    })
}

/// Builds the event-specific payload.
fn build_payload(
    kind: HookEventKind,
    document: &Value,
    redactor: &Redactor,
) -> Result<HookPayload, HookError> {
    match kind {
        HookEventKind::SessionStart => Ok(HookPayload::SessionStart {
            source: optional_string_field(document, "source")?
                .unwrap_or("unknown")
                .to_owned(),
        }),
        HookEventKind::UserPromptSubmit => Ok(HookPayload::UserPrompt {
            prompt: redactor.field(string_field(document, "prompt")?),
        }),
        HookEventKind::PreToolUse => Ok(HookPayload::PreToolUse {
            tool_name: string_field(document, "tool_name")?.to_owned(),
            tool_input: Box::new(redact_json(document, "tool_input", redactor)?),
        }),
        HookEventKind::PostToolUse | HookEventKind::PostToolUseFailure => {
            Ok(HookPayload::PostToolUse(Box::new(ToolResult {
                tool_name: string_field(document, "tool_name")?.to_owned(),
                tool_input: redact_json(document, "tool_input", redactor)?,
                tool_response: redact_json(document, "tool_response", redactor)?,
                error: optional_string_field(document, "error")?.map(|error| redactor.field(error)),
            })))
        }
        HookEventKind::Stop => Ok(HookPayload::Stop {
            // `OpenCode` has no stop-hook concept; the field keeps the shared model stable.
            stop_hook_active: false,
        }),
        HookEventKind::SessionEnd => Ok(HookPayload::SessionEnd {
            reason: optional_string_field(document, "reason")?
                .unwrap_or("unknown")
                .to_owned(),
        }),
    }
}

/// Reads a required string field.
fn string_field<'a>(document: &'a Value, field: &'static str) -> Result<&'a str, HookError> {
    match document.get(field) {
        None | Some(Value::Null) => Err(HookError::MissingField { field }),
        Some(Value::String(text)) => Ok(text),
        Some(_) => Err(HookError::FieldType {
            field,
            expected: "string",
        }),
    }
}

/// Reads an optional string field, refusing a present-but-wrongly-typed one.
fn optional_string_field<'a>(
    document: &'a Value,
    field: &'static str,
) -> Result<Option<&'a str>, HookError> {
    match document.get(field) {
        None | Some(Value::Null) => Ok(None),
        Some(Value::String(text)) => Ok(Some(text)),
        Some(_) => Err(HookError::FieldType {
            field,
            expected: "string",
        }),
    }
}

/// Renders a required structured field as canonical JSON, then redacts it.
fn redact_json(
    document: &Value,
    field: &'static str,
    redactor: &Redactor,
) -> Result<Redacted<String>, HookError> {
    let value = document
        .get(field)
        .ok_or(HookError::MissingField { field })?;
    let canonical = agent_jit_domain::canonical::canonical_string(value)?;
    Ok(redactor.field(&canonical))
}

/// Collects unknown top-level fields as bounded, redacted metadata.
fn extension_metadata(document: &Value, redactor: &Redactor) -> BTreeMap<String, String> {
    let Some(object) = document.as_object() else {
        return BTreeMap::new();
    };

    let mut extensions = BTreeMap::new();
    for (key, value) in object {
        if KNOWN_FIELDS.contains(&key.as_str()) || extensions.len() >= MAX_EXTENSIONS {
            continue;
        }

        let rendered = match value {
            Value::String(text) => text.clone(),
            other => other.to_string(),
        };
        let mut redacted = redactor.field(&rendered).into_value();
        if redacted.len() > MAX_EXTENSION_BYTES {
            let mut boundary = MAX_EXTENSION_BYTES;
            while boundary > 0 && !redacted.is_char_boundary(boundary) {
                boundary -= 1;
            }
            redacted.truncate(boundary);
        }
        extensions.insert(key.clone(), redacted);
    }
    extensions
}
