#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
//! Export destination confinement and staging cleanup regressions.

mod corpus_support;

use std::os::unix::fs::symlink;

use corpus_support::{bin, init_repo, json, record};

#[test]
fn given_repository_destination_when_export_runs_then_it_is_refused_without_writes() {
    // Given
    let home = tempfile::tempdir().unwrap();
    let repo = tempfile::tempdir().unwrap();
    init_repo(repo.path());
    record(home.path(), repo.path());
    let output = repo.path().join("export");

    // When
    let result = bin(home.path())
        .args([
            "corpus",
            "export",
            "--repo",
            repo.path().to_str().unwrap(),
            "--output",
            output.to_str().unwrap(),
            "--mode",
            "metadata-only",
            "--json",
        ])
        .assert()
        .code(4);

    // Then
    assert_eq!(
        json(&result.get_output().stdout)["error"]["code"],
        "export_path_in_repository"
    );
    assert!(!output.exists());
}

#[test]
fn given_symlink_parent_into_repository_when_export_runs_then_alias_is_refused() {
    // Given
    let home = tempfile::tempdir().unwrap();
    let repo = tempfile::tempdir().unwrap();
    let outside = tempfile::tempdir().unwrap();
    init_repo(repo.path());
    record(home.path(), repo.path());
    symlink(repo.path(), outside.path().join("alias")).unwrap();
    let output = outside.path().join("alias/export");

    // When
    let result = bin(home.path())
        .args([
            "corpus",
            "export",
            "--repo",
            repo.path().to_str().unwrap(),
            "--output",
            output.to_str().unwrap(),
            "--mode",
            "metadata-only",
            "--json",
        ])
        .assert()
        .code(4);

    // Then
    assert_eq!(
        json(&result.get_output().stdout)["error"]["code"],
        "export_path_in_repository"
    );
    assert!(!repo.path().join("export").exists());
}

#[test]
fn given_dot_dot_destination_resolving_into_repository_when_export_runs_then_it_is_refused() {
    // Given
    let home = tempfile::tempdir().unwrap();
    let repo = tempfile::tempdir().unwrap();
    init_repo(repo.path());
    record(home.path(), repo.path());
    std::fs::create_dir(repo.path().join("child")).unwrap();
    let output = repo.path().join("child/../export");

    // When
    let result = bin(home.path())
        .args([
            "corpus",
            "export",
            "--repo",
            repo.path().to_str().unwrap(),
            "--output",
            output.to_str().unwrap(),
            "--mode",
            "metadata-only",
            "--json",
        ])
        .assert()
        .code(4);

    // Then
    assert_eq!(
        json(&result.get_output().stdout)["error"]["code"],
        "export_path_in_repository"
    );
    assert!(!repo.path().join("export").exists());
}
