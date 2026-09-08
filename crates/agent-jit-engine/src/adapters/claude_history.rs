//! Opt-in importer for Claude Code's internal transcript JSONL.
//!
//! Claude's transcript format is an implementation detail, not a contract. This importer therefore
//! exists only to backfill *historical* sessions recorded before the recorder was installed, and it
//! is deliberately hostile to ambiguity: a file is imported only when it matches a named profile
//! exactly, and every other file is rejected with a reason code and counted. There is no permissive
//! auto-detection, because a misread historical file would silently inflate the corpus that a
//! stop/go gate divides by.
//!
//! Nothing here bypasses the live pipeline: accepted records go through the same redaction and the
//! same domain contracts as hook events, and carry provenance marking them as imported.

use std::collections::BTreeSet;
use std::io::{BufRead, BufReader};
use std::path::{Path, PathBuf};

use agent_jit_domain::canonical::Digest;
use agent_jit_domain::redaction::{Redacted, Redactor};
use serde::Serialize;
use serde_json::Value;

/// Identifier of the one importer profile this build implements.
pub const PROFILE_ID: &str = "claude_code.v2_1.jsonl";

/// Claude CLI major/minor series this profile understands.
const SUPPORTED_VERSION_PREFIX: &str = "2.1.";

/// Maximum bytes accepted for one transcript line.
///
/// This bound exists to cap allocation while parsing, not to limit what is retained — retention is
/// bounded separately and much more tightly by redaction. It is set to the trajectory ceiling the
/// rest of the system already uses, because a transcript line is one record and a record may
/// legitimately embed a whole file that the agent read.
///
/// A smaller cap is not "safer" in any useful sense and is actively harmful: at 1 MiB it rejected
/// three real pilot sessions carrying 202, 281, and 378 assistant turns, because one tool result in
/// each exceeded it. The largest line measured across the real corpus is 1.34 MiB.
pub const MAX_LINE_BYTES: usize = agent_jit_domain::redaction::MAX_TRAJECTORY_BYTES;

/// Maximum bytes accepted for one transcript file.
pub const MAX_FILE_BYTES: u64 = 64 * 1024 * 1024;

/// Directory name whose contents are subagent transcripts, never main sessions.
const SUBAGENT_DIR: &str = "subagents";

/// Why a transcript file was not imported.
///
/// Every variant is a *reason code*, not a diagnostic: the Phase 0 report counts rejections by
/// reason, so a corpus that fell short can be explained rather than guessed at.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum RejectionReason {
    /// The file lives under a `subagents/` directory.
    SubagentTranscript,
    /// A record marked `isSidechain`, so this is not a main session.
    SidechainRecord,
    /// No record carried a version marker.
    MissingVersionMarker,
    /// The version marker is outside the supported series.
    UnsupportedVersion,
    /// Records disagreed about the Claude version, so the session spans an upgrade.
    MixedVersionMarkers,
    /// Records disagreed about the session identifier.
    MixedSessionIds,
    /// No record carried a session identifier.
    MissingSessionId,
    /// A working directory lay outside the target repository.
    RepositoryMismatch,
    /// A line was not a JSON object.
    MalformedRecord,
    /// A line exceeded the line ceiling.
    LineTooLarge,
    /// The file exceeded the file ceiling.
    FileTooLarge,
    /// The file contained no agent activity, so there is no trajectory to import.
    NoAgentActivity,
    /// The file could not be read.
    Unreadable,
    /// An identical file was already imported.
    AlreadyImported,
}

impl RejectionReason {
    /// Returns the stable `snake_case` code.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::SubagentTranscript => "subagent_transcript",
            Self::SidechainRecord => "sidechain_record",
            Self::MissingVersionMarker => "missing_version_marker",
            Self::UnsupportedVersion => "unsupported_version",
            Self::MixedVersionMarkers => "mixed_version_markers",
            Self::MixedSessionIds => "mixed_session_ids",
            Self::MissingSessionId => "missing_session_id",
            Self::RepositoryMismatch => "repository_mismatch",
            Self::MalformedRecord => "malformed_record",
            Self::LineTooLarge => "line_too_large",
            Self::FileTooLarge => "file_too_large",
            Self::NoAgentActivity => "no_agent_activity",
            Self::Unreadable => "unreadable",
            Self::AlreadyImported => "already_imported",
        }
    }
}

