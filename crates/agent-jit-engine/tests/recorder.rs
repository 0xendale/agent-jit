#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
//! The recorder turns a stream of hook events into one trajectory, and survives being killed
//! halfway through doing it.

use std::path::Path;

use agent_jit_domain::redaction::Redactor;
use agent_jit_engine::adapters::claude_hooks::{HookEventKind, NormalizedHook, normalize};
use agent_jit_engine::recorder::{Recorder, SegmentStore, SegmentWriteError};

const SESSION: &str = "b2c3d4e5-1111-2222-3333-444455556666";

fn hook(kind: HookEventKind, extra: &serde_json::Value) -> NormalizedHook {
    let mut document = serde_json::json!({
        "session_id": SESSION,
        "cwd": "/tmp/pilot",
        "hook_event_name": kind.claude_event_name(),
        "transcript_path": "/tmp/pilot-transcript.jsonl",
    });
    if let (Some(object), Some(extra)) = (document.as_object_mut(), extra.as_object()) {
        for (key, value) in extra {
            object.insert(key.clone(), value.clone());
        }
    }
    normalize(kind, &document.to_string(), &Redactor::new(), Some("2.0.0")).unwrap()
}

fn prompt(text: &str) -> NormalizedHook {
    hook(
        HookEventKind::UserPromptSubmit,
        &serde_json::json!({"prompt": text}),
    )
}

fn tool(command: &str) -> NormalizedHook {
    hook(
        HookEventKind::PostToolUse,
        &serde_json::json!({
            "tool_name": "Bash",
            "tool_input": {"command": command},
            "tool_response": {"stdout": "ok"}
        }),
    )
}

fn stop() -> NormalizedHook {
    hook(
        HookEventKind::Stop,
        &serde_json::json!({"stop_hook_active": false}),
    )
}

fn session_end() -> NormalizedHook {
    hook(
        HookEventKind::SessionEnd,
        &serde_json::json!({"reason": "exit"}),
    )
}

fn segments(directory: &Path) -> SegmentStore {
    SegmentStore::open(directory).unwrap()
}

#[test]
fn each_event_becomes_one_immutable_segment() {
    let temp = tempfile::tempdir().unwrap();
    let store = segments(temp.path());

    store.write(&prompt("do the thing"), 1_000).unwrap();
    store.write(&tool("cargo test"), 2_000).unwrap();

    let session_segments = store.read_session(SESSION).unwrap();
    assert_eq!(session_segments.accepted.len(), 2);
    assert!(session_segments.quarantined.is_empty());
    // Order keys are monotonic in the order events were written.
    assert!(session_segments.accepted[0].order_key < session_segments.accepted[1].order_key);
}

#[test]
fn a_repeated_hook_is_deduplicated_by_digest() {
    let temp = tempfile::tempdir().unwrap();
    let store = segments(temp.path());

    let event = prompt("do the thing");
    store.write(&event, 1_000).unwrap();
    store.write(&event, 1_500).unwrap();
    store.write(&event, 2_000).unwrap();

    let read = store.read_session(SESSION).unwrap();
    assert_eq!(read.accepted.len(), 1, "identical events collapse to one");
    assert_eq!(read.duplicates, 2);
}

#[test]
fn a_truncated_segment_is_quarantined_rather_than_dropped_or_trusted() {
    let temp = tempfile::tempdir().unwrap();
    let store = segments(temp.path());
    store.write(&prompt("intent"), 1_000).unwrap();
    store.write(&tool("cargo test"), 2_000).unwrap();

    // Simulate a process killed mid-write by truncating one segment on disk.
    let directory = store.session_directory(SESSION);
    let mut files: Vec<_> = std::fs::read_dir(&directory)
        .unwrap()
        .map(|entry| entry.unwrap().path())
        .collect();
    files.sort();
    std::fs::write(&files[1], b"{\"segment_version\":1,\"pay").unwrap();

    let read = store.read_session(SESSION).unwrap();
    assert_eq!(read.accepted.len(), 1);
    assert_eq!(read.quarantined.len(), 1);
    assert_eq!(read.quarantined[0].reason, "segment_malformed");
}

