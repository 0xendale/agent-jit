#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
//! Negative recorder health checks at process boundaries.

mod corpus_support;

use corpus_support::{bin, json};

#[test]
fn doctor_reports_integrity_failure_when_plugin_content_changes() {
    // Given: an owned plugin whose hook registration has been replaced.
    let home = tempfile::tempdir().unwrap();
    let installed = bin(home.path())
        .args(["claude", "materialize", "--json"])
        .assert()
        .success();
    let report = json(&installed.get_output().stdout);
    let path =
        std::path::Path::new(report["plugin_dir"].as_str().unwrap()).join("hooks/hooks.json");
    std::fs::write(&path, b"{}").unwrap();

    // When: inspecting recorder health without running Claude.
    let output = bin(home.path())
        .env("PATH", "")
        .args(["doctor", "--scope", "recorder", "--json"])
        .assert()
        .failure();

    // Then: integrity failure is identified without repairing the modified file.
    assert_eq!(
        json(&output.get_output().stdout)["error"]["details"]["checks"]["plugin"]["code"],
        "plugin_integrity_failed"
    );
    assert_eq!(std::fs::read(&path).unwrap(), b"{}");
}

#[test]
fn doctor_reports_recorder_loss_when_health_spool_contains_a_fault() {
    // Given: a malformed hook that failed open and recorded its fault.
    let home = tempfile::tempdir().unwrap();
    bin(home.path())
        .args(["hook", "ingest", "--event", "session-start"])
        .write_stdin("{")
        .assert()
        .success()
        .stdout(predicates::str::is_empty());

    // When: checking health after the fault, even if a new probe could work.
    let output = bin(home.path())
        .env("PATH", "")
        .args(["doctor", "--scope", "recorder", "--json"])
        .assert()
        .failure();

    // Then: the retained recorder fault prevents a healthy report.
    assert_eq!(
        json(&output.get_output().stdout)["error"]["details"]["checks"]["hook_round_trip"]["code"],
        "recorder_health_fault"
    );
}
