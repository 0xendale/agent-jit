#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
//! The historical importer is deliberately hostile to ambiguity.
//!
//! Every test is named `historical_import_*` so `cargo test -p agent-jit-engine historical_import_`
//! selects exactly this suite.

use std::collections::BTreeSet;
use std::path::{Path, PathBuf};

use agent_jit_domain::canonical::Digest;
use agent_jit_domain::redaction::Redactor;
use agent_jit_engine::adapters::claude_history::{
    MAX_LINE_BYTES, PROFILE_ID, RejectionReason, scan,
};

fn fixtures() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/claude-history")
}

/// The fixtures claim `/repo` as their working directory, so the import target is a real directory
/// standing in for it.
fn repo_root() -> PathBuf {
    PathBuf::from("/repo")
}

fn scan_dir(
    directory: &Path,
    seen: &BTreeSet<Digest>,
) -> agent_jit_engine::adapters::claude_history::ImportScan {
    scan(directory, &repo_root(), &Redactor::new(), seen)
}

fn reason_for(
    scan: &agent_jit_engine::adapters::claude_history::ImportScan,
    file: &str,
) -> RejectionReason {
    scan.rejected
        .iter()
        .find(|rejected| rejected.source_path.ends_with(file))
        .unwrap_or_else(|| panic!("{file} was not rejected: {:?}", scan.rejected))
        .reason
}

#[test]
fn historical_import_accepts_only_supported_sessions() {
    let scan = scan_dir(&fixtures().join("supported"), &BTreeSet::new());

    let mut accepted: Vec<&str> = scan
        .accepted
        .iter()
        .map(|session| session.source_path.rsplit('/').next().unwrap_or_default())
        .collect();
    accepted.sort_unstable();
    assert_eq!(
        accepted,
        vec![
            "session-complete.jsonl",
            "session-second.jsonl",
            "session-with-secrets.jsonl"
        ],
        "{scan:?}"
    );

    for session in &scan.accepted {
        assert_eq!(session.profile, PROFILE_ID);
        assert!(session.claude_version.starts_with("2.1."));
    }
}

#[test]
fn historical_import_recovers_the_intent_and_the_tool_calls() {
    let scan = scan_dir(&fixtures().join("supported"), &BTreeSet::new());
    let session = scan
        .accepted
        .iter()
        .find(|session| session.source_path.ends_with("session-complete.jsonl"))
        .unwrap();

    assert_eq!(
        session.intent.value(),
        "validate this change the way this repository expects"
    );
    assert_eq!(session.prompts, 1, "a tool result is not a user prompt");
    assert_eq!(session.assistant_turns, 2);
    assert_eq!(session.tool_calls.len(), 1);
    assert_eq!(session.tool_calls[0].tool_name, "Bash");
    assert!(
        session.tool_calls[0]
            .tool_input
            .value()
            .contains("cargo test --workspace")
    );
    assert_eq!(session.session_key, "11111111-2222-3333-4444-555555555555");
    assert_eq!(session.claude_version, "2.1.223");
}

#[test]
fn historical_import_never_reads_a_subagent_transcript() {
    let scan = scan_dir(&fixtures().join("supported"), &BTreeSet::new());
    assert_eq!(
        reason_for(&scan, "agent-abc123.jsonl"),
        RejectionReason::SubagentTranscript
    );
    assert!(
        !scan
            .accepted
            .iter()
            .any(|session| session.source_path.contains("subagents")),
        "a subagent transcript must never be imported"
    );
}

#[test]
fn historical_import_rejects_every_unsupported_shape_atomically() {
    let scan = scan_dir(&fixtures().join("rejected"), &BTreeSet::new());
    assert!(
        scan.accepted.is_empty(),
        "nothing in the rejected fixtures may be accepted: {:?}",
        scan.accepted
    );

    for (file, expected) in [
        ("sidechain.jsonl", RejectionReason::SidechainRecord),
        (
            "unsupported-version.jsonl",
            RejectionReason::UnsupportedVersion,
        ),
        ("no-version.jsonl", RejectionReason::MissingVersionMarker),
        ("mixed-versions.jsonl", RejectionReason::MixedVersionMarkers),
        ("mixed-sessions.jsonl", RejectionReason::MixedSessionIds),
        ("wrong-repo.jsonl", RejectionReason::RepositoryMismatch),
        ("malformed-line.jsonl", RejectionReason::MalformedRecord),
        ("no-agent-activity.jsonl", RejectionReason::NoAgentActivity),
    ] {
        assert_eq!(reason_for(&scan, file), expected, "{file}");
    }
}

