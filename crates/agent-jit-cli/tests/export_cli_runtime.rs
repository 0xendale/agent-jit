#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
//! Runtime-red corpus export command contract.

mod corpus_support;

use corpus_support::{bin, init_repo, json, record};

#[test]
fn given_recorded_trace_when_full_export_runs_then_manifest_and_records_are_reported() {
    // Given
    let home = tempfile::tempdir().unwrap();
    let repo = tempfile::tempdir().unwrap();
    init_repo(repo.path());
    record(home.path(), repo.path());
    let output_dir = home.path().join("export");

    // When
    let output = bin(home.path())
        .args([
            "corpus",
            "export",
            "--repo",
            repo.path().to_str().unwrap(),
            "--output",
            output_dir.to_str().unwrap(),
            "--mode",
            "full-redacted",
            "--json",
        ])
        .assert()
        .success();

    // Then
    let report = json(&output.get_output().stdout);
    assert!(report["manifest_digest"].is_string());
    assert!(report["manifest_file"].is_string());
    assert!(!report["record_files"].as_array().unwrap().is_empty());
}
