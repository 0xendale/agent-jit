//! The Claude Code hook adapter.
//!
//! This is the only place the product knows what Claude Code sends. It reads the *documented* hook
//! payload from stdin and nothing else: `transcript_path` is recorded as provenance and never
//! opened, because Claude's internal transcript JSONL is not a stable contract and reading it
//! would make the recorder depend on an implementation detail (see
//! `docs/adr/0002-claude-code-first-runtime.md`).
//!
//! Unknown fields are kept — bounded and redacted — as extension metadata rather than dropped: a
//! later Claude release adding a field should show up in the evidence, not vanish. They are not
//! part of the canonical form, so their arrival cannot change a digest computed before they
//! existed.

use std::collections::BTreeMap;
use std::str::FromStr;

use agent_jit_domain::canonical::{CanonicalError, Digest, digest_of};
use agent_jit_domain::redaction::{Redacted, Redactor};
use serde::{Deserialize, Serialize};
use serde_json::Value;

/// Identifier of this adapter's normalization contract. Stamped on every event.
pub const ADAPTER_SCHEMA: &str = "agent_jit.claude_hooks.v1";

/// Maximum bytes kept per extension metadata value.
const MAX_EXTENSION_BYTES: usize = 1024;

/// Maximum number of unknown fields kept per event.
const MAX_EXTENSIONS: usize = 16;

/// Which hook fired.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum HookEventKind {
    /// A session started.
    SessionStart,
    /// The user submitted a prompt.
    UserPromptSubmit,
    /// A tool is about to run.
    PreToolUse,
    /// A tool finished.
    PostToolUse,
    /// A tool finished with an error. Claude reports this through `PostToolUse` with an `error`
    /// field; the distinction is kept here because the recorder treats failure differently.
    PostToolUseFailure,
    /// The agent stopped.
    Stop,
    /// The session ended.
    SessionEnd,
}

impl HookEventKind {
    /// Every hook this adapter accepts.
    pub const ALL: [Self; 7] = [
        Self::SessionStart,
        Self::UserPromptSubmit,
        Self::PreToolUse,
        Self::PostToolUse,
        Self::PostToolUseFailure,
        Self::Stop,
        Self::SessionEnd,
    ];

    /// Returns the CLI-facing name, e.g. `post-tool-use`.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::SessionStart => "session-start",
            Self::UserPromptSubmit => "user-prompt-submit",
            Self::PreToolUse => "pre-tool-use",
            Self::PostToolUse => "post-tool-use",
            Self::PostToolUseFailure => "post-tool-use-failure",
            Self::Stop => "stop",
            Self::SessionEnd => "session-end",
        }
    }

    /// Returns the `hook_event_name` Claude Code sends for this hook.
    #[must_use]
    pub const fn claude_event_name(self) -> &'static str {
        match self {
            Self::SessionStart => "SessionStart",
            Self::UserPromptSubmit => "UserPromptSubmit",
            Self::PreToolUse => "PreToolUse",
            // A failure arrives as a PostToolUse carrying an error.
            Self::PostToolUse | Self::PostToolUseFailure => "PostToolUse",
            Self::Stop => "Stop",
            Self::SessionEnd => "SessionEnd",
        }
    }
}

impl FromStr for HookEventKind {
    type Err = HookError;

    fn from_str(text: &str) -> Result<Self, Self::Err> {
        Self::ALL
            .into_iter()
            .find(|kind| kind.as_str() == text)
            .ok_or_else(|| HookError::UnknownEvent {
                found: text.to_owned(),
            })
    }
}

/// A tool call and what it produced.
///
/// Boxed inside [`HookPayload`]: tool payloads dwarf the other variants, and an enum sized for the
/// largest one would make every event that much bigger.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ToolResult {
    /// Tool name.
    pub tool_name: String,
    /// Canonical JSON of the tool input.
    pub tool_input: Redacted<String>,
    /// Canonical JSON of the tool response.
    pub tool_response: Redacted<String>,
    /// Error text, when the tool failed.
    pub error: Option<Redacted<String>>,
}

/// The event-specific part of a normalized hook.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "payload", rename_all = "snake_case")]
pub enum HookPayload {
    /// A session started.
    SessionStart {
        /// How the session began, e.g. `startup` or `resume`.
        source: String,
    },
    /// The user submitted a prompt: the stated intent of whatever follows.
    UserPrompt {
        /// The prompt text.
        prompt: Redacted<String>,
    },
    /// A tool is about to run.
    PreToolUse {
        /// Tool name, e.g. `Bash`.
        tool_name: String,
        /// Canonical JSON of the tool input.
        tool_input: Box<Redacted<String>>,
    },
    /// A tool finished, with or without an error.
    PostToolUse(Box<ToolResult>),
    /// The agent stopped.
    Stop {
        /// Whether a stop hook was already active.
        stop_hook_active: bool,
    },
    /// The session ended.
    SessionEnd {
        /// Why the session ended.
        reason: String,
    },
}

