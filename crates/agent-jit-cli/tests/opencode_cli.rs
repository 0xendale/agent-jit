#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
//! The `OpenCode` launcher is a settings-mutation firewall: injection happens through environment
//! and private state only, and the runtime's argv is never rewritten.

mod corpus_support;

use std::os::unix::fs::PermissionsExt as _;
use std::path::Path;

use corpus_support::{bin, init_repo, json};

const SESSION: &str = "op-probe-1111-2222-3333-444455556666";

/// A fake `OpenCode` runtime that records how it was invoked.
fn fake_opencode(tools: &Path, record: &Path) -> String {
    let record = record.display();
    let executable = tools.join("opencode");
    std::fs::write(
        &executable,
        format!(
            "#!/bin/sh\nif [ \"$1\" = '--version' ]; then printf '1.18.29 (opencode)\\n'; exit 0; fi\n\
             printf 'cwd: %s\\n' \"$PWD\" >> \"{record}\"\n\
             printf 'argv:%s\\n' \"$(printf ' %s' \"$@\")\" >> \"{record}\"\n\
             printf 'config: %s\\n' \"$OPENCODE_CONFIG\" >> \"{record}\"\n\
             printf 'home: %s\\n' \"$AGENT_JIT_HOME\" >> \"{record}\"\n\
             printf 'runtime: %s\\n' \"$AGENT_JIT_RUNTIME_VERSION\" >> \"{record}\"\n"
        ),
    )
    .unwrap();
    std::fs::set_permissions(&executable, std::fs::Permissions::from_mode(0o700)).unwrap();
    executable.to_str().unwrap().to_owned()
}

#[test]
fn materialize_writes_a_complete_plugin_with_recorded_digests() {
    let home = tempfile::tempdir().unwrap();
    let output = bin(home.path())
        .args(["opencode", "materialize", "--json"])
        .assert()
        .success();
    let report = json(&output.get_output().stdout);
    let directory = Path::new(report["plugin_dir"].as_str().unwrap());

    let files = report["files"].as_object().unwrap();
    assert_eq!(files.len(), 2, "bridge and config: {files:?}");
    for (relative, expected) in files {
        let bytes = std::fs::read(directory.join(relative)).unwrap();
        assert_eq!(
            expected.as_str().unwrap(),
            agent_jit_domain::canonical::Digest::of_bytes(&bytes).to_string(),
            "{relative} digest mismatch"
        );
    }

    // The bridge references the recorder binary; the config loads the bridge by absolute path.
    let bridge = std::fs::read_to_string(directory.join("bridge.ts")).unwrap();
    assert!(
        bridge.contains("\"hook\", \"ingest\""),
        "bridge must invoke the recorder: {bridge}"
    );
    assert!(
        bridge.contains("Bun.spawn"),
        "bridge must spawn the recorder as argv data: {bridge}"
    );
    let config = std::fs::read_to_string(directory.join("opencode.json")).unwrap();
    let config: serde_json::Value = serde_json::from_str(&config).unwrap();
    let plugin = config["plugin"].as_array().unwrap()[0].as_str().unwrap();
    assert!(
        Path::new(plugin).is_absolute()
            && plugin.ends_with("bridge.ts")
            && Path::new(plugin).exists(),
        "plugin entry must be the materialized bridge: {plugin}"
    );

    // Everything the product materialized is private.
    for entry in walk(directory) {
        let mode = std::fs::symlink_metadata(&entry)
            .unwrap()
            .permissions()
            .mode()
            & 0o777;
        let expected = if std::fs::symlink_metadata(&entry).unwrap().is_dir() {
            0o700
        } else {
            0o600
        };
        assert_eq!(mode, expected, "`{}` is {mode:o}", entry.display());
    }
}

/// Every path under `root`, depth-first.
fn walk(root: &Path) -> Vec<std::path::PathBuf> {
    let mut found = Vec::new();
    for entry in std::fs::read_dir(root).unwrap() {
        let path = entry.unwrap().path();
        if path.is_dir() {
            found.extend(walk(&path));
        }
        found.push(path);
    }
    found
}

#[test]
fn rematerializing_is_idempotent() {
    let home = tempfile::tempdir().unwrap();
    let first = bin(home.path())
        .args(["opencode", "materialize", "--json"])
        .assert()
        .success();
    let second = bin(home.path())
        .args(["opencode", "materialize", "--json"])
        .assert()
        .success();
    assert_eq!(
        json(&first.get_output().stdout)["plugin_dir"],
        json(&second.get_output().stdout)["plugin_dir"]
    );
}