#[test]
fn a_corrupted_checksum_is_quarantined() {
    let temp = tempfile::tempdir().unwrap();
    let store = segments(temp.path());
    store.write(&prompt("intent"), 1_000).unwrap();

    let directory = store.session_directory(SESSION);
    let path = std::fs::read_dir(&directory)
        .unwrap()
        .next()
        .unwrap()
        .unwrap()
        .path();
    let mut document: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(&path).unwrap()).unwrap();
    document["payload"]["cwd"] = serde_json::json!("/tmp/somewhere-else");
    std::fs::write(&path, document.to_string()).unwrap();

    let read = store.read_session(SESSION).unwrap();
    assert!(read.accepted.is_empty());
    assert_eq!(read.quarantined[0].reason, "segment_checksum_mismatch");
}

#[test]
fn a_leftover_temporary_file_is_ignored() {
    let temp = tempfile::tempdir().unwrap();
    let store = segments(temp.path());
    store.write(&prompt("intent"), 1_000).unwrap();

    let directory = store.session_directory(SESSION);
    std::fs::write(directory.join(".tmp-interrupted"), b"half a record").unwrap();

    let read = store.read_session(SESSION).unwrap();
    assert_eq!(read.accepted.len(), 1);
    assert!(read.quarantined.is_empty(), "{:?}", read.quarantined);
}

#[test]
fn distinct_sessions_never_merge() {
    let temp = tempfile::tempdir().unwrap();
    let store = segments(temp.path());

    store.write(&prompt("first"), 1_000).unwrap();

    let mut other = prompt("second");
    other.session_key = "99999999-0000-0000-0000-000000000000".to_owned();
    store.write(&other, 1_100).unwrap();

    assert_eq!(store.read_session(SESSION).unwrap().accepted.len(), 1);
    assert_eq!(
        store
            .read_session("99999999-0000-0000-0000-000000000000")
            .unwrap()
            .accepted
            .len(),
        1
    );
    assert_eq!(store.sessions().unwrap().len(), 2);
}

#[test]
fn a_session_key_that_is_not_a_plain_identifier_is_refused() {
    let temp = tempfile::tempdir().unwrap();
    let store = segments(temp.path());

    let mut hostile = prompt("intent");
    hostile.session_key = "../../escape".to_owned();
    let error = store.write(&hostile, 1_000).unwrap_err();
    assert!(
        matches!(error, SegmentWriteError::UnsafeSessionKey { .. }),
        "{error:?}"
    );
    assert_eq!(error.code(), "segment_unsafe_session_key");
}

#[test]
fn active_duration_sums_prompt_to_stop_intervals_and_excludes_idle_time() {
    let temp = tempfile::tempdir().unwrap();
    let store = segments(temp.path());

    // Turn one: 1s of work. Then the user goes to lunch for an hour. Turn two: 2s of work.
    store.write(&prompt("first"), 10_000).unwrap();
    store.write(&tool("cargo test"), 10_500).unwrap();
    store.write(&stop(), 11_000).unwrap();
    store.write(&prompt("second"), 3_611_000).unwrap();
    store.write(&stop(), 3_613_000).unwrap();
    store.write(&session_end(), 3_613_100).unwrap();

    let recorder = Recorder::new(store);
    let aggregate = recorder.aggregate(SESSION).unwrap();

    assert_eq!(aggregate.active_duration_ms, 3_000);
    assert_eq!(aggregate.wall_duration_ms, 3_603_100);
    assert_eq!(aggregate.turns, 2);
    assert_eq!(aggregate.tool_calls, 1);
    assert!(aggregate.complete, "SessionEnd was observed");
    assert_eq!(aggregate.intent, "first");
}

#[test]
fn an_unfinished_session_is_reported_as_incomplete_rather_than_finalized() {
    let temp = tempfile::tempdir().unwrap();
    let store = segments(temp.path());
    store.write(&prompt("intent"), 1_000).unwrap();
    store.write(&tool("cargo test"), 1_500).unwrap();

    let aggregate = Recorder::new(store).aggregate(SESSION).unwrap();
    assert!(!aggregate.complete);
    // An open turn still counts the work observed so far.
    assert_eq!(aggregate.turns, 1);
}

