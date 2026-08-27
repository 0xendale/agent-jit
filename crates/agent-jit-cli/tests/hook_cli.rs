#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
//! `agent-jit hook ingest` runs inside Claude's critical path: it never prints to stdout, never
//! blocks Claude's work with a nonzero exit, and never persists something it could not parse.

use std::path::Path;

use assert_cmd::Command;
use predicates::str::contains;

fn bin(home: &Path) -> Command {
    let mut command = Command::cargo_bin("agent-jit").unwrap();
    command.env("AGENT_JIT_HOME", home);
    command
}

fn fixture(name: &str) -> String {
    let path = format!(
        "{}/../agent-jit-engine/tests/fixtures/claude-hooks/{name}",
        env!("CARGO_MANIFEST_DIR")
    );
    std::fs::read_to_string(&path).unwrap_or_else(|error| panic!("{path}: {error}"))
}

fn spool_lines(home: &Path, name: &str) -> Vec<String> {
    let path = home.join("cache/spool").join(name);
    if !path.exists() {
        return Vec::new();
    }
    std::fs::read_to_string(path)
        .unwrap()
        .lines()
        .map(str::to_owned)
        .collect()
}

/// Every spooled segment, in file-name order (which is chronological).
fn segments(home: &Path) -> Vec<serde_json::Value> {
    let root = home.join("cache/spool/segments");
    let mut files = Vec::new();
    let Ok(sessions) = std::fs::read_dir(&root) else {
        return files;
    };
    for session in sessions.flatten() {
        if !session.path().is_dir() || session.file_name() == "quarantine" {
            continue;
        }
        let mut paths: Vec<_> = std::fs::read_dir(session.path())
            .unwrap()
            .flatten()
            .map(|entry| entry.path())
            .filter(|path| path.is_file())
            .collect();
        paths.sort();
        for path in paths {
            files.push(
                serde_json::from_str(&std::fs::read_to_string(&path).unwrap())
                    .expect("segments are JSON"),
            );
        }
    }
    files
}

#[test]
fn every_hook_fixture_produces_one_spooled_event_and_no_stdout() {
    let cases = [
        ("session-start", "session-start.json"),
        ("user-prompt-submit", "user-prompt-submit.json"),
        ("pre-tool-use", "pre-tool-use.json"),
        ("post-tool-use", "post-tool-use.json"),
        ("post-tool-use-failure", "post-tool-use-failure.json"),
        ("stop", "stop.json"),
        ("session-end", "session-end.json"),
    ];

    for (event, file) in cases {
        let home = tempfile::tempdir().unwrap();
        bin(home.path())
            .args(["hook", "ingest", "--event", event])
            .write_stdin(fixture(file))
            .assert()
            .success()
            .stdout(predicates::str::is_empty());

        let written = segments(home.path());
        assert_eq!(written.len(), 1, "{event}: {written:?}");
        let parsed = &written[0]["payload"];
        assert_eq!(parsed["adapter_schema"], "agent_jit.claude_hooks.v1");
        assert_eq!(parsed["kind"], event.replace('-', "_"));
        assert_eq!(written[0]["segment_version"], 1);
        assert_eq!(written[0]["event_id"], written[0]["checksum"]);
        assert!(spool_lines(home.path(), "health.jsonl").is_empty());
    }
}

#[test]
fn malformed_input_is_counted_as_health_and_never_persisted_as_an_event() {
    let home = tempfile::tempdir().unwrap();
    bin(home.path())
        .args(["hook", "ingest", "--event", "user-prompt-submit"])
        .write_stdin("{not json")
        .assert()
        // A recorder fault must never fail Claude's own work.
        .success()
        .stdout(predicates::str::is_empty())
        .stderr(contains("hook_malformed_json"));

    assert!(segments(home.path()).is_empty());
    let health = spool_lines(home.path(), "health.jsonl");
    assert_eq!(health.len(), 1, "{health:?}");
    assert!(health[0].contains("hook_malformed_json"));
}

#[test]
fn a_missing_required_field_is_counted_as_health() {
    let home = tempfile::tempdir().unwrap();
    bin(home.path())
        .args(["hook", "ingest", "--event", "user-prompt-submit"])
        .write_stdin(fixture("missing-session-id.json"))
        .assert()
        .success()
        .stdout(predicates::str::is_empty());

    assert!(segments(home.path()).is_empty());
    assert!(spool_lines(home.path(), "health.jsonl")[0].contains("hook_missing_field"));
}

#[test]
fn an_oversized_payload_is_bounded_and_still_exits_zero() {
    let home = tempfile::tempdir().unwrap();
    let huge = format!(
        "{{\"session_id\":\"s\",\"cwd\":\"/tmp\",\"hook_event_name\":\"UserPromptSubmit\",\"prompt\":\"{}\"}}",
        "x".repeat(4 * 1024 * 1024)
    );

    bin(home.path())
        .args(["hook", "ingest", "--event", "user-prompt-submit"])
        .write_stdin(huge)
        .assert()
        .success()
        .stdout(predicates::str::is_empty());

    // Bounded reading cuts the JSON short, so this is a parse failure counted as health — the
    // point is that the recorder neither grew without limit nor took Claude down with it.
    assert!(segments(home.path()).is_empty());
    assert_eq!(spool_lines(home.path(), "health.jsonl").len(), 1);
}

#[test]
fn secrets_never_reach_the_spool() {
    let home = tempfile::tempdir().unwrap();
    bin(home.path())
        .args(["hook", "ingest", "--event", "post-tool-use"])
        .write_stdin(fixture("with-secrets.json"))
        .assert()
        .success();

    let spooled = segments(home.path())
        .iter()
        .map(ToString::to_string)
        .collect::<Vec<_>>()
        .join("\n");
    for canary in [
        "CANARY-header-9c1f",
        "CANARY-url-9c1f",
        "CANARY-query-9c1f",
        "CANARY-env-9c1f",
        "CANARY-pem-9c1f",
    ] {
        assert!(!spooled.contains(canary), "{canary} reached the spool");
    }
}

#[test]
fn an_unknown_event_name_is_a_usage_error_before_anything_is_read() {
    let home = tempfile::tempdir().unwrap();
    bin(home.path())
        .args(["hook", "ingest", "--event", "not-a-hook"])
        .write_stdin("{}")
        .assert()
        .code(2)
        .stderr(contains("usage: agent-jit hook ingest"));
}

#[test]
fn the_transcript_path_is_recorded_but_never_read() {
    let home = tempfile::tempdir().unwrap();
    bin(home.path())
        .args(["hook", "ingest", "--event", "session-start"])
        .write_stdin(fixture("session-start.json"))
        .assert()
        .success();

    let written = segments(home.path());
    let parsed = &written[0]["payload"];
    assert!(
        parsed["transcript_path"]
            .as_str()
            .unwrap()
            .ends_with("b2c3d4e5.jsonl"),
        "{parsed}"
    );
}