#[test]
fn an_interrupted_materialization_leaves_no_active_partial_version() {
    let home = tempfile::tempdir().unwrap();
    bin(home.path())
        .args(["opencode", "materialize", "--json"])
        .assert()
        .success();
    let report = json(
        &bin(home.path())
            .args(["opencode", "materialize", "--json"])
            .assert()
            .success()
            .get_output()
            .stdout,
    );
    let root = Path::new(report["plugin_dir"].as_str().unwrap())
        .parent()
        .unwrap()
        .to_owned();

    // A leftover staging directory is never a version, and never blocks rematerialization.
    std::fs::create_dir_all(root.join(".staging-99999")).unwrap();
    std::fs::write(root.join(".staging-99999/bridge.ts"), b"partial").unwrap();
    bin(home.path())
        .args(["opencode", "materialize", "--json"])
        .assert()
        .success();
    let versions: Vec<_> = std::fs::read_dir(&root)
        .unwrap()
        .map(|entry| entry.unwrap().file_name().to_string_lossy().to_string())
        .filter(|name| !name.starts_with(".staging-"))
        .collect();
    assert_eq!(
        versions.len(),
        1,
        "exactly one active version: {versions:?}"
    );
    assert!(
        std::fs::read(root.join(&versions[0]).join("bridge.ts")).is_ok(),
        "the active version is complete"
    );
}

#[test]
fn run_without_a_repo_is_a_usage_error() {
    let home = tempfile::tempdir().unwrap();
    bin(home.path())
        .args(["opencode", "run"])
        .assert()
        .code(2)
        .stderr(predicates::str::contains("usage: agent-jit opencode"));
}

#[test]
fn run_refuses_a_root_that_is_not_a_repository_before_launching_opencode() {
    let home = tempfile::tempdir().unwrap();
    let not_a_repo = tempfile::tempdir().unwrap();
    bin(home.path())
        .args([
            "opencode",
            "run",
            "--repo",
            not_a_repo.path().to_str().unwrap(),
        ])
        .assert()
        .failure()
        .stderr(predicates::str::contains("repo_not_a_repository"));
}

#[test]
fn run_passes_the_original_argv_through_and_injects_only_environment() {
    let home = tempfile::tempdir().unwrap();
    let repo = tempfile::tempdir().unwrap();
    init_repo(repo.path());
    let tools = tempfile::tempdir().unwrap();
    let record = tools.path().join("invocation");
    let opencode = fake_opencode(tools.path(), &record);

    bin(home.path())
        .env("HOME", tools.path())
        .args([
            "opencode",
            "run",
            "--repo",
            repo.path().to_str().unwrap(),
            "--opencode-bin",
            &opencode,
            "--",
            "run",
            "--format",
            "json",
            "explain closures",
        ])
        .assert()
        .success();

    let observed = std::fs::read_to_string(&record).unwrap();
    let canonical_repo = repo.path().canonicalize().unwrap();
    assert!(
        observed.contains(&format!("cwd: {}", canonical_repo.display())),
        "runtime cwd is the repository (git resolves symlinks): {observed}"
    );
    assert!(
        observed.contains("argv: run --format json explain closures"),
        "argv passes through untouched: {observed}"
    );
    assert!(
        !observed.contains("--plugin"),
        "no plugin flag is injected into argv: {observed}"
    );

    let config_path = observed
        .lines()
        .find_map(|line| line.strip_prefix("config: "))
        .unwrap()
        .to_owned();
    let config = std::fs::read_to_string(&config_path).unwrap();
    let config: serde_json::Value = serde_json::from_str(&config).unwrap();
    let plugin = config["plugin"].as_array().unwrap()[0].as_str().unwrap();
    assert!(
        Path::new(plugin).starts_with(home.path()),
        "private state only: {plugin}"
    );

    let home_line = observed
        .lines()
        .find_map(|line| line.strip_prefix("home: "))
        .unwrap();
    assert_eq!(home_line, home.path().to_str().unwrap());

    let runtime = observed
        .lines()
        .find_map(|line| line.strip_prefix("runtime: "))
        .unwrap();
    assert_eq!(runtime, "1.18.29");
}

