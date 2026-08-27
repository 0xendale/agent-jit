#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
//! `agent-jit store` migrates and checks the private database, and refuses an unsafe one.

use std::os::unix::fs::PermissionsExt as _;
use std::path::Path;

use agent_jit_store::CURRENT_SCHEMA_VERSION;
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

#[test]
fn migrate_creates_a_private_database_and_reports_the_schema_version() {
    let home = tempfile::tempdir().unwrap();
    let assert = bin(home.path())
        .args(["store", "migrate", "--json"])
        .assert()
        .success();
    let report = json(&assert.get_output().stdout);

    assert_eq!(report["from_version"], 0);
    // Version-agnostic: a new migration must not require editing this assertion.
    assert_eq!(report["to_version"], CURRENT_SCHEMA_VERSION);
    assert!(report["applied"].as_u64().unwrap() >= 1);

    let database = home.path().join("data/state.sqlite3");
    assert!(database.exists());
    let mode = std::fs::metadata(&database).unwrap().permissions().mode() & 0o777;
    assert_eq!(mode, 0o600, "database mode was {mode:o}");
}

#[test]
fn migrate_is_idempotent_and_check_reports_health() {
    let home = tempfile::tempdir().unwrap();
    bin(home.path())
        .args(["store", "migrate", "--json"])
        .assert()
        .success();

    let again = bin(home.path())
        .args(["store", "migrate", "--json"])
        .assert()
        .success();
    assert_eq!(json(&again.get_output().stdout)["applied"], 0);

    let checked = bin(home.path())
        .args(["store", "check", "--json"])
        .assert()
        .success();
    let health = json(&checked.get_output().stdout);
    assert_eq!(health["healthy"], true);
    assert_eq!(health["integrity"], "ok");
    assert_eq!(health["foreign_key_violations"], 0);
    assert_eq!(health["schema_version"], CURRENT_SCHEMA_VERSION);
}

#[test]
fn a_world_readable_database_is_refused_and_left_alone() {
    let home = tempfile::tempdir().unwrap();
    bin(home.path())
        .args(["store", "migrate", "--json"])
        .assert()
        .success();

    let database = home.path().join("data/state.sqlite3");
    std::fs::set_permissions(&database, std::fs::Permissions::from_mode(0o644)).unwrap();

    bin(home.path())
        .args(["store", "check", "--json"])
        .assert()
        .code(4)
        .stdout(contains("store_permissions_too_open"));

    let mode = std::fs::metadata(&database).unwrap().permissions().mode() & 0o777;
    assert_eq!(mode, 0o644, "the command must not silently repair the file");
}

#[test]
fn a_symlinked_database_is_refused() {
    let home = tempfile::tempdir().unwrap();
    let data = home.path().join("data");
    std::fs::create_dir_all(&data).unwrap();
    let real = home.path().join("elsewhere.sqlite3");
    std::fs::write(&real, b"").unwrap();
    std::os::unix::fs::symlink(&real, data.join("state.sqlite3")).unwrap();

    bin(home.path())
        .args(["store", "check", "--json"])
        .assert()
        .code(4)
        .stdout(contains("store_path_symlink"));
}

#[test]
fn checking_before_migrating_reports_an_unmigrated_database_rather_than_claiming_health() {
    let home = tempfile::tempdir().unwrap();
    let assert = bin(home.path())
        .args(["store", "check", "--json"])
        .assert()
        .failure();
    assert!(
        String::from_utf8(assert.get_output().stdout.clone())
            .unwrap()
            .contains("store_not_migrated")
    );
}

#[test]
fn an_unknown_store_subcommand_is_a_usage_error() {
    let home = tempfile::tempdir().unwrap();
    bin(home.path())
        .args(["store", "vacuum"])
        .assert()
        .code(2)
        .stderr(contains("usage: agent-jit store"));
}
