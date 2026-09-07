#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
//! Real Claude Code capture conformance through the full launch path.
//!
//! These tests run the installed Claude runtime for real. When no runtime is
//! installed the test prints its skip reason and returns; release evidence must
//! always come from a machine where the runtime is present, never from a skip.

mod corpus_support;

use std::io::{self, Write as _};
use std::os::unix::fs::PermissionsExt as _;
use std::path::{Path, PathBuf};
use std::process::Command as StdCommand;
use std::time::Duration;

use agent_jit_domain::canonical::Digest;
use corpus_support::{bin, init_repo, json};
use serde_json::Value;

const MARKER: &str = "fixture-note-7f3a9c1d";

/// Returns the installed Claude version, or `None` when the runtime is absent.
fn installed_claude() -> Option<String> {
    let output = StdCommand::new("claude").arg("--version").output().ok()?;
    if !output.status.success() {
        return None;
    }
    let text = std::str::from_utf8(&output.stdout).ok()?;
    text.split_whitespace().next().map(str::to_owned)
}

fn skip(reason: &str) {
    let _ = writeln!(io::stderr().lock(), "SKIP: {reason}");
}

fn git(root: &Path, args: [&str; 2]) -> String {
    let output = StdCommand::new("git")
        .args(args)
        .current_dir(root)
        .output()
        .unwrap();
    assert!(output.status.success(), "git {args:?} failed");
    String::from_utf8(output.stdout).unwrap().trim().to_owned()
}

/// Tracked-file changes only; the runtime may leave its own untracked artifacts.
fn tracked_changes(root: &Path) -> String {
    git(root, ["status", "--porcelain"])
        .lines()
        .filter(|line| !line.starts_with("??"))
        .collect::<Vec<_>>()
        .join("\n")
}

fn assert_private_tree(path: &Path) {
    let metadata = std::fs::symlink_metadata(path).unwrap();
    let mode = metadata.permissions().mode() & 0o777;
    if metadata.is_dir() {
        assert_eq!(mode, 0o700, "directory `{}` is {mode:o}", path.display());
        for entry in std::fs::read_dir(path).unwrap() {
            assert_private_tree(&entry.unwrap().path());
        }
    } else {
        assert_eq!(mode, 0o600, "file `{}` is {mode:o}", path.display());
    }
}

fn settings_path() -> PathBuf {
    Path::new(&std::env::var("HOME").unwrap()).join(".claude/settings.json")
}

fn digest_file(path: &Path) -> Option<String> {
    std::fs::read(path)
        .ok()
        .map(|bytes| Digest::of_bytes(&bytes).to_string())
}

