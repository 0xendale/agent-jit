#![allow(dead_code)]

use std::path::Path;
use std::process::Command as StdCommand;

use agent_jit_domain::ids::{RepositoryId, TrajectoryId};
use assert_cmd::Command;
use serde_json::Value;

pub const SESSION: &str = "bbbbbbbb-1111-2222-3333-444455556666";
pub const CANARY: &str = "sk-ant-api03-AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA";

pub fn bin(home: &Path) -> Command {
    let mut command = Command::cargo_bin("agent-jit").unwrap();
    command.env("AGENT_JIT_HOME", home);
    command.env("LC_ALL", "C");
    command.env("TZ", "UTC");
    command
}

pub fn json(bytes: &[u8]) -> Value {
    serde_json::from_slice(bytes).expect("stdout is JSON")
}

pub fn init_repo(root: &Path) {
    for argv in [
        ["init", "--initial-branch=main"].as_slice(),
        ["config", "user.name", "test"].as_slice(),
        ["config", "user.email", "test@example.com"].as_slice(),
    ] {
        assert!(
            StdCommand::new("git")
                .args(argv)
                .current_dir(root)
                .status()
                .unwrap()
                .success()
        );
    }
    std::fs::write(root.join("README.md"), "seed\n").unwrap();
    for argv in [
        ["add", "README.md"].as_slice(),
        ["commit", "-m", "seed", "--no-gpg-sign"].as_slice(),
    ] {
        assert!(
            StdCommand::new("git")
                .args(argv)
                .current_dir(root)
                .status()
                .unwrap()
                .success()
        );
    }
}

fn payload(kind: &str, repository: &Path, extra: &Value) -> String {
    let mut value = serde_json::json!({
        "session_id": SESSION,
        "cwd": repository,
        "hook_event_name": kind,
        "transcript_path": repository.join("private-transcript.jsonl"),
    });
    value
        .as_object_mut()
        .unwrap()
        .extend(extra.as_object().unwrap().clone());
    value.to_string()
}

fn ingest(home: &Path, event: &str, payload: String) {
    bin(home)
        .args([
            "hook",
            "ingest",
            "--event",
            event,
            "--claude-version",
            "2.0.0",
        ])
        .write_stdin(payload)
        .assert()
        .success();
}

pub fn record(home: &Path, repository: &Path) -> (TrajectoryId, RepositoryId) {
    bin(home)
        .args(["store", "migrate", "--json"])
        .assert()
        .success();
    ingest(
        home,
        "session-start",
        payload(
            "SessionStart",
            repository,
            &serde_json::json!({"source": "startup"}),
        ),
    );
    ingest(
        home,
        "user-prompt-submit",
        payload(
            "UserPromptSubmit",
            repository,
            &serde_json::json!({"prompt": format!("validate safely {CANARY}")}),
        ),
    );
    ingest(
        home,
        "stop",
        payload(
            "Stop",
            repository,
            &serde_json::json!({"stop_hook_active": false}),
        ),
    );
    ingest(
        home,
        "session-end",
        payload(
            "SessionEnd",
            repository,
            &serde_json::json!({"reason": "exit"}),
        ),
    );
    let recovered = bin(home)
        .args(["store", "recover", "--json"])
        .assert()
        .success();
    let trajectory: TrajectoryId =
        json(&recovered.get_output().stdout)["recovered"][0]["trajectory_id"]
            .as_str()
            .unwrap()
            .parse()
            .unwrap();
    let shown = bin(home)
        .args(["trace", "show", &trajectory.to_string(), "--json"])
        .assert()
        .success();
    let repository_id = json(&shown.get_output().stdout)["repository_id"]
        .as_str()
        .unwrap()
        .parse()
        .unwrap();
    (trajectory, repository_id)
}
