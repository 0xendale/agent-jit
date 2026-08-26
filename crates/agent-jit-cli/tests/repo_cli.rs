#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
//! `agent-jit repo inspect` is the first surface that touches a real repository.

use std::path::Path;
use std::process::Command as StdCommand;

use assert_cmd::Command;
use predicates::str::contains;

fn bin() -> Command {
    Command::cargo_bin("agent-jit").unwrap()
}

fn init_repo(root: &Path) {
    for argv in [
        vec!["init", "--initial-branch=main"],
        vec!["config", "user.name", "test"],
        vec!["config", "user.email", "test@example.com"],
    ] {
        assert!(
            StdCommand::new("git")
                .args(&argv)
                .current_dir(root)
                .status()
                .unwrap()
                .success()
        );
    }
    std::fs::write(root.join("README.md"), "seed\n").unwrap();
    for argv in [
        vec!["add", "README.md"],
        vec!["commit", "-m", "seed", "--no-gpg-sign"],
    ] {
        assert!(
            StdCommand::new("git")
                .args(&argv)
                .current_dir(root)
                .status()
                .unwrap()
                .success()
        );
    }
}

#[test]
fn inspecting_a_repository_reports_a_stable_identity_as_json() {
    let temp = tempfile::tempdir().unwrap();
    init_repo(temp.path());
    let root = temp.path().to_str().unwrap();

    let first = bin()
        .args(["repo", "inspect", "--root", root, "--json"])
        .assert()
        .success();
    let second = bin()
        .args(["repo", "inspect", "--root", root, "--json"])
        .assert()
        .success();

    let parse = |bytes: &[u8]| -> serde_json::Value {
        serde_json::from_slice(bytes).expect("output is JSON")
    };
    let left = parse(&first.get_output().stdout);
    let right = parse(&second.get_output().stdout);

    assert_eq!(left, right, "identity must be stable across runs");
    assert!(
        left["repo_id"].as_str().unwrap().starts_with("rep_"),
        "{left}"
    );
    assert_eq!(left["head_commit"].as_str().unwrap().len(), 40);
    assert!(left["worktree_root"].is_string());
    assert!(left["git_common_dir"].is_string());
    assert_eq!(left["host"]["os"], "macos");
    assert_eq!(left["host"]["arch"], "aarch64");
}

#[test]
fn the_default_rendering_is_human_readable() {
    let temp = tempfile::tempdir().unwrap();
    init_repo(temp.path());

    bin()
        .args(["repo", "inspect", "--root", temp.path().to_str().unwrap()])
        .assert()
        .success()
        .stdout(contains("repo_id"))
        .stdout(contains("head_commit"));
}

#[test]
fn inspecting_a_non_repository_fails_without_creating_state() {
    let temp = tempfile::tempdir().unwrap();
    let before: Vec<_> = std::fs::read_dir(temp.path()).unwrap().collect();

    bin()
        .args(["repo", "inspect", "--root", temp.path().to_str().unwrap()])
        .assert()
        .failure()
        .stdout(predicates::str::is_empty())
        .stderr(contains("repo_not_a_repository"));

    let after: Vec<_> = std::fs::read_dir(temp.path()).unwrap().collect();
    assert_eq!(before.len(), after.len(), "no state may be created");
}

#[test]
fn inspecting_through_a_symlink_is_refused() {
    let temp = tempfile::tempdir().unwrap();
    let real = temp.path().join("real");
    std::fs::create_dir_all(&real).unwrap();
    init_repo(&real);
    let link = temp.path().join("link");
    std::os::unix::fs::symlink(&real, &link).unwrap();

    bin()
        .args(["repo", "inspect", "--root", link.to_str().unwrap()])
        .assert()
        .failure()
        .stdout(predicates::str::is_empty())
        .stderr(contains("repo_root_symlink"));
}

#[test]
fn repo_inspect_without_a_root_is_a_usage_error() {
    bin()
        .args(["repo", "inspect"])
        .assert()
        .code(2)
        .stderr(contains("usage: agent-jit repo inspect"));
}
