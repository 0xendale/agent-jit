#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
//! `agent-jit schema` is the boundary where stored documents are accepted or refused.

use assert_cmd::Command;
use predicates::str::contains;

fn bin() -> Command {
    Command::cargo_bin("agent-jit").unwrap()
}

fn fixture(name: &str) -> String {
    format!(
        "{}/tests/fixtures/schema/{name}",
        env!("CARGO_MANIFEST_DIR")
    )
}

#[test]
fn validating_a_well_formed_record_reports_its_schema_and_digest() {
    bin()
        .args(["schema", "validate", &fixture("valid-trajectory.json")])
        .assert()
        .success()
        .stdout(contains("agent_jit.trajectory v1"))
        .stdout(contains("trj_01J0000000000000000000000A"));
}

#[test]
fn validating_an_unknown_version_fails_with_a_stable_code_and_no_stdout() {
    bin()
        .args(["schema", "validate", &fixture("unknown-version.json")])
        .assert()
        .failure()
        .stdout(predicates::str::is_empty())
        .stderr(contains("schema_unsupported"));
}

#[test]
fn validating_a_float_metric_fails_with_a_stable_code() {
    bin()
        .args(["schema", "validate", &fixture("float-metric.json")])
        .assert()
        .failure()
        .stdout(predicates::str::is_empty())
        .stderr(contains("schema_invalid_record"));
}

#[test]
fn validating_a_missing_file_fails_without_a_partial_record() {
    bin()
        .args(["schema", "validate", &fixture("does-not-exist.json")])
        .assert()
        .failure()
        .stdout(predicates::str::is_empty())
        .stderr(contains("schema_unreadable"));
}

#[test]
fn schema_validate_without_a_path_is_a_usage_error() {
    bin()
        .args(["schema", "validate"])
        .assert()
        .code(2)
        .stderr(contains("usage: agent-jit schema validate"));
}

#[test]
fn schema_generate_writes_every_contract_to_the_requested_directory() {
    let out = tempfile::tempdir().unwrap();
    bin()
        .args(["schema", "generate", "--out", out.path().to_str().unwrap()])
        .assert()
        .success()
        .stdout(contains("13 schemas"));

    let written = std::fs::read_dir(out.path()).unwrap().count();
    assert_eq!(written, 13);
}

#[test]
fn schema_generate_is_idempotent() {
    let out = tempfile::tempdir().unwrap();
    let mut first = bin();
    first
        .args(["schema", "generate", "--out", out.path().to_str().unwrap()])
        .assert()
        .success();
    let before =
        std::fs::read_to_string(out.path().join("agent_jit_trajectory.schema.json")).unwrap();

    bin()
        .args(["schema", "generate", "--out", out.path().to_str().unwrap()])
        .assert()
        .success();
    let after =
        std::fs::read_to_string(out.path().join("agent_jit_trajectory.schema.json")).unwrap();

    assert_eq!(before, after);
}
