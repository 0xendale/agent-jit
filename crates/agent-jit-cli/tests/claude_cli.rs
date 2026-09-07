#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
//! The plugin is materialized into private app data and passed to Claude explicitly.
//!
//! The property under test throughout is *what agent-jit does not touch*: not the user's Claude
//! settings, not the project's `.claude/`, not `PATH`, not the observed repository.

use std::os::unix::fs::PermissionsExt as _;
use std::path::Path;
use std::process::Command as StdCommand;

use assert_cmd::Command;
use predicates::str::contains;
use serde_json::Value;

fn bin(home: &Path) -> Command {
    let mut command = Command::cargo_bin("agent-jit").unwrap();
    command.env("AGENT_JIT_HOME", home);
    command
}

fn json(output: &[u8]) -> Value {
    serde_json::from_slice(output).expect("stdout is JSON")
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

/// Writes a fake `claude` that records the argv it received, one argument per line.
fn fake_claude(directory: &Path) -> std::path::PathBuf {
    let script = directory.join("fake-claude");
    let log = directory.join("argv.log");
    std::fs::write(
        &script,
        format!(
            "#!/bin/sh\n: > '{log}'\nfor argument in \"$@\"; do printf '%s\\n' \"$argument\" >> '{log}'; done\nexit 0\n",
            log = log.display()
        ),
    )
    .unwrap();
    std::fs::set_permissions(&script, std::fs::Permissions::from_mode(0o755)).unwrap();
    script
}

fn recorded_argv(directory: &Path) -> Vec<String> {
    std::fs::read_to_string(directory.join("argv.log"))
        .unwrap_or_default()
        .lines()
        .map(str::to_owned)
        .collect()
}

#[test]
fn materializing_writes_a_complete_plugin_with_recorded_digests() {
    let home = tempfile::tempdir().unwrap();
    let assert = bin(home.path())
        .args(["claude", "materialize", "--json"])
        .assert()
        .success();
    let report = json(&assert.get_output().stdout);

    let plugin_dir = Path::new(report["plugin_dir"].as_str().unwrap());
    assert!(
        plugin_dir.starts_with(home.path()),
        "{}",
        plugin_dir.display()
    );

    for relative in [
        ".claude-plugin/plugin.json",
        "hooks/hooks.json",
        ".mcp.json",
    ] {
        assert!(plugin_dir.join(relative).exists(), "{relative} is missing");
    }

    // Every generated file is listed with its digest, and the digests describe what is on disk.
    let files = report["files"].as_object().unwrap();
    assert_eq!(files.len(), 3);
    for (relative, digest) in files {
        let bytes = std::fs::read(plugin_dir.join(relative)).unwrap();
        let recorded = digest.as_str().unwrap();
        assert_eq!(recorded.len(), 64);
        assert_eq!(
            recorded,
            blake3::hash(&bytes).to_hex().to_string(),
            "{relative} does not match its recorded digest"
        );
    }
}

#[test]
fn the_generated_hooks_only_ever_invoke_agent_jit_hook_ingest() {
    let home = tempfile::tempdir().unwrap();
    let assert = bin(home.path())
        .args(["claude", "materialize", "--json"])
        .assert()
        .success();
    let plugin_dir = json(&assert.get_output().stdout)["plugin_dir"]
        .as_str()
        .unwrap()
        .to_owned();

    let hooks: Value = serde_json::from_str(
        &std::fs::read_to_string(Path::new(&plugin_dir).join("hooks/hooks.json")).unwrap(),
    )
    .unwrap();

    let mut seen_events = Vec::new();
    for (_event, matchers) in hooks["hooks"].as_object().unwrap() {
        for matcher in matchers.as_array().unwrap() {
            for hook in matcher["hooks"].as_array().unwrap() {
                assert_eq!(hook["type"], "command");
                let command = hook["command"].as_str().unwrap();
                assert!(
                    command.contains("hook ingest --event "),
                    "hook command is not an ingest: {command}"
                );
                // A fixed event name, never anything derived from user text.
                let event = command.split("--event ").nth(1).unwrap().trim();
                seen_events.push(event.to_owned());
                // Hooks are synchronous with a short, fixed timeout.
                assert!(hook["timeout"].as_u64().unwrap() <= 10);
            }
        }
    }

    seen_events.sort();
    assert_eq!(
        seen_events,
        vec![
            "post-tool-use",
            "pre-tool-use",
            "session-end",
            "session-start",
            "stop",
            "user-prompt-submit",
        ]
    );
}

#[test]
fn the_generated_mcp_server_only_ever_invokes_agent_jit_mcp_serve() {
    let home = tempfile::tempdir().unwrap();
    let assert = bin(home.path())
        .args(["claude", "materialize", "--json"])
        .assert()
        .success();
    let plugin_dir = json(&assert.get_output().stdout)["plugin_dir"]
        .as_str()
        .unwrap()
        .to_owned();

    let mcp: Value = serde_json::from_str(
        &std::fs::read_to_string(Path::new(&plugin_dir).join(".mcp.json")).unwrap(),
    )
    .unwrap();

    let servers = mcp["mcpServers"].as_object().unwrap();
    assert_eq!(servers.len(), 1, "exactly one MCP server");
    let server = servers.values().next().unwrap();
    assert_eq!(server["args"], serde_json::json!(["mcp", "serve"]));
    assert!(server["command"].as_str().unwrap().ends_with("agent-jit"));
}

#[test]
fn rematerializing_is_idempotent() {
    let home = tempfile::tempdir().unwrap();
    let first = bin(home.path())
        .args(["claude", "materialize", "--json"])
        .assert()
        .success();
    let second = bin(home.path())
        .args(["claude", "materialize", "--json"])
        .assert()
        .success();

    let left = json(&first.get_output().stdout);
    let right = json(&second.get_output().stdout);
    assert_eq!(left["plugin_dir"], right["plugin_dir"]);
    assert_eq!(left["files"], right["files"]);
}

#[test]
fn an_interrupted_materialization_leaves_no_active_partial_version() {
    let home = tempfile::tempdir().unwrap();
    bin(home.path())
        .args(["claude", "materialize", "--json"])
        .assert()
        .success();

    // A staging directory left behind by an interrupted run must not be mistaken for a version.
    let versions = home.path().join("data/claude-plugin");
    std::fs::create_dir_all(versions.join(".staging-interrupted")).unwrap();
    std::fs::write(
        versions.join(".staging-interrupted/plugin.json"),
        b"{ half written",
    )
    .unwrap();

    let assert = bin(home.path())
        .args(["claude", "materialize", "--json"])
        .assert()
        .success();
    let plugin_dir = json(&assert.get_output().stdout)["plugin_dir"]
        .as_str()
        .unwrap()
        .to_owned();
    assert!(
        !plugin_dir.contains(".staging"),
        "a staging directory must never become the active version: {plugin_dir}"
    );
    assert!(Path::new(&plugin_dir).join(".mcp.json").exists());
}

#[test]
fn run_passes_the_original_argv_through_and_appends_exactly_one_plugin_dir() {
    let home = tempfile::tempdir().unwrap();
    let repository = tempfile::tempdir().unwrap();
    init_repo(repository.path());
    let claude = tempfile::tempdir().unwrap();
    let executable = fake_claude(claude.path());

    bin(home.path())
        .args([
            "claude",
            "run",
            "--repo",
            repository.path().to_str().unwrap(),
            "--claude-bin",
            executable.to_str().unwrap(),
            "--",
            "-p",
            "fixture prompt",
        ])
        .assert()
        .success();

    let argv = recorded_argv(claude.path());
    assert_eq!(argv[0], "-p", "{argv:?}");
    assert_eq!(argv[1], "fixture prompt", "{argv:?}");
    assert_eq!(argv[2], "--plugin-dir", "{argv:?}");
    assert_eq!(
        argv.len(),
        4,
        "exactly one --plugin-dir is appended: {argv:?}"
    );
    assert!(Path::new(&argv[3]).join(".mcp.json").exists());
}

#[test]
fn run_refuses_a_root_that_is_not_a_repository_before_launching_claude() {
    let home = tempfile::tempdir().unwrap();
    let not_a_repository = tempfile::tempdir().unwrap();
    let claude = tempfile::tempdir().unwrap();
    let executable = fake_claude(claude.path());

    bin(home.path())
        .args([
            "claude",
            "run",
            "--repo",
            not_a_repository.path().to_str().unwrap(),
            "--claude-bin",
            executable.to_str().unwrap(),
            "--",
            "-p",
            "hello",
        ])
        .assert()
        .failure()
        .stderr(contains("repo_not_a_repository"));

    assert!(
        recorded_argv(claude.path()).is_empty(),
        "Claude must not be launched when the repository is refused"
    );
}

#[test]
fn hostile_paths_are_argv_data_and_never_executed() {
    let home = tempfile::tempdir().unwrap();
    // A repository whose path contains a space, a quote, a semicolon, and a command substitution.
    let parent = tempfile::tempdir().unwrap();
    let repository = parent.path().join("re po';$(touch /tmp/agent-jit-pwned)&");
    std::fs::create_dir_all(&repository).unwrap();
    init_repo(&repository);

    let claude = tempfile::tempdir().unwrap();
    let executable = fake_claude(claude.path());
    let marker = Path::new("/tmp/agent-jit-pwned");
    let _ = std::fs::remove_file(marker);

    bin(home.path())
        .args([
            "claude",
            "run",
            "--repo",
            repository.to_str().unwrap(),
            "--claude-bin",
            executable.to_str().unwrap(),
            "--",
            "-p",
            "hello; $(touch /tmp/agent-jit-pwned)",
        ])
        .assert()
        .success();

    assert!(
        !marker.exists(),
        "a path or argument must never be executed as a command"
    );
    let argv = recorded_argv(claude.path());
    assert_eq!(argv[1], "hello; $(touch /tmp/agent-jit-pwned)", "{argv:?}");
}

#[test]
fn run_does_not_touch_claude_settings_the_project_or_the_repository() {
    let home = tempfile::tempdir().unwrap();
    let repository = tempfile::tempdir().unwrap();
    init_repo(repository.path());
    let claude = tempfile::tempdir().unwrap();
    let executable = fake_claude(claude.path());

    // A user settings file and a project-level .claude/ that must both come back untouched.
    let user_settings = claude.path().join("settings.json");
    std::fs::write(&user_settings, b"{\"theme\":\"dark\"}").unwrap();
    let project_claude = repository.path().join(".claude");
    std::fs::create_dir_all(&project_claude).unwrap();
    std::fs::write(project_claude.join("settings.json"), b"{\"hooks\":{}}").unwrap();

    let before_settings = std::fs::read(&user_settings).unwrap();
    let before_project = std::fs::read(project_claude.join("settings.json")).unwrap();
    let before_status = StdCommand::new("git")
        .args(["status", "--porcelain"])
        .current_dir(repository.path())
        .output()
        .unwrap()
        .stdout;

    bin(home.path())
        .args([
            "claude",
            "run",
            "--repo",
            repository.path().to_str().unwrap(),
            "--claude-bin",
            executable.to_str().unwrap(),
            "--",
            "-p",
            "hello",
        ])
        .assert()
        .success();

    assert_eq!(std::fs::read(&user_settings).unwrap(), before_settings);
    assert_eq!(
        std::fs::read(project_claude.join("settings.json")).unwrap(),
        before_project
    );
    assert_eq!(
        StdCommand::new("git")
            .args(["status", "--porcelain"])
            .current_dir(repository.path())
            .output()
            .unwrap()
            .stdout,
        before_status
    );
}

#[test]
fn uninstall_removes_only_the_generated_plugin() {
    let home = tempfile::tempdir().unwrap();
    bin(home.path())
        .args(["claude", "materialize", "--json"])
        .assert()
        .success();

    // Something unrelated living alongside the plugin must survive.
    let unrelated = home.path().join("data/unrelated.txt");
    std::fs::write(&unrelated, b"keep me").unwrap();

    bin(home.path())
        .args(["claude", "uninstall", "--json"])
        .assert()
        .success();

    assert!(!home.path().join("data/claude-plugin").exists());
    assert!(unrelated.exists(), "unrelated files must be preserved");
    // The private state itself is not removed by a plugin uninstall.
    assert!(home.path().join("data").exists());
}

#[test]
fn uninstalling_when_nothing_is_installed_is_not_an_error() {
    let home = tempfile::tempdir().unwrap();
    let assert = bin(home.path())
        .args(["claude", "uninstall", "--json"])
        .assert()
        .success();
    assert_eq!(json(&assert.get_output().stdout)["removed"], false);
}

#[test]
fn run_without_a_repo_is_a_usage_error() {
    let home = tempfile::tempdir().unwrap();
    bin(home.path())
        .args(["claude", "run"])
        .assert()
        .code(2)
        .stderr(contains("usage: agent-jit claude"));
}