/// One tool call recovered from a transcript.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct ImportedToolCall {
    /// Tool name, e.g. `Bash`.
    pub tool_name: String,
    /// Redacted canonical JSON of the tool input.
    pub tool_input: Redacted<String>,
}

/// One session recovered from a transcript file.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct ImportedSession {
    /// Profile that accepted the file.
    pub profile: &'static str,
    /// Digest of the source file, used to detect a repeated import.
    pub source_digest: Digest,
    /// Path of the source file, for provenance.
    pub source_path: String,
    /// Claude's session identifier.
    pub session_key: String,
    /// Exact Claude CLI version observed.
    pub claude_version: String,
    /// Working directory reported by the session.
    pub cwd: String,
    /// Git branch reported by the session, when present.
    pub git_branch: Option<String>,
    /// The first user prompt: the session's stated intent.
    pub intent: Redacted<String>,
    /// Number of user prompts.
    pub prompts: u32,
    /// Number of assistant turns.
    pub assistant_turns: u32,
    /// Tool calls, in order.
    pub tool_calls: Vec<ImportedToolCall>,
    /// First timestamp observed, as reported by Claude.
    pub started_at: Option<String>,
    /// Last timestamp observed, as reported by Claude.
    pub ended_at: Option<String>,
    /// Lines that were skipped inside an otherwise-accepted file.
    pub skipped_lines: u32,
}

/// A file that was not imported, and why.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct RejectedFile {
    /// Path of the rejected file.
    pub source_path: String,
    /// Why it was rejected.
    pub reason: RejectionReason,
}

/// The result of scanning a project directory.
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize)]
pub struct ImportScan {
    /// Sessions that matched the profile.
    pub accepted: Vec<ImportedSession>,
    /// Files that did not, with reasons.
    pub rejected: Vec<RejectedFile>,
}

impl ImportScan {
    /// Counts rejections by reason, for the report.
    #[must_use]
    pub fn rejection_counts(&self) -> std::collections::BTreeMap<&'static str, u32> {
        let mut counts = std::collections::BTreeMap::new();
        for rejected in &self.rejected {
            *counts.entry(rejected.reason.as_str()).or_insert(0) += 1;
        }
        counts
    }
}

/// Scans `project_dir` for transcripts belonging to `worktree_root`.
///
/// Only top-level `*.jsonl` files are considered: Claude nests subagent transcripts under a
/// `subagents/` directory, and a subagent is not a session anyone ran.
///
/// `already_imported` holds digests of files imported previously, so a repeated import reports
/// `already_imported` instead of counting the same work twice.
#[must_use]
pub fn scan(
    project_dir: &Path,
    worktree_root: &Path,
    redactor: &Redactor,
    already_imported: &BTreeSet<Digest>,
) -> ImportScan {
    let mut scan = ImportScan::default();

    let mut paths: Vec<PathBuf> = Vec::new();
    collect_transcripts(project_dir, &mut paths, &mut scan);
    paths.sort();

    for path in paths {
        match read_session(&path, worktree_root, redactor, already_imported) {
            Ok(session) => scan.accepted.push(session),
            Err(reason) => scan.rejected.push(RejectedFile {
                source_path: path.to_string_lossy().into_owned(),
                reason,
            }),
        }
    }

    scan.accepted
        .sort_by(|left, right| left.source_path.cmp(&right.source_path));
    scan
}

/// Collects candidate transcripts, rejecting nested subagent files explicitly.
fn collect_transcripts(directory: &Path, paths: &mut Vec<PathBuf>, scan: &mut ImportScan) {
    let Ok(entries) = std::fs::read_dir(directory) else {
        return;
    };

    for entry in entries.flatten() {
        let path = entry.path();
        if path.is_dir() {
            // A subagent transcript is recorded as a rejection rather than ignored: "we saw it and
            // refused it" is a different claim from "we never looked".
            if path.file_name().and_then(|name| name.to_str()) == Some(SUBAGENT_DIR) {
                if let Ok(nested) = std::fs::read_dir(&path) {
                    for child in nested.flatten() {
                        if child.path().extension().and_then(|e| e.to_str()) == Some("jsonl") {
                            scan.rejected.push(RejectedFile {
                                source_path: child.path().to_string_lossy().into_owned(),
                                reason: RejectionReason::SubagentTranscript,
                            });
                        }
                    }
                }
            } else {
                collect_transcripts(&path, paths, scan);
            }
        } else if path.extension().and_then(|e| e.to_str()) == Some("jsonl") {
            paths.push(path);
        }
    }
}

