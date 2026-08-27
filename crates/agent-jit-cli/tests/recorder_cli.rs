#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
//! End to end: hooks in, one trajectory out, and a spool that survives being interrupted.

use std::path::Path;
use std::process::Command as StdCommand;

use assert_cmd::Command;
use predicates::str::contains;
use serde_json::Value;

const SESSION: &str = "aaaaaaaa-1111-2222-3333-444455556666";

fn bin(home: &Path) -> Command {
    let mut command = Command::cargo_bin("agent-jit").unwrap();
    command.env("AGENT_JIT_HOME", home);
    command
}

fn json(output: &[u8]) -> Value {
    serde_json::from_slice(output).expect("stdout is JSON")
}

fn init_repo(root: &Path) {
    for argv in [
        vec!["init", "--initial-branch=main"],
        vec!["config", "user.name", "test"],
        vec!["config", "user.email", "test@example.com"],
    ] {
        assert!(
            StdCommand::new("git")
                .args(&argv)
                .current_dir(root)
                .status()
                .unwrap()
                .success()
        );
    }
    std::fs::write(root.join("README.md"), "seed\n").unwrap();
    for argv in [
        vec!["add", "README.md"],
        vec!["commit", "-m", "seed", "--no-gpg-sign"],
    ] {
        assert!(
            StdCommand::new("git")
                .args(&argv)
                .current_dir(root)
                .status()
                .unwrap()
                .success()
        );
    }
}

fn payload(kind: &str, repository: &Path, extra: &Value) -> String {
    let mut document = serde_json::json!({
        "session_id": SESSION,
        "cwd": repository.to_string_lossy(),
        "hook_event_name": kind,
        "transcript_path": "/tmp/does-not-exist.jsonl",
    });
    if let (Some(object), Some(extra)) = (document.as_object_mut(), extra.as_object()) {
        for (key, value) in extra {
            object.insert(key.clone(), value.clone());
        }
    }
    document.to_string()
}

fn ingest(home: &Path, event: &str, body: &str) {
    bin(home)
        .args([
            "hook",
            "ingest",
            "--event",
            event,
            "--claude-version",
            "2.0.0",
        ])
        .write_stdin(body.to_owned())
        .assert()
        .success()
        .stdout(predicates::str::is_empty());
}

/// Feeds a complete two-turn session through the hook command.
fn record_session(home: &Path, repository: &Path) {
    ingest(
        home,
        "session-start",
        &payload(
            "SessionStart",
            repository,
            &serde_json::json!({"source": "startup"}),
        ),
    );
    ingest(
        home,
        "user-prompt-submit",
        &payload(
            "UserPromptSubmit",
            repository,
            &serde_json::json!({"prompt": "validate this change"}),
        ),
    );
    ingest(
        home,
        "pre-tool-use",
        &payload(
            "PreToolUse",
            repository,
            &serde_json::json!({"tool_name": "Bash", "tool_input": {"command": "cargo test"}}),
        ),
    );
    ingest(
        home,
        "post-tool-use",
        &payload(
            "PostToolUse",
            repository,
            &serde_json::json!({
                "tool_name": "Bash",
                "tool_input": {"command": "cargo test"},
                "tool_response": {"stdout": "ok"}
            }),
        ),
    );
    ingest(
        home,
        "stop",
        &payload(
            "Stop",
            repository,
            &serde_json::json!({"stop_hook_active": false}),
        ),
    );
    ingest(
        home,
        "session-end",
        &payload(
            "SessionEnd",
            repository,
            &serde_json::json!({"reason": "exit"}),
        ),
    );
}

#[test]
fn a_recorded_session_becomes_one_inspectable_trajectory() {
    let home = tempfile::tempdir().unwrap();
    let repository = tempfile::tempdir().unwrap();
    init_repo(repository.path());

    bin(home.path())
        .args(["store", "migrate", "--json"])
        .assert()
        .success();
    record_session(home.path(), repository.path());

    let recovered = bin(home.path())
        .args(["store", "recover", "--json"])
        .assert()
        .success();
    let report = json(&recovered.get_output().stdout);
    assert_eq!(report["recovered"].as_array().unwrap().len(), 1, "{report}");
    assert_eq!(report["pending"].as_array().unwrap().len(), 0);
    assert_eq!(report["quarantined_segments"], 0);

    // The spool is empty once its contents are safely in the database.
    let segments_root = home.path().join("cache/spool/segments").join(SESSION);
    assert!(!segments_root.exists(), "spool must be drained");

    let trajectory_id = report["recovered"][0]["trajectory_id"].as_str().unwrap();
    let shown = bin(home.path())
        .args(["trace", "show", trajectory_id, "--json"])
        .assert()
        .success();
    let trace = json(&shown.get_output().stdout);

    assert_eq!(trace["intent"], "validate this change");
    assert_eq!(trace["outcome"], "unannotated");
    assert_eq!(trace["events"].as_array().unwrap().len(), 6);
    // Tools appear in the order they ran.
    let tools: Vec<&str> = trace["events"]
        .as_array()
        .unwrap()
        .iter()
        .filter_map(|event| event["tool_name"].as_str())
        .collect();
    assert_eq!(tools, vec!["Bash", "Bash"]);

    let listed = bin(home.path())
        .args([
            "trace",
            "list",
            "--repo",
            repository.path().to_str().unwrap(),
            "--json",
        ])
        .assert()
        .success();
    assert_eq!(json(&listed.get_output().stdout)["count"], 1);
}