#[test]
fn out_of_order_arrival_is_tolerated_because_order_keys_decide() {
    let temp = tempfile::tempdir().unwrap();
    let store = segments(temp.path());

    // The Stop hook lands before the tool result it followed.
    store.write(&prompt("intent"), 1_000).unwrap();
    store.write(&stop(), 3_000).unwrap();
    store.write(&tool("cargo test"), 2_000).unwrap();
    store.write(&session_end(), 4_000).unwrap();

    let aggregate = Recorder::new(store).aggregate(SESSION).unwrap();
    assert_eq!(aggregate.active_duration_ms, 2_000);
    assert_eq!(aggregate.tool_calls, 1);
}

#[test]
fn ingestion_stays_fast_enough_for_the_hook_path() {
    let temp = tempfile::tempdir().unwrap();
    let store = segments(temp.path());
    let event = tool("cargo test --workspace");

    // Warm up the filesystem cache, then measure.
    for index in 0..50 {
        store.write(&event, 1_000 + index).unwrap();
    }

    let mut timings = Vec::with_capacity(1_000);
    for index in 0..1_000_i64 {
        let started = std::time::Instant::now();
        store.write(&event, 100_000 + index).unwrap();
        timings.push(started.elapsed());
    }
    timings.sort_unstable();

    let p95 = timings[949];
    assert!(
        p95 < std::time::Duration::from_millis(50),
        "p95 segment write was {p95:?}"
    );
}

#[test]
fn finalizing_writes_one_trajectory_and_is_idempotent() {
    use agent_jit_engine::process::ProcessRunner;
    use agent_jit_engine::recorder::finalize;
    use agent_jit_engine::repository::discover;
    use agent_jit_store::{Store, StorePath};

    let temp = tempfile::tempdir().unwrap();
    let repository = temp.path().join("repo");
    std::fs::create_dir_all(&repository).unwrap();
    for argv in [
        vec!["init", "--initial-branch=main"],
        vec!["config", "user.name", "test"],
        vec!["config", "user.email", "test@example.com"],
    ] {
        assert!(
            std::process::Command::new("git")
                .args(&argv)
                .current_dir(&repository)
                .status()
                .unwrap()
                .success()
        );
    }
    std::fs::write(repository.join("README.md"), "seed\n").unwrap();
    for argv in [
        vec!["add", "README.md"],
        vec!["commit", "-m", "seed", "--no-gpg-sign"],
    ] {
        assert!(
            std::process::Command::new("git")
                .args(&argv)
                .current_dir(&repository)
                .status()
                .unwrap()
                .success()
        );
    }

    let store_directory = temp.path().join("state");
    std::fs::create_dir_all(&store_directory).unwrap();
    let mut store =
        Store::open(&StorePath::new(&store_directory.join("state.sqlite3")).unwrap()).unwrap();
    store.migrate().unwrap();

    let segment_store = segments(&temp.path().join("segments"));
    segment_store
        .write(&prompt("validate the change"), 10_000)
        .unwrap();
    segment_store.write(&tool("cargo test"), 10_500).unwrap();
    segment_store.write(&stop(), 11_000).unwrap();
    segment_store.write(&session_end(), 11_100).unwrap();

    let identity = discover(&ProcessRunner::new(), &repository).unwrap();
    let aggregate = Recorder::new(segment_store).aggregate(SESSION).unwrap();

    let first = finalize(&mut store, &aggregate, &identity, "agent-jit/0.1.0").unwrap();
    assert!(first.created);
    assert_eq!(first.events_written, 4);

    let second = finalize(&mut store, &aggregate, &identity, "agent-jit/0.1.0").unwrap();
    assert!(!second.created, "finalizing twice must not duplicate work");
    assert_eq!(second.trajectory_id, first.trajectory_id);

    assert_eq!(store.count_trajectories(&identity.repo_id).unwrap(), 1);
    let trajectory = store.get_trajectory(&first.trajectory_id).unwrap().unwrap();
    assert_eq!(trajectory.body().intent, "validate the change");
    assert_eq!(trajectory.body().duration_ms, 1_000);
    assert!(
        trajectory.body().outcome_id.is_none(),
        "the outcome is annotated by a human, never guessed"
    );
}