#[test]
fn run_synthesizes_session_end_for_sessions_the_plugin_observed() {
    let home = tempfile::tempdir().unwrap();
    let repo = tempfile::tempdir().unwrap();
    init_repo(repo.path());
    let tools = tempfile::tempdir().unwrap();

    // The fake runtime plays the bridge's part: it leaves a session marker in private state.
    let executable = tools.path().join("opencode");
    std::fs::write(
        &executable,
        format!(
            "#!/bin/sh\nif [ \"$1\" = '--version' ]; then printf '1.18.29 (opencode)\\n'; exit 0; fi\n\
             cfg_dir=$(dirname \"$OPENCODE_CONFIG\")\n\
             mkdir -p \"$cfg_dir/sessions\"\n\
             printf '{{\"directory\":\"{repo}\",\"runtime_version\":\"1.18.28\"}}' > \"$cfg_dir/sessions/{SESSION}.json\"\n",
            repo = repo.path().display()
        ),
    )
    .unwrap();
    std::fs::set_permissions(&executable, std::fs::Permissions::from_mode(0o700)).unwrap();

    bin(home.path())
        .env("HOME", tools.path())
        .args([
            "opencode",
            "run",
            "--repo",
            repo.path().to_str().unwrap(),
            "--opencode-bin",
            executable.to_str().unwrap(),
            "--",
            "run",
            "harmless",
        ])
        .assert()
        .success();

    // The launcher turned the marker into a session-end event in the spool.
    let segments =
        agent_jit_engine::recorder::SegmentStore::open(&home.path().join("cache/spool/segments"))
            .unwrap();
    let read = segments.read_session(SESSION).unwrap();
    assert!(!read.accepted.is_empty(), "no session-end synthesized");
    assert!(
        read.accepted.iter().any(|segment| matches!(
            segment.payload.kind,
            agent_jit_engine::adapters::claude_hooks::HookEventKind::SessionEnd
        )),
        "expected a session-end event"
    );
    let session_end = read
        .accepted
        .iter()
        .find(|segment| {
            matches!(
                segment.payload.kind,
                agent_jit_engine::adapters::claude_hooks::HookEventKind::SessionEnd
            )
        })
        .unwrap();
    assert_eq!(
        session_end.payload.claude_version.as_deref(),
        Some("1.18.28")
    );

    // The marker is consumed so a later run cannot resurrect the session.
    let report = json(
        &bin(home.path())
            .args(["opencode", "materialize", "--json"])
            .assert()
            .success()
            .get_output()
            .stdout,
    );
    let sessions = Path::new(report["plugin_dir"].as_str().unwrap()).join("sessions");
    assert!(
        !sessions.join(format!("{SESSION}.json")).exists(),
        "marker must be consumed"
    );
}

#[test]
fn uninstall_removes_only_the_generated_plugin() {
    let home = tempfile::tempdir().unwrap();
    bin(home.path())
        .args(["opencode", "materialize", "--json"])
        .assert()
        .success();
    let report = json(
        &bin(home.path())
            .args(["opencode", "materialize", "--json"])
            .assert()
            .success()
            .get_output()
            .stdout,
    );
    let root = Path::new(report["plugin_dir"].as_str().unwrap())
        .parent()
        .unwrap()
        .to_owned();
    let unrelated = root.parent().unwrap().join("unrelated-state.txt");
    std::fs::write(&unrelated, b"keep me").unwrap();

    bin(home.path())
        .args(["opencode", "uninstall", "--json"])
        .assert()
        .success();
    assert!(!root.exists(), "owned plugin removed");
    assert_eq!(std::fs::read(&unrelated).unwrap(), b"keep me");
}

#[test]
fn uninstalling_when_nothing_is_installed_is_not_an_error() {
    let home = tempfile::tempdir().unwrap();
    let output = bin(home.path())
        .args(["opencode", "uninstall", "--json"])
        .assert()
        .success();
    assert_eq!(json(&output.get_output().stdout)["removed"], false);
}

#[test]
fn hostile_paths_are_argv_data_and_never_executed() {
    let home = tempfile::tempdir().unwrap();
    let hostile_root = tempfile::tempdir().unwrap();
    let hostile = hostile_root
        .path()
        .join("re po';$(touch /tmp/agent-jit-pwned)&");
    std::fs::create_dir_all(&hostile).unwrap();
    init_repo(&hostile);
    let tools = tempfile::tempdir().unwrap();
    let opencode = fake_opencode(tools.path(), &tools.path().join("invocation"));

    bin(home.path())
        .env("HOME", tools.path())
        .args([
            "opencode",
            "run",
            "--repo",
            hostile.to_str().unwrap(),
            "--opencode-bin",
            &opencode,
            "--",
            "run",
            "task",
        ])
        .assert()
        .success();

    assert!(
        !Path::new("/tmp/agent-jit-pwned").exists(),
        "hostile path bytes were executed"
    );
}
