#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
//! Repository identity must survive worktrees, and refuse anything that is not a repository.

use std::path::Path;
use std::process::Command;

use agent_jit_engine::process::ProcessRunner;
use agent_jit_engine::repository::{RepositoryError, discover};

/// Creates a repository with one commit and returns its path.
fn init_repo(root: &Path) {
    for argv in [
        vec!["init", "--initial-branch=main"],
        vec!["config", "user.name", "test"],
        vec!["config", "user.email", "test@example.com"],
    ] {
        let status = Command::new("git")
            .args(&argv)
            .current_dir(root)
            .status()
            .unwrap();
        assert!(status.success(), "git {argv:?} failed");
    }
    std::fs::write(root.join("README.md"), "seed\n").unwrap();
    for argv in [
        vec!["add", "README.md"],
        vec!["commit", "-m", "seed", "--no-gpg-sign"],
    ] {
        let status = Command::new("git")
            .args(&argv)
            .current_dir(root)
            .status()
            .unwrap();
        assert!(status.success(), "git {argv:?} failed");
    }
}

#[test]
fn discovery_reports_worktree_root_head_and_a_stable_identity() {
    let temp = tempfile::tempdir().unwrap();
    init_repo(temp.path());

    let runner = ProcessRunner::new();
    let first = discover(&runner, temp.path()).unwrap();
    let second = discover(&runner, temp.path()).unwrap();

    assert_eq!(first.repo_id, second.repo_id);
    assert_eq!(first.head_commit.len(), 40);
    assert!(first.head_commit.chars().all(|c| c.is_ascii_hexdigit()));
    assert_eq!(
        first.worktree_root.canonicalize().unwrap(),
        temp.path().canonicalize().unwrap()
    );
}

#[test]
fn discovery_from_a_subdirectory_finds_the_same_worktree() {
    let temp = tempfile::tempdir().unwrap();
    init_repo(temp.path());
    let nested = temp.path().join("src/deep");
    std::fs::create_dir_all(&nested).unwrap();

    let runner = ProcessRunner::new();
    let from_root = discover(&runner, temp.path()).unwrap();
    let from_nested = discover(&runner, &nested).unwrap();

    assert_eq!(from_root.repo_id, from_nested.repo_id);
    assert_eq!(from_root.worktree_root, from_nested.worktree_root);
}

#[test]
fn two_worktrees_of_one_repository_share_an_identity_but_not_a_worktree() {
    let temp = tempfile::tempdir().unwrap();
    let main = temp.path().join("main");
    std::fs::create_dir_all(&main).unwrap();
    init_repo(&main);

    let linked = temp.path().join("linked");
    let status = Command::new("git")
        .args(["worktree", "add", "-b", "side"])
        .arg(&linked)
        .current_dir(&main)
        .status()
        .unwrap();
    assert!(status.success(), "git worktree add failed");

    let runner = ProcessRunner::new();
    let primary = discover(&runner, &main).unwrap();
    let secondary = discover(&runner, &linked).unwrap();

    assert_eq!(
        primary.repo_id, secondary.repo_id,
        "worktrees of one repository must share an identity"
    );
    assert_ne!(primary.worktree_root, secondary.worktree_root);
    assert_eq!(primary.git_common_dir, secondary.git_common_dir);
}

#[test]
fn a_directory_that_is_not_a_repository_is_refused() {
    let temp = tempfile::tempdir().unwrap();
    let runner = ProcessRunner::new();
    let error = discover(&runner, temp.path()).unwrap_err();
    assert_eq!(error.code(), "repo_not_a_repository");
}

#[test]
fn a_missing_root_is_refused() {
    let temp = tempfile::tempdir().unwrap();
    let runner = ProcessRunner::new();
    let error = discover(&runner, &temp.path().join("nope")).unwrap_err();
    assert_eq!(error.code(), "repo_root_unreadable");
}

#[test]
fn a_relative_traversal_in_the_root_is_refused() {
    let temp = tempfile::tempdir().unwrap();
    init_repo(temp.path());
    let runner = ProcessRunner::new();
    let error = discover(&runner, &temp.path().join("../..")).unwrap_err();
    assert!(
        matches!(error, RepositoryError::Traversal { .. }),
        "{error:?}"
    );
    assert_eq!(error.code(), "repo_path_traversal");
}

#[test]
fn a_symlinked_root_is_refused_because_it_can_be_repointed() {
    let temp = tempfile::tempdir().unwrap();
    let real = temp.path().join("real");
    std::fs::create_dir_all(&real).unwrap();
    init_repo(&real);

    let link = temp.path().join("link");
    std::os::unix::fs::symlink(&real, &link).unwrap();

    let runner = ProcessRunner::new();
    let error = discover(&runner, &link).unwrap_err();
    assert_eq!(error.code(), "repo_root_symlink");
}
