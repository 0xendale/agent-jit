#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
//! The command tree, the private paths, and the error shape are contracts of their own.

use std::os::unix::fs::PermissionsExt as _;
use std::path::Path;
use std::process::Command as StdCommand;

use assert_cmd::Command;
use predicates::str::contains;
use serde_json::Value;

/// Every command the binary reserves. Unfinished ones must say so, not pretend to work.
const RESERVED: &[&str] = &[
    "paths",
    "doctor",
    "schema",
    "store",
    "repo",
    "hook",
    "trace",
    "corpus",
    "group",
    "candidate",
    "phase0",
    "profile",
    "capability",
    "replay",
    "sandbox",
    "mcp",
    "claude",
    "benchmark",
    "install",
    "uninstall",
];

/// Commands that already do something; the rest must report `not_implemented`.
const IMPLEMENTED: &[&str] = &["paths", "doctor", "schema", "repo"];

fn bin(home: &Path) -> Command {
    let mut command = Command::cargo_bin("agent-jit").unwrap();
    command.env("AGENT_JIT_HOME", home);
    command
}

fn json_stdout(output: &[u8]) -> Value {
    serde_json::from_slice(output).expect("stdout is JSON")
}

#[test]
fn paths_reports_private_locations_and_creates_them_privately() {
    let home = tempfile::tempdir().unwrap();
    let assert = bin(home.path())
        .args(["paths", "--json"])
        .assert()
        .success();
    let report = json_stdout(&assert.get_output().stdout);

    for key in ["home", "data", "cache", "state_db", "spool", "logs"] {
        let path = report[key]
            .as_str()
            .unwrap_or_else(|| panic!("{key} missing: {report}"));
        assert!(Path::new(path).is_absolute(), "{key} must be absolute");
        assert!(
            path.starts_with(home.path().to_str().unwrap()),
            "{key} must live under AGENT_JIT_HOME: {path}"
        );
    }

    for key in ["data", "cache", "spool", "logs"] {
        let path = report[key].as_str().unwrap();
        let mode = std::fs::metadata(path).unwrap().permissions().mode() & 0o777;
        assert_eq!(mode, 0o700, "{key} must be 0700, was {mode:o}");
    }
}

#[test]
fn paths_json_output_contains_no_ansi_escapes() {
    let home = tempfile::tempdir().unwrap();
    let assert = bin(home.path())
        .args(["paths", "--json"])
        .assert()
        .success();
    let text = String::from_utf8(assert.get_output().stdout.clone()).unwrap();
    assert!(
        !text.contains('\u{1b}'),
        "JSON output must not contain ANSI"
    );
}

#[test]
fn doctor_reports_host_and_store_status_as_json() {
    let home = tempfile::tempdir().unwrap();
    let assert = bin(home.path())
        .args(["doctor", "--json"])
        .assert()
        .success();
    let report = json_stdout(&assert.get_output().stdout);

    assert_eq!(report["host"]["os"], "macos");
    assert_eq!(report["host"]["arch"], "aarch64");
    assert_eq!(report["host"]["supported"], true);
    assert!(report["paths"]["home"].is_string());
    assert!(report["version"].is_string());
}

#[test]
fn every_reserved_command_is_known_and_documented() {
    let home = tempfile::tempdir().unwrap();
    let assert = bin(home.path()).arg("--help").assert().success();
    let help = String::from_utf8(assert.get_output().stdout.clone()).unwrap();

    for command in RESERVED {
        assert!(help.contains(command), "help does not mention `{command}`");
    }
}

#[test]
fn unfinished_commands_report_not_implemented_rather_than_succeeding() {
    let home = tempfile::tempdir().unwrap();
    for command in RESERVED {
        if IMPLEMENTED.contains(command) {
            continue;
        }
        bin(home.path())
            .args([command, "--json"])
            .assert()
            .code(3)
            .stdout(contains("not_implemented"));
    }
}

#[test]
fn an_unknown_command_uses_the_usage_exit_class() {
    let home = tempfile::tempdir().unwrap();
    bin(home.path())
        .arg("not-a-command")
        .assert()
        .code(2)
        .stderr(contains("unknown command"));
}

#[test]
fn errors_in_json_mode_are_structured_on_stdout_with_diagnostics_on_stderr() {
    let home = tempfile::tempdir().unwrap();
    let assert = bin(home.path())
        .args([
            "repo",
            "inspect",
            "--root",
            home.path().to_str().unwrap(),
            "--json",
        ])
        .assert()
        .failure();

    let report = json_stdout(&assert.get_output().stdout);
    assert_eq!(report["error"]["code"], "repo_not_a_repository");
    assert_eq!(report["error"]["retryable"], false);
    assert!(report["error"]["message"].is_string());

    let stderr = String::from_utf8(assert.get_output().stderr.clone()).unwrap();
    assert!(stderr.contains("repo_not_a_repository"), "{stderr}");
    assert!(!stderr.contains('\u{1b}'));
}

#[test]
fn a_home_inside_a_git_repository_is_refused_before_anything_is_created() {
    let repo = tempfile::tempdir().unwrap();
    assert!(
        StdCommand::new("git")
            .args(["init", "--initial-branch=main"])
            .current_dir(repo.path())
            .status()
            .unwrap()
            .success()
    );

    let inside = repo.path().join("agent-jit-state");
    bin(&inside)
        .args(["paths", "--json"])
        .assert()
        .code(4)
        .stdout(contains("home_inside_repository"));

    assert!(
        !inside.exists(),
        "nothing may be created inside an observed repository"
    );
}

#[test]
fn a_symlinked_home_is_refused() {
    let temp = tempfile::tempdir().unwrap();
    let real = temp.path().join("real");
    std::fs::create_dir_all(&real).unwrap();
    let link = temp.path().join("link");
    std::os::unix::fs::symlink(&real, &link).unwrap();

    bin(&link)
        .args(["paths", "--json"])
        .assert()
        .code(4)
        .stdout(contains("home_symlink"));
}

#[test]
fn malformed_arguments_never_panic() {
    let home = tempfile::tempdir().unwrap();
    let hostile = [
        vec!["repo", "inspect", "--root"],
        vec!["repo", "inspect", "--root", ""],
        vec!["schema", "validate", "--", "--"],
        vec!["--json"],
        vec!["paths", "--unknown-flag"],
        vec!["schema"],
        vec![""],
    ];

    for args in hostile {
        let assert = bin(home.path()).args(&args).assert();
        let output = assert.get_output();
        let stderr = String::from_utf8(output.stderr.clone()).unwrap();
        assert!(
            !stderr.contains("panicked"),
            "panicked on {args:?}: {stderr}"
        );
        assert!(
            output.status.code().is_some(),
            "process died by signal on {args:?}"
        );
    }
}