#[test]
fn historical_import_counts_rejections_by_reason() {
    let scan = scan_dir(&fixtures().join("rejected"), &BTreeSet::new());
    let counts = scan.rejection_counts();

    // Every rejection is attributable; a corpus that fell short can be explained.
    assert_eq!(counts.values().sum::<u32>() as usize, scan.rejected.len());
    assert_eq!(counts.get("mixed_version_markers"), Some(&1));
    assert_eq!(counts.get("repository_mismatch"), Some(&1));
}

#[test]
fn historical_import_is_deterministic() {
    let first = scan_dir(&fixtures().join("supported"), &BTreeSet::new());
    let second = scan_dir(&fixtures().join("supported"), &BTreeSet::new());
    assert_eq!(first, second);

    // The source digest identifies the file, so it is stable across runs.
    for (left, right) in first.accepted.iter().zip(second.accepted.iter()) {
        assert_eq!(left.source_digest, right.source_digest);
    }
}

#[test]
fn historical_import_reports_an_already_imported_file_rather_than_counting_it_twice() {
    let first = scan_dir(&fixtures().join("supported"), &BTreeSet::new());
    let seen: BTreeSet<Digest> = first
        .accepted
        .iter()
        .map(|session| session.source_digest)
        .collect();

    let second = scan_dir(&fixtures().join("supported"), &seen);
    assert!(second.accepted.is_empty());
    assert_eq!(
        second.rejection_counts().get("already_imported"),
        Some(&3),
        "{second:?}"
    );
}

#[test]
fn historical_import_redacts_secrets_before_they_leave_the_importer() {
    let scan = scan_dir(&fixtures().join("supported"), &BTreeSet::new());
    let rendered = serde_json::to_string(&scan).unwrap();
    assert!(
        !rendered.contains("HISTCANARY-9c1f"),
        "a credential survived the importer"
    );

    let session = scan
        .accepted
        .iter()
        .find(|session| session.source_path.ends_with("session-with-secrets.jsonl"))
        .unwrap();
    assert!(
        session.tool_calls[0]
            .tool_input
            .value()
            .contains("[redacted:authorization]"),
        "{}",
        session.tool_calls[0].tool_input.value()
    );
}

#[test]
fn historical_import_of_an_empty_directory_is_empty_rather_than_an_error() {
    let temp = tempfile::tempdir().unwrap();
    let scan = scan_dir(temp.path(), &BTreeSet::new());
    assert!(scan.accepted.is_empty());
    assert!(scan.rejected.is_empty());
}

#[test]
fn historical_import_rejects_a_line_past_the_parse_ceiling() {
    let temp = tempfile::tempdir().unwrap();
    let path = temp.path().join("huge.jsonl");
    let huge = format!(
        "{{\"type\":\"user\",\"sessionId\":\"s\",\"cwd\":\"/repo\",\"version\":\"2.1.223\",\"message\":{{\"role\":\"user\",\"content\":\"{}\"}}}}",
        "x".repeat(MAX_LINE_BYTES + 1)
    );
    std::fs::write(&path, huge).unwrap();

    let scan = scan_dir(temp.path(), &BTreeSet::new());
    assert!(scan.accepted.is_empty());
    assert_eq!(
        reason_for(&scan, "huge.jsonl"),
        RejectionReason::LineTooLarge
    );
}

/// Largest single line measured across the real pilot corpus: one tool result that embedded a file
/// the agent had read. The parse ceiling must sit above this, or sessions with hundreds of turns are
/// discarded for containing one big record.
const LARGEST_REAL_LINE_BYTES: usize = 1_341_760;

// A parse ceiling at or below what real transcripts contain is a defect, so it fails the build
// rather than the test run.
const _: () = assert!(MAX_LINE_BYTES > LARGEST_REAL_LINE_BYTES);

#[test]
fn historical_import_accepts_a_line_a_real_transcript_actually_contains() {
    let temp = tempfile::tempdir().unwrap();
    let big_but_real = "y".repeat(LARGEST_REAL_LINE_BYTES);
    let records = [
        "{\"type\":\"user\",\"isMeta\":false,\"sessionId\":\"s1\",\"cwd\":\"/repo\",\"version\":\"2.1.223\",\"message\":{\"role\":\"user\",\"content\":\"do it\"}}".to_owned(),
        format!(
            "{{\"type\":\"assistant\",\"sessionId\":\"s1\",\"cwd\":\"/repo\",\"version\":\"2.1.223\",\"message\":{{\"role\":\"assistant\",\"content\":[{{\"type\":\"text\",\"text\":\"{big_but_real}\"}}]}}}}"
        ),
    ];
    std::fs::write(temp.path().join("big.jsonl"), records.join("\n")).unwrap();

    let scan = scan_dir(temp.path(), &BTreeSet::new());
    assert_eq!(scan.accepted.len(), 1, "{:?}", scan.rejected);
    assert_eq!(scan.accepted[0].assistant_turns, 1);
}
