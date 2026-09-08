#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
//! Real `OpenCode` capture conformance through the full launch path.
//!
//! These tests run the installed `OpenCode` runtime for real. When no runtime is installed the
//! test prints its skip reason and returns; release evidence must always come from a machine
//! where the runtime is present, never from a skip.

mod corpus_support;
mod opencode_smoke_support;

use std::path::{Path, PathBuf};
use std::process::Command as StdCommand;
use std::time::Duration;

use agent_jit_domain::canonical::Digest;
use agent_jit_engine::adapters::claude_hooks::HookPayload;
use corpus_support::{bin, init_repo, json};
use opencode_smoke_support::{configured_model, installed_opencode, skip};
use serde_json::Value;

const MARKER: &str = "fixture-note-7f3a9c1d";

fn git(root: &Path, args: [&str; 2]) -> String {
    let output = StdCommand::new("git")
        .args(args)
        .current_dir(root)
        .output()
        .unwrap();
    assert!(output.status.success(), "git {args:?} failed");
    String::from_utf8(output.stdout).unwrap().trim().to_owned()
}

fn digest_file(path: &Path) -> Option<String> {
    std::fs::read(path)
        .ok()
        .map(|bytes| Digest::of_bytes(&bytes).to_string())
}

fn opencode_settings_paths() -> [PathBuf; 2] {
    let config = Path::new(&std::env::var("HOME").unwrap()).join(".config/opencode");
    [config.join("opencode.json"), config.join("opencode.jsonc")]
}

fn assert_raw_capture(home: &Path, version: &str) -> String {
    let segments_root = home.join("cache/spool/segments");
    let session_dirs: Vec<_> = std::fs::read_dir(&segments_root)
        .unwrap()
        .map(|entry| entry.unwrap().path())
        .collect();
    assert_eq!(
        session_dirs.len(),
        1,
        "expected exactly one captured session: {session_dirs:?}"
    );
    let session_id = session_dirs[0]
        .file_stem()
        .unwrap()
        .to_string_lossy()
        .to_string();

    let segments = agent_jit_engine::recorder::SegmentStore::open(&segments_root).unwrap();
    let read = segments.read_session(&session_id).unwrap();
    assert!(read.quarantined.is_empty(), "quarantined capture bytes");
    for segment in &read.accepted {
        assert_eq!(
            segment.payload.claude_version.as_deref(),
            Some(version),
            "segment missing runtime provenance"
        );
        assert_eq!(
            segment.payload.adapter_schema, "agent_jit.opencode_hooks.v1",
            "segment normalized by the wrong adapter"
        );
    }
    let captured: Vec<String> = read
        .accepted
        .iter()
        .map(|segment| format!("{:?}", segment.payload.kind))
        .collect();
    for required in ["SessionStart", "UserPromptSubmit", "Stop", "SessionEnd"] {
        assert!(
            captured.iter().any(|kind| kind == required),
            "missing {required} in {captured:?}"
        );
    }
    let prompt = read
        .accepted
        .iter()
        .find_map(|segment| match &segment.payload.payload {
            HookPayload::UserPrompt { prompt } => Some(prompt.value()),
            HookPayload::SessionStart { .. }
            | HookPayload::PreToolUse { .. }
            | HookPayload::PostToolUse(_)
            | HookPayload::Stop { .. }
            | HookPayload::SessionEnd { .. } => None,
        })
        .unwrap();
    assert!(!prompt.is_empty(), "captured prompt must not be empty");
    session_id
}

fn recover_trajectory(home: &Path, repo: &str) -> String {
    let recovered = bin(home)
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

    let listed = bin(home)
        .args(["trace", "list", "--repo", repo, "--json"])
        .assert()
        .success();
    let list_report = json(&listed.get_output().stdout);
    let trajectories = list_report["trajectories"].as_array().unwrap();
    assert_eq!(
        trajectories.len(),
        1,
        "expected one trajectory: {list_report}"
    );
    trajectories[0]["trajectory_id"]
        .as_str()
        .unwrap()
        .to_owned()
}

fn assert_trace_and_export(home: &Path, repo: &str, trajectory_id: &str) {
    let shown = bin(home)
        .args(["trace", "show", trajectory_id, "--json"])
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

    let output_dir = home.join("export");
    let exported = bin(home)
        .args([
            "corpus",
            "export",
            "--repo",
            repo,
            "--output",
            output_dir.to_str().unwrap(),
            "--mode",
            "full-redacted",
            "--json",
        ])
        .assert()
        .success();
    let export_report: Value = json(&exported.get_output().stdout);
    assert!(export_report["manifest_digest"].is_string());
    for file in export_report["record_files"].as_array().unwrap() {
        let record = Path::new(file.as_str().unwrap());
        bin(home)
            .args(["schema", "validate", &record.to_string_lossy()])
            .assert()
            .success();
    }
}

#[test]
fn real_opencode_session_is_captured_finalized_and_exported() {
    let Some(version) = installed_opencode() else {
        skip("opencode executable unavailable; real capture cannot run here");
        return;
    };
    let Some(model) = configured_model() else {
        skip("AGENT_JIT_OPENCODE_TEST_MODEL is unset; real provider capture cannot run here");
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

    // Recorder health passes with the real runtimes present, before any capture.
    let doctor = bin(home.path())
        .args(["doctor", "--scope", "recorder", "--json"])
        .assert()
        .success();
    let doctor_report = json(&doctor.get_output().stdout);
    assert_eq!(doctor_report["status"], "pass");
    assert_eq!(
        doctor_report["checks"]["opencode"]["details"]["version"],
        version
    );

    // User settings and repository state are snapshotted before launch.
    let settings = opencode_settings_paths();
    let settings_before = settings
        .iter()
        .map(|path| digest_file(path))
        .collect::<Vec<_>>();
    let head_before = git(repo.path(), ["rev-parse", "HEAD"]);

    // When: one real headless session runs through the launcher and its bridge.
    let run = bin(home.path())
        .timeout(Duration::from_secs(240))
        .args([
            "opencode",
            "run",
            "--repo",
            repo_str,
            "--",
            "run",
            "--format",
            "json",
            "--model",
            model.as_str(),
            "Read notes.txt and reply with exactly its contents and nothing else.",
        ])
        .assert()
        .success();

    // Then: the task itself succeeded and used the fixture through a tool.
    let answer = std::str::from_utf8(&run.get_output().stdout).unwrap();
    let errors = std::str::from_utf8(&run.get_output().stderr).unwrap();
    assert!(
        answer.contains(MARKER),
        "reply missing marker; stdout was: {answer}; stderr was: {errors}"
    );

    assert_raw_capture(home.path(), &version);
    let trajectory_id = recover_trajectory(home.path(), repo_str);
    assert_trace_and_export(home.path(), repo_str, &trajectory_id);

    // Neither user settings nor the observed repository's tracked source changed.
    let settings_after = settings
        .iter()
        .map(|path| digest_file(path))
        .collect::<Vec<_>>();
    assert_eq!(settings_before, settings_after);
    assert_eq!(git(repo.path(), ["rev-parse", "HEAD"]), head_before);
    let status = git(repo.path(), ["status", "--porcelain"]);
    assert!(
        status.lines().all(|line| line.starts_with("?? .om")),
        "unexpected repository mutation: {status}"
    );
    assert!(!repo.path().join(".agent-jit").exists());
    assert!(!repo.path().join(".opencode").exists());

    // Uninstall removes only the owned bridge.
    bin(home.path())
        .args(["opencode", "uninstall", "--json"])
        .assert()
        .success();
}