/// Facts gathered while streaming one transcript.
#[derive(Default)]
struct Observed {
    versions: BTreeSet<String>,
    session_ids: BTreeSet<String>,
    cwds: BTreeSet<String>,
    git_branches: BTreeSet<String>,
    prompts: u32,
    assistant_turns: u32,
    tool_calls: Vec<ImportedToolCall>,
    intent: Option<String>,
    first_timestamp: Option<String>,
    last_timestamp: Option<String>,
    skipped_lines: u32,
}

/// Reads and validates one transcript file.
fn read_session(
    path: &Path,
    worktree_root: &Path,
    redactor: &Redactor,
    already_imported: &BTreeSet<Digest>,
) -> Result<ImportedSession, RejectionReason> {
    let metadata = std::fs::metadata(path).map_err(|_| RejectionReason::Unreadable)?;
    if metadata.len() > MAX_FILE_BYTES {
        return Err(RejectionReason::FileTooLarge);
    }

    // The source is hashed before it is interpreted, so a repeated import is detected even if the
    // interpretation later changes.
    let bytes = std::fs::read(path).map_err(|_| RejectionReason::Unreadable)?;
    let source_digest = Digest::of_bytes(&bytes);
    if already_imported.contains(&source_digest) {
        return Err(RejectionReason::AlreadyImported);
    }

    let file = std::fs::File::open(path).map_err(|_| RejectionReason::Unreadable)?;
    let mut observed = Observed::default();

    for line in BufReader::new(file).lines() {
        let line = line.map_err(|_| RejectionReason::Unreadable)?;
        if line.trim().is_empty() {
            continue;
        }
        if line.len() > MAX_LINE_BYTES {
            return Err(RejectionReason::LineTooLarge);
        }

        let record: Value =
            serde_json::from_str(&line).map_err(|_| RejectionReason::MalformedRecord)?;
        let Some(object) = record.as_object() else {
            return Err(RejectionReason::MalformedRecord);
        };

        if object
            .get("isSidechain")
            .and_then(Value::as_bool)
            .unwrap_or(false)
        {
            return Err(RejectionReason::SidechainRecord);
        }

        observe(&record, &mut observed, redactor);
    }

    finish(path, source_digest, worktree_root, observed)
}

/// Accumulates one record's facts.
fn observe(record: &Value, observed: &mut Observed, redactor: &Redactor) {
    for (field, target) in [
        ("version", &mut observed.versions),
        ("sessionId", &mut observed.session_ids),
        ("cwd", &mut observed.cwds),
        ("gitBranch", &mut observed.git_branches),
    ] {
        if let Some(value) = record.get(field).and_then(Value::as_str)
            && !value.is_empty()
        {
            target.insert(value.to_owned());
        }
    }

    if let Some(timestamp) = record.get("timestamp").and_then(Value::as_str) {
        observed
            .first_timestamp
            .get_or_insert_with(|| timestamp.to_owned());
        observed.last_timestamp = Some(timestamp.to_owned());
    }

    match record.get("type").and_then(Value::as_str) {
        Some("user") => {
            // `isMeta` marks a synthetic user turn — a tool result Claude feeds back to itself —
            // rather than something a person typed.
            let is_meta = record
                .get("isMeta")
                .and_then(Value::as_bool)
                .unwrap_or(false);
            if !is_meta {
                observed.prompts = observed.prompts.saturating_add(1);
                if observed.intent.is_none()
                    && let Some(text) = user_text(record)
                {
                    observed.intent = Some(redactor.field(&text).into_value());
                }
            }
        }
        Some("assistant") => {
            observed.assistant_turns = observed.assistant_turns.saturating_add(1);
            collect_tool_calls(record, observed, redactor);
        }
        _ => observed.skipped_lines = observed.skipped_lines.saturating_add(1),
    }
}

