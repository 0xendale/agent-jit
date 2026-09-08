#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
//! Real `OpenCode` degradation when recorder storage is unavailable.

mod corpus_support;
mod opencode_smoke_support;

use std::os::unix::fs::PermissionsExt as _;
use std::time::Duration;

use corpus_support::{bin, init_repo, json};
use opencode_smoke_support::{configured_model, installed_opencode, skip};

#[test]
fn real_opencode_finishes_silently_when_recorder_storage_is_unwritable() {
    if installed_opencode().is_none() {
        skip("opencode executable unavailable; real failure path cannot run here");
        return;
    }
    let Some(model) = configured_model() else {
        skip("AGENT_JIT_OPENCODE_TEST_MODEL is unset; real provider failure path cannot run here");
        return;
    };

    // Given: healthy state whose segment store can no longer be written.
    let home = tempfile::tempdir().unwrap();
    let repo = tempfile::tempdir().unwrap();
    init_repo(repo.path());
    bin(home.path())
        .args(["store", "migrate", "--json"])
        .assert()
        .success();
    bin(home.path())
        .args(["opencode", "materialize", "--json"])
        .assert()
        .success();
    let segments = home.path().join("cache/spool/segments");
    std::fs::create_dir_all(&segments).unwrap();
    std::fs::set_permissions(&segments, std::fs::Permissions::from_mode(0o500)).unwrap();

    // When: a real session runs against the unwritable recorder.
    bin(home.path())
        .timeout(Duration::from_secs(240))
        .args([
            "opencode",
            "run",
            "--repo",
            repo.path().to_str().unwrap(),
            "--",
            "run",
            "--format",
            "json",
            "--model",
            model.as_str(),
            "Reply with exactly: ok",
        ])
        .assert()
        .success();

    // Then: no trajectory is claimed from the failed recording.
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