#[test]
fn an_open_session_is_refused_by_finalization() {
    use agent_jit_engine::recorder::finalize;
    use agent_jit_engine::repository::RepositoryIdentity;
    use agent_jit_store::{Store, StorePath};

    let temp = tempfile::tempdir().unwrap();
    let mut store =
        Store::open(&StorePath::new(&temp.path().join("state.sqlite3")).unwrap()).unwrap();
    store.migrate().unwrap();

    let store_segments = segments(&temp.path().join("segments"));
    store_segments.write(&prompt("intent"), 1_000).unwrap();
    let aggregate = Recorder::new(store_segments).aggregate(SESSION).unwrap();

    let identity = RepositoryIdentity {
        repo_id: agent_jit_domain::ids::RepositoryId::from_body("01J0000000000000000000000R")
            .unwrap(),
        git_common_dir: temp.path().join(".git"),
        worktree_root: temp.path().to_path_buf(),
        head_commit: "0".repeat(40),
    };

    let error = finalize(&mut store, &aggregate, &identity, "agent-jit/0.1.0").unwrap_err();
    assert_eq!(error.code(), "recorder_session_incomplete");
}

#[test]
fn two_identical_events_in_one_session_are_two_distinct_records() {
    // A multi-turn session produces byte-identical `Stop` payloads, one per turn. They are two
    // occurrences, not one duplicate, so they must survive as two events with distinct identities.
    use agent_jit_engine::process::ProcessRunner;
    use agent_jit_engine::recorder::finalize;
    use agent_jit_engine::repository::discover;
    use agent_jit_store::{Store, StorePath};

    let temp = tempfile::tempdir().unwrap();
    let repository = temp.path().join("repo");
    std::fs::create_dir_all(&repository).unwrap();
    for argv in [
        vec!["init", "--initial-branch=main"],
        vec!["config", "user.name", "test"],
        vec!["config", "user.email", "test@example.com"],
    ] {
        assert!(
            std::process::Command::new("git")
                .args(&argv)
                .current_dir(&repository)
                .status()
                .unwrap()
                .success()
        );
    }
    std::fs::write(repository.join("README.md"), "seed\n").unwrap();
    for argv in [
        vec!["add", "README.md"],
        vec!["commit", "-m", "seed", "--no-gpg-sign"],
    ] {
        assert!(
            std::process::Command::new("git")
                .args(&argv)
                .current_dir(&repository)
                .status()
                .unwrap()
                .success()
        );
    }

    let mut store =
        Store::open(&StorePath::new(&temp.path().join("state.sqlite3")).unwrap()).unwrap();
    store.migrate().unwrap();

    let segment_store = segments(&temp.path().join("segments"));
    segment_store.write(&prompt("first"), 1_000).unwrap();
    segment_store.write(&stop(), 2_000).unwrap();
    segment_store.write(&prompt("second"), 3_000).unwrap();
    // Byte-identical to the first Stop, and correctly a separate occurrence.
    segment_store.write(&stop(), 4_000).unwrap();
    segment_store.write(&session_end(), 5_000).unwrap();

    let identity = discover(&ProcessRunner::new(), &repository).unwrap();
    let aggregate = Recorder::new(segment_store).aggregate(SESSION).unwrap();
    assert_eq!(aggregate.turns, 2);

    let report = finalize(&mut store, &aggregate, &identity, "agent-jit/0.1.0").unwrap();
    assert_eq!(
        report.events_written, 5,
        "every occurrence is its own event"
    );

    let trajectory = store
        .get_trajectory(&report.trajectory_id)
        .unwrap()
        .unwrap();
    let mut ids: Vec<String> = trajectory
        .body()
        .events
        .iter()
        .map(ToString::to_string)
        .collect();
    let count = ids.len();
    ids.sort();
    ids.dedup();
    assert_eq!(ids.len(), count, "event identifiers must be unique");
}
