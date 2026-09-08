#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
//! Recorder health is checked through real subprocess and filesystem boundaries.

use assert_cmd::Command;
use serde_json::Value;
use std::os::unix::fs::PermissionsExt as _;

mod corpus_support;

#[test]
fn recorder_reports_failed_checks_when_executables_are_unavailable() {
    // Given: private empty state and no executable search path.
    let home = tempfile::tempdir().unwrap();
    let mut command = Command::cargo_bin("agent-jit").unwrap();
    command
        .env("AGENT_JIT_HOME", home.path())
        .env("PATH", "")
        .env("LC_ALL", "C")
        .env("TZ", "UTC");

    // When: checking the recorder through its public process boundary.
    let result = command
        .args(["doctor", "--scope", "recorder", "--json"])
        .assert()
        .failure();

    // Then: unavailable prerequisites are health failures, not usage errors.
    let report: Value = serde_json::from_slice(&result.get_output().stdout).unwrap();
    assert_eq!(report["error"]["code"], "recorder_unhealthy");
    assert_eq!(report["error"]["details"]["scope"], "recorder");
    assert_eq!(
        report["error"]["details"]["checks"]["git"]["status"],
        "fail"
    );
    assert_eq!(
        report["error"]["details"]["checks"]["claude"]["status"],
        "fail"
    );
    assert_eq!(
        report["error"]["details"]["checks"]["opencode"]["status"],
        "fail"
    );
}

#[test]
fn recorder_passes_when_local_prerequisites_and_hook_round_trip_work() {
    // Given: migrated state, generated plugin, and deterministic version executables.
    let home = tempfile::tempdir().unwrap();
    let tools = tempfile::tempdir().unwrap();
    let claude = tools.path().join("claude");
    std::fs::write(&claude, "#!/bin/sh\nprintf '2.1.263 (Claude Code)\\n'\n").unwrap();
    let opencode = tools.path().join("opencode");
    std::fs::write(&opencode, "#!/bin/sh\nprintf '1.18.29\\n'\n").unwrap();
    for executable in [&claude, &opencode] {
        std::fs::set_permissions(executable, std::fs::Permissions::from_mode(0o700)).unwrap();
    }
    corpus_support::bin(home.path())
        .args(["store", "migrate", "--json"])
        .assert()
        .success();
    corpus_support::bin(home.path())
        .args(["claude", "materialize", "--json"])
        .assert()
        .success();

    // When: all recorder checks run against local process and disk boundaries.
    let result = corpus_support::bin(home.path())
        .env("PATH", format!("{}:/usr/bin:/bin", tools.path().display()))
        .args(["doctor", "--scope", "recorder", "--json"])
        .assert()
        .success();

    // Then: every check passes, with no diagnostic trajectory added.
    let report = corpus_support::json(&result.get_output().stdout);
    assert_eq!(report["status"], "pass");
    assert_eq!(report["checks"].as_object().unwrap().len(), 8);
    assert_eq!(report["checks"]["claude"]["details"]["version"], "2.1.263");
    assert_eq!(
        report["checks"]["opencode"]["details"]["version"],
        "1.18.29"
    );
    assert_eq!(
        report["checks"]["hook_round_trip"]["details"]["trajectory_created"],
        false
    );
    assert_eq!(
        std::fs::read_dir(home.path().join("cache/spool"))
            .unwrap()
            .count(),
        0
    );
}

#[test]
fn launch_propagates_observed_runtime_version_to_hook_segments() {
    // Given: a runtime that invokes the real hook command in its child environment.
    let home = tempfile::tempdir().unwrap();
    let repository = tempfile::tempdir().unwrap();
    corpus_support::init_repo(repository.path());
    let runtime = tempfile::tempdir().unwrap();
    let executable = runtime.path().join("claude");
    std::fs::write(&executable, r#"#!/bin/sh
if [ "$1" = '--version' ]; then printf '2.1.263 (Claude Code)\n'; exit 0; fi
printf '{"session_id":"runtime-probe","cwd":"/tmp","hook_event_name":"SessionStart"}' | "$JIT_TEST_BINARY" hook ingest --event session-start
"#).unwrap();
    std::fs::set_permissions(&executable, std::fs::Permissions::from_mode(0o700)).unwrap();

    // When: the launcher invokes the selected runtime, not another executable on PATH.
    corpus_support::bin(home.path())
        .env("JIT_TEST_BINARY", assert_cmd::cargo::cargo_bin("agent-jit"))
        .args([
            "claude",
            "run",
            "--repo",
            repository.path().to_str().unwrap(),
            "--claude-bin",
            executable.to_str().unwrap(),
            "--",
            "-p",
            "fixture",
        ])
        .assert()
        .success()
        .stdout(predicates::str::is_empty());

    // Then: persisted hook provenance has the selected runtime's exact semver.
    let segments =
        agent_jit_engine::recorder::SegmentStore::open(&home.path().join("cache/spool/segments"))
            .unwrap();
    let read = segments.read_session("runtime-probe").unwrap();
    assert_eq!(read.accepted.len(), 1);
    assert_eq!(
        read.accepted[0].payload.claude_version.as_deref(),
        Some("2.1.263")
    );
}

#[test]
fn hook_remains_silent_and_successful_when_private_home_is_unusable() {
    // Given: a file where the private directory must be.
    let root = tempfile::tempdir().unwrap();
    let home = root.path().join("blocked");
    std::fs::write(&home, b"not a directory").unwrap();

    // When: a valid hook arrives during a recorder storage failure.
    let result = Command::cargo_bin("agent-jit")
        .unwrap()
        .env("AGENT_JIT_HOME", &home)
        .args(["hook", "ingest", "--event", "session-start"])
        .write_stdin(r#"{"session_id":"probe","cwd":"/tmp","hook_event_name":"SessionStart"}"#)
        .assert();

    // Then: user work continues without injected context.
    result.success().stdout(predicates::str::is_empty());
}