#[test]
fn recovery_is_idempotent_and_leaves_one_trajectory() {
    let home = tempfile::tempdir().unwrap();
    let repository = tempfile::tempdir().unwrap();
    init_repo(repository.path());

    bin(home.path())
        .args(["store", "migrate", "--json"])
        .assert()
        .success();
    record_session(home.path(), repository.path());

    bin(home.path())
        .args(["store", "recover", "--json"])
        .assert()
        .success();
    let second = bin(home.path())
        .args(["store", "recover", "--json"])
        .assert()
        .success();
    assert_eq!(
        json(&second.get_output().stdout)["recovered"]
            .as_array()
            .unwrap()
            .len(),
        0,
        "a drained spool has nothing left to recover"
    );

    let listed = bin(home.path())
        .args([
            "trace",
            "list",
            "--repo",
            repository.path().to_str().unwrap(),
            "--json",
        ])
        .assert()
        .success();
    assert_eq!(json(&listed.get_output().stdout)["count"], 1);
}

#[test]
fn an_open_session_stays_in_the_spool_and_is_reported_as_pending() {
    let home = tempfile::tempdir().unwrap();
    let repository = tempfile::tempdir().unwrap();
    init_repo(repository.path());

    bin(home.path())
        .args(["store", "migrate", "--json"])
        .assert()
        .success();
    ingest(
        home.path(),
        "user-prompt-submit",
        &payload(
            "UserPromptSubmit",
            repository.path(),
            &serde_json::json!({"prompt": "still working"}),
        ),
    );

    let recovered = bin(home.path())
        .args(["store", "recover", "--json"])
        .assert()
        .success();
    let report = json(&recovered.get_output().stdout);
    assert_eq!(report["recovered"].as_array().unwrap().len(), 0);
    assert_eq!(report["pending"].as_array().unwrap().len(), 1);
    assert!(
        home.path()
            .join("cache/spool/segments")
            .join(SESSION)
            .exists(),
        "an open session keeps its spool"
    );
}

#[test]
fn a_corrupt_tail_is_quarantined_and_the_rest_still_recovers() {
    let home = tempfile::tempdir().unwrap();
    let repository = tempfile::tempdir().unwrap();
    init_repo(repository.path());

    bin(home.path())
        .args(["store", "migrate", "--json"])
        .assert()
        .success();
    record_session(home.path(), repository.path());

    // Simulate a process killed mid-write: one segment on disk is a fragment.
    let session_directory = home.path().join("cache/spool/segments").join(SESSION);
    let mut files: Vec<_> = std::fs::read_dir(&session_directory)
        .unwrap()
        .flatten()
        .map(|entry| entry.path())
        .collect();
    files.sort();
    std::fs::write(files.last().unwrap(), b"{\"segment_version\":1,\"pay").unwrap();
    // ... and a duplicate of an earlier record is left behind by a retried hook.
    std::fs::copy(
        &files[0],
        session_directory.join("00000000000000000001-dup.json"),
    )
    .unwrap();

    let recovered = bin(home.path())
        .args(["store", "recover", "--json"])
        .assert()
        .success();
    let report = json(&recovered.get_output().stdout);

    // SessionEnd was the corrupted record, so the session is correctly reported as still open
    // rather than finalized from partial evidence.
    assert_eq!(report["quarantined_segments"], 1, "{report}");
    assert_eq!(report["pending"].as_array().unwrap().len(), 1, "{report}");

    let quarantine = home.path().join("cache/spool/segments/quarantine");
    assert!(quarantine.exists());
    let quarantined: Vec<_> = std::fs::read_dir(&quarantine).unwrap().flatten().collect();
    assert_eq!(quarantined.len(), 1);
    assert!(
        quarantined[0]
            .file_name()
            .to_string_lossy()
            .starts_with("segment_malformed"),
        "{:?}",
        quarantined[0].file_name()
    );
}

#[test]
fn showing_an_unknown_trajectory_is_a_usage_error() {
    let home = tempfile::tempdir().unwrap();
    bin(home.path())
        .args(["store", "migrate", "--json"])
        .assert()
        .success();

    bin(home.path())
        .args(["trace", "show", "trj_01J0000000000000000000000A", "--json"])
        .assert()
        .code(2)
        .stdout(contains("trace_not_found"));
}