/// Extracts the text of a user message, which may be a string or a content array.
fn user_text(record: &Value) -> Option<String> {
    let content = record.get("message")?.get("content")?;
    match content {
        Value::String(text) => Some(text.clone()),
        Value::Array(blocks) => {
            let mut text = String::new();
            for block in blocks {
                if block.get("type").and_then(Value::as_str) == Some("text")
                    && let Some(chunk) = block.get("text").and_then(Value::as_str)
                {
                    text.push_str(chunk);
                }
            }
            (!text.is_empty()).then_some(text)
        }
        _ => None,
    }
}

/// Collects the tool calls an assistant turn made.
fn collect_tool_calls(record: &Value, observed: &mut Observed, redactor: &Redactor) {
    let Some(blocks) = record
        .get("message")
        .and_then(|message| message.get("content"))
        .and_then(Value::as_array)
    else {
        return;
    };

    for block in blocks {
        if block.get("type").and_then(Value::as_str) != Some("tool_use") {
            continue;
        }
        let Some(name) = block.get("name").and_then(Value::as_str) else {
            continue;
        };
        let input = block.get("input").unwrap_or(&Value::Null);
        let canonical = agent_jit_domain::canonical::canonical_string(input)
            .unwrap_or_else(|_| "{}".to_owned());
        observed.tool_calls.push(ImportedToolCall {
            tool_name: name.to_owned(),
            tool_input: redactor.field(&canonical),
        });
    }
}

/// Applies the profile's acceptance rules to what was observed.
fn finish(
    path: &Path,
    source_digest: Digest,
    worktree_root: &Path,
    observed: Observed,
) -> Result<ImportedSession, RejectionReason> {
    if observed.versions.is_empty() {
        return Err(RejectionReason::MissingVersionMarker);
    }
    if observed.versions.len() > 1 {
        // A session resumed across a Claude upgrade spans two behaviours; grouping it with either
        // would be a guess, so it is refused rather than attributed.
        return Err(RejectionReason::MixedVersionMarkers);
    }
    let claude_version = observed.versions.iter().next().cloned().unwrap_or_default();
    if !claude_version.starts_with(SUPPORTED_VERSION_PREFIX) {
        return Err(RejectionReason::UnsupportedVersion);
    }

    if observed.session_ids.is_empty() {
        return Err(RejectionReason::MissingSessionId);
    }
    if observed.session_ids.len() > 1 {
        return Err(RejectionReason::MixedSessionIds);
    }
    let session_key = observed
        .session_ids
        .iter()
        .next()
        .cloned()
        .unwrap_or_default();

    // Every working directory must be inside the repository being imported into. A session that
    // wandered into a subdirectory is still this repository; one outside it is not.
    let root = worktree_root
        .canonicalize()
        .unwrap_or_else(|_| worktree_root.to_path_buf());
    if observed.cwds.is_empty() || !observed.cwds.iter().all(|cwd| within(&root, cwd)) {
        return Err(RejectionReason::RepositoryMismatch);
    }

    if observed.assistant_turns == 0 {
        return Err(RejectionReason::NoAgentActivity);
    }

    let cwd = observed.cwds.iter().next().cloned().unwrap_or_default();

    Ok(ImportedSession {
        profile: PROFILE_ID,
        source_digest,
        source_path: path.to_string_lossy().into_owned(),
        session_key,
        claude_version,
        cwd,
        git_branch: observed.git_branches.iter().next().cloned(),
        intent: Redactor::new().field(&observed.intent.unwrap_or_default()),
        prompts: observed.prompts,
        assistant_turns: observed.assistant_turns,
        tool_calls: observed.tool_calls,
        started_at: observed.first_timestamp,
        ended_at: observed.last_timestamp,
        skipped_lines: observed.skipped_lines,
    })
}

/// Whether `candidate` is `root` or lies inside it.
fn within(root: &Path, candidate: &str) -> bool {
    let path = Path::new(candidate);
    let resolved = path.canonicalize().unwrap_or_else(|_| path.to_path_buf());
    resolved.starts_with(root)
}
