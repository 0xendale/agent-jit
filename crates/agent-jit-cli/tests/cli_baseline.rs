#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
//! Baseline process-level contracts for the `agent-jit` binary.

use assert_cmd::Command;
use predicates::str::contains;

fn bin() -> Command {
    Command::cargo_bin("agent-jit").unwrap()
}

#[test]
fn version_flag_reports_crate_version() {
    bin()
        .arg("--version")
        .assert()
        .success()
        .stdout(contains(concat!("agent-jit ", env!("CARGO_PKG_VERSION"))));
}

#[test]
fn unknown_command_fails_with_typed_message_on_stderr() {
    bin()
        .arg("definitely-not-a-command")
        .assert()
        .failure()
        .stderr(contains("unknown command"));
}

#[test]
fn no_arguments_prints_usage_and_fails() {
    bin()
        .assert()
        .failure()
        .stderr(contains("usage: agent-jit"));
}
