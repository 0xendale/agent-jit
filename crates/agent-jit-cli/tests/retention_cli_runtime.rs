#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
//! Runtime-red corpus retention command contract.

mod corpus_support;

use corpus_support::{bin, init_repo, json, record};

#[test]
fn given_recorded_trace_when_prune_has_no_apply_flag_then_defaults_are_non_mutating() {
    // Given
    let home = tempfile::tempdir().unwrap();
    let repo = tempfile::tempdir().unwrap();
    init_repo(repo.path());
    record(home.path(), repo.path());

    // When
    let output = bin(home.path())
        .args(["corpus", "prune", "--json"])
        .assert()
        .success();

    // Then
    let report = json(&output.get_output().stdout);
    assert_eq!(report["dry_run"], true);
    assert_eq!(report["max_age_days"], 30);
    assert_eq!(report["max_bytes"], 1_073_741_824_u64);
    assert!(report["deleted_sessions"].as_array().unwrap().is_empty());
}