#[test]
#[allow(clippy::too_many_lines)]
fn real_claude_session_is_captured_finalized_and_exported() {
    let Some(version) = installed_claude() else {
        skip("claude executable unavailable; real capture cannot run here");
        return;
    };

    // Given: private state, a disposable repository, and a deterministic fixture.
    let home = tempfile::tempdir().unwrap();
    let repo = tempfile::tempdir().unwrap();
    init_repo(repo.path());
    std::fs::write(repo.path().join("notes.txt"), format!("{MARKER}\n")).unwrap();
    for argv in [
        ["add", "notes.txt"].as_slice(),
        ["commit", "-m", "fixture", "--no-gpg-sign"].as_slice(),
    ] {
        assert!(
            StdCommand::new("git")
                .args(argv)
                .current_dir(repo.path())
                .status()
                .unwrap()
                .success()
        );
    }
    let repo_str = repo.path().to_str().unwrap();
    bin(home.path())
        .args(["store", "migrate", "--json"])
        .assert()
        .success();

    // Recorder health passes with the real runtime present, before any capture.
    let doctor = bin(home.path())
        .args(["doctor", "--scope", "recorder", "--json"])
        .assert()
        .success();
    let doctor_report = json(&doctor.get_output().stdout);
    assert_eq!(doctor_report["status"], "pass");
    assert_eq!(
        doctor_report["checks"]["claude"]["details"]["version"],
        version
    );

    // User settings and repository state are snapshotted before launch.
    let settings_before = digest_file(&settings_path());
    let head_before = git(repo.path(), ["rev-parse", "HEAD"]);

    // When: one real main session runs through the launcher and its plugin.
    let run = bin(home.path())
        .timeout(Duration::from_secs(240))
        .args([
            "claude",
            "run",
            "--repo",
            repo_str,
            "--",
            "-p",
            "--output-format",
            "json",
            "--allowedTools=Read",
            "Read notes.txt and reply with exactly its contents and nothing else.",
        ])
        .assert()
        .success();

    // Then: the task itself succeeded and used the fixture through a tool.
    let result: Value = serde_json::from_slice(&run.get_output().stdout)
        .expect("claude stdout is one result document");
    assert_eq!(result["is_error"], false, "claude reported an error");
    let session_id = result["session_id"].as_str().unwrap().to_owned();
    let answer = result["result"].as_str().unwrap();
    assert!(answer.contains(MARKER), "reply missing marker: {answer}");

    // Every captured event carries the observed runtime version.
    let segments =
        agent_jit_engine::recorder::SegmentStore::open(&home.path().join("cache/spool/segments"))
            .unwrap();
    let read = segments.read_session(&session_id).unwrap();
    assert!(read.quarantined.is_empty(), "quarantined capture bytes");
    assert!(!read.accepted.is_empty(), "no events captured");
    for segment in &read.accepted {
        assert_eq!(
            segment.payload.claude_version.as_deref(),
            Some(version.as_str()),
            "segment missing runtime provenance"
        );
    }
    let captured: Vec<String> = read
        .accepted
        .iter()
        .map(|segment| format!("{:?}", segment.payload.kind))
        .collect();
    for required in [
        "SessionStart",
        "UserPromptSubmit",
        "PreToolUse",
        "PostToolUse",
        "Stop",
        "SessionEnd",
    ] {
        assert!(
            captured.iter().any(|kind| kind == required),
            "missing {required} in {captured:?}"
        );
    }

    // Recovery finalizes the complete session into exactly one trajectory.
    let recovered = bin(home.path())
        .args(["store", "recover", "--json"])
        .assert()
        .success();
    let recover_report = json(&recovered.get_output().stdout);
    assert_eq!(
        recover_report["recovered"].as_array().unwrap().len(),
        1,
        "expected one recovered session: {recover_report}"
    );
    assert_eq!(recover_report["pending"].as_array().unwrap().len(), 0);
    assert_eq!(recover_report["quarantined_segments"], 0);

    // Finalization produces exactly one trajectory for the session.
    let listed = bin(home.path())
        .args(["trace", "list", "--repo", repo_str, "--json"])
        .assert()
        .success();
    let list_report = json(&listed.get_output().stdout);
    let trajectories = list_report["trajectories"].as_array().unwrap();
    assert_eq!(
        trajectories.len(),
        1,
        "expected one trajectory: {list_report}"
    );
    let trajectory_id = trajectories[0]["trajectory_id"]
        .as_str()
        .unwrap()
        .to_owned();

    let shown = bin(home.path())
        .args(["trace", "show", &trajectory_id, "--json"])
        .assert()
        .success();
    let show = json(&shown.get_output().stdout);
    let events = show["events"].as_array().unwrap();
    for required in ["SessionStart", "UserPrompt", "Stop", "SessionEnd"] {
        assert!(
            events
                .iter()
                .any(|event| event["kind"].as_str() == Some(required)),
            "finalized trajectory missing {required}"
        );
    }
    assert!(events.iter().any(|event| {
        event["kind"].as_str() == Some("ToolCall") && event["tool_name"].as_str() == Some("Read")
    }));
    assert!(events.iter().any(|event| {
        event["kind"].as_str() == Some("ToolResult") && event["tool_name"].as_str() == Some("Read")
    }));

    // The spool is drained once the session is finalized.
    assert_eq!(
        std::fs::read_dir(home.path().join("cache/spool/segments"))
            .unwrap()
            .count(),
        0
    );

    // Private permissions hold across the whole state tree.
    assert_private_tree(home.path());

    // Neither user settings nor the observed repository's tracked source changed.
    assert_eq!(settings_before, digest_file(&settings_path()));
    assert_eq!(git(repo.path(), ["rev-parse", "HEAD"]), head_before);
    assert_eq!(tracked_changes(repo.path()), "");

    // The exported corpus validates against the checked-in schemas.
    let output_dir = home.path().join("export");
    let exported = bin(home.path())
        .args([
            "corpus",
            "export",
            "--repo",
            repo_str,
            "--output",
            output_dir.to_str().unwrap(),
            "--mode",
            "full-redacted",
            "--json",
        ])
        .assert()
        .success();
    let export_report = json(&exported.get_output().stdout);
    assert!(export_report["manifest_digest"].is_string());
    for file in export_report["record_files"].as_array().unwrap() {
        let record = Path::new(file.as_str().unwrap());
        bin(home.path())
            .args(["schema", "validate", &record.to_string_lossy()])
            .assert()
            .success();
    }

    // Uninstall removes only the owned plugin.
    bin(home.path())
        .args(["claude", "uninstall", "--json"])
        .assert()
        .success();
    assert!(!home.path().join("state/claude-plugin").exists());
}

#[test]
fn real_claude_finishes_silently_when_recorder_storage_is_unwritable() {
    let Some(_version) = installed_claude() else {
        skip("claude executable unavailable; real failure path cannot run here");
        return;
    };

    // Given: healthy state whose spool can no longer be written.
    let home = tempfile::tempdir().unwrap();
    let repo = tempfile::tempdir().unwrap();
    init_repo(repo.path());
    bin(home.path())
        .args(["store", "migrate", "--json"])
        .assert()
        .success();
    bin(home.path())
        .args(["claude", "materialize", "--json"])
        .assert()
        .success();
    // The launcher repairs its own top-level directories, so the segment store
    // itself is what stays unwritable for the run.
    let segments = home.path().join("cache/spool/segments");
    std::fs::create_dir_all(&segments).unwrap();
    std::fs::set_permissions(&segments, std::fs::Permissions::from_mode(0o500)).unwrap();

    // When: a real session runs against the unwritable recorder.
    let run = bin(home.path())
        .timeout(Duration::from_secs(240))
        .args([
            "claude",
            "run",
            "--repo",
            repo.path().to_str().unwrap(),
            "--",
            "-p",
            "--output-format",
            "json",
            "Reply with exactly: ok",
        ])
        .assert()
        .success();

    // Then: the user's task still finished, with clean hook output.
    let result: Value = serde_json::from_slice(&run.get_output().stdout)
        .expect("claude stdout is one result document");
    assert_eq!(result["is_error"], false, "claude task failed");

    // No trajectory is claimed from the failed recording.
    let listed = bin(home.path())
        .args([
            "trace",
            "list",
            "--repo",
            repo.path().to_str().unwrap(),
            "--json",
        ])
        .assert()
        .success();
    assert_eq!(
        json(&listed.get_output().stdout)["trajectories"]
            .as_array()
            .unwrap()
            .len(),
        0
    );

    // The recorder reports its own failure instead of hiding it.
    bin(home.path())
        .args(["doctor", "--scope", "recorder", "--json"])
        .assert()
        .failure();

    // Cleanup: restore permissions so the temporary home can be removed.
    std::fs::set_permissions(&segments, std::fs::Permissions::from_mode(0o700)).unwrap();
}