/// One hook payload, normalized, redacted, and bounded.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct NormalizedHook {
    /// Adapter contract this event was produced by. Always [`ADAPTER_SCHEMA`] for events this
    /// build produced; carried as data so a stored event states which contract normalized it.
    pub adapter_schema: String,
    /// Which hook fired.
    pub kind: HookEventKind,
    /// Claude's session identifier, used to correlate events before a session record exists.
    pub session_key: String,
    /// Working directory Claude reported.
    pub cwd: String,
    /// Exact Claude CLI version observed, when the launcher could determine it.
    pub claude_version: Option<String>,
    /// Path Claude reported for its transcript. Provenance only: never opened.
    pub transcript_path: Option<String>,
    /// Event-specific fields.
    pub payload: HookPayload,
    /// Unknown top-level fields, redacted and bounded. Not part of the canonical form.
    pub extensions: BTreeMap<String, String>,
}

impl NormalizedHook {
    /// Digests the canonical form of the event.
    ///
    /// Extension metadata is excluded: a field added by a future Claude release must not change
    /// the identity of an event whose meaning did not change.
    ///
    /// # Errors
    ///
    /// Returns [`CanonicalError`] when the event cannot be canonicalized.
    pub fn canonical_digest(&self) -> Result<Digest, CanonicalError> {
        let mut value = agent_jit_domain::canonical::to_value(self)?;
        if let Some(object) = value.as_object_mut() {
            object.remove("extensions");
        }
        digest_of(&value)
    }
}

/// Why a hook payload was refused.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum HookError {
    /// The event name is not one this adapter handles.
    #[error("`{found}` is not a hook this adapter handles")]
    UnknownEvent {
        /// The rejected name.
        found: String,
    },
    /// The payload was not JSON.
    #[error("hook payload is not JSON: {reason}")]
    MalformedJson {
        /// Parser message. Never carries payload bytes.
        reason: String,
    },
    /// A required field was absent.
    #[error("hook payload is missing `{field}`")]
    MissingField {
        /// The absent field.
        field: &'static str,
    },
    /// A field was present with the wrong type.
    #[error("hook payload field `{field}` is not a {expected}")]
    FieldType {
        /// The offending field.
        field: &'static str,
        /// What was expected.
        expected: &'static str,
    },
    /// The payload described a different hook than the one requested.
    #[error("payload is `{found}` but `{expected}` was requested")]
    EventMismatch {
        /// Event name in the payload.
        found: String,
        /// Event name that was requested.
        expected: &'static str,
    },
    /// The normalized event could not be canonicalized.
    #[error("{}: {source}", source.code())]
    Canonical {
        /// The underlying failure.
        #[from]
        source: CanonicalError,
    },
}

impl HookError {
    /// Stable machine-readable code.
    #[must_use]
    pub const fn code(&self) -> &'static str {
        match self {
            Self::UnknownEvent { .. } => "hook_unknown_event",
            Self::MalformedJson { .. } => "hook_malformed_json",
            Self::MissingField { .. } => "hook_missing_field",
            Self::FieldType { .. } => "hook_field_type",
            Self::EventMismatch { .. } => "hook_event_mismatch",
            Self::Canonical { source } => source.code(),
        }
    }
}

/// Fields the adapter understands. Anything else becomes extension metadata.
const KNOWN_FIELDS: &[&str] = &[
    "session_id",
    "transcript_path",
    "cwd",
    "hook_event_name",
    "permission_mode",
    "source",
    "prompt",
    "tool_name",
    "tool_input",
    "tool_response",
    "error",
    "stop_hook_active",
    "reason",
];

/// Normalizes one hook payload.
///
/// # Errors
///
/// Returns [`HookError`] when the payload is not JSON, is missing a required field, has a field of
/// the wrong type, or describes a different hook. No event is produced in any of those cases.
pub fn normalize(
    kind: HookEventKind,
    payload: &str,
    redactor: &Redactor,
    claude_version: Option<&str>,
) -> Result<NormalizedHook, HookError> {
    let document: Value =
        serde_json::from_str(payload).map_err(|error| HookError::MalformedJson {
            // `serde_json`'s message names the position, not the content.
            reason: format!("line {}, column {}", error.line(), error.column()),
        })?;

    let event_name = string_field(&document, "hook_event_name")?;
    if event_name != kind.claude_event_name() {
        return Err(HookError::EventMismatch {
            found: event_name.to_owned(),
            expected: kind.claude_event_name(),
        });
    }

    let session_key = string_field(&document, "session_id")?.to_owned();
    let cwd = string_field(&document, "cwd")?.to_owned();
    let transcript_path = optional_string_field(&document, "transcript_path")?.map(str::to_owned);

    let payload = build_payload(kind, &document, redactor)?;
    let extensions = extension_metadata(&document, redactor);

    Ok(NormalizedHook {
        adapter_schema: ADAPTER_SCHEMA.to_owned(),
        kind,
        session_key,
        cwd,
        claude_version: claude_version.map(str::to_owned),
        transcript_path,
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
            stop_hook_active: document
                .get("stop_hook_active")
                .and_then(Value::as_bool)
                .unwrap_or(false),
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
