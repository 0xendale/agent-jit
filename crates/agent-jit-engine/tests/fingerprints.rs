#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
//! Fingerprints decide whether a compiled capability may still run, so what they include —
//! and what they refuse to include — is a safety property.

use std::path::Path;

use agent_jit_domain::fingerprint::FingerprintKind;
use agent_jit_engine::fingerprint::{FingerprintError, FingerprintRequest, compute};

fn write(root: &Path, relative: &str, contents: &str) {
    let path = root.join(relative);
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).unwrap();
    }
    std::fs::write(path, contents).unwrap();
}

fn request(paths: &[&str]) -> FingerprintRequest {
    FingerprintRequest {
        paths: paths.iter().map(|path| (*path).to_owned()).collect(),
        kind: FingerprintKind::SourcePath,
    }
}

#[test]
fn content_decides_the_digest_and_input_order_does_not() {
    let temp = tempfile::tempdir().unwrap();
    write(temp.path(), "Cargo.lock", "one\n");
    write(temp.path(), "src/main.rs", "two\n");

    let forward = compute(temp.path(), &request(&["Cargo.lock", "src/main.rs"])).unwrap();
    let reversed = compute(temp.path(), &request(&["src/main.rs", "Cargo.lock"])).unwrap();
    assert_eq!(forward.digest, reversed.digest);

    write(temp.path(), "src/main.rs", "two changed\n");
    let changed = compute(temp.path(), &request(&["Cargo.lock", "src/main.rs"])).unwrap();
    assert_ne!(forward.digest, changed.digest);
}

#[test]
fn a_timestamp_only_change_does_not_alter_the_digest() {
    let temp = tempfile::tempdir().unwrap();
    write(temp.path(), "Cargo.lock", "one\n");
    let before = compute(temp.path(), &request(&["Cargo.lock"])).unwrap();

    // Rewrite identical bytes: mtime moves, content does not.
    std::thread::sleep(std::time::Duration::from_millis(10));
    write(temp.path(), "Cargo.lock", "one\n");
    let after = compute(temp.path(), &request(&["Cargo.lock"])).unwrap();

    assert_eq!(before.digest, after.digest);
}

#[test]
fn an_absent_file_is_recorded_as_absent_rather_than_skipped() {
    let temp = tempfile::tempdir().unwrap();
    write(temp.path(), "Cargo.lock", "one\n");

    let with_absent = compute(temp.path(), &request(&["Cargo.lock", "flake.nix"])).unwrap();
    let without = compute(temp.path(), &request(&["Cargo.lock"])).unwrap();
    assert_ne!(
        with_absent.digest, without.digest,
        "an absence marker must be part of the digest"
    );

    let absent = with_absent
        .inputs
        .iter()
        .find(|input| input.key == "flake.nix")
        .unwrap();
    assert!(absent.digest.is_none());

    // Creating the file later must invalidate the fingerprint.
    write(temp.path(), "flake.nix", "{}\n");
    let now_present = compute(temp.path(), &request(&["Cargo.lock", "flake.nix"])).unwrap();
    assert_ne!(with_absent.digest, now_present.digest);
}

#[test]
fn inputs_are_sorted_by_key() {
    let temp = tempfile::tempdir().unwrap();
    write(temp.path(), "b.txt", "b\n");
    write(temp.path(), "a.txt", "a\n");

    let computed = compute(temp.path(), &request(&["b.txt", "a.txt"])).unwrap();
    let keys: Vec<&str> = computed.inputs.iter().map(|i| i.key.as_str()).collect();
    assert_eq!(keys, vec!["a.txt", "b.txt"]);
}

#[test]
fn a_path_escaping_the_repository_is_refused() {
    let temp = tempfile::tempdir().unwrap();
    write(temp.path(), "Cargo.lock", "one\n");

    for escape in ["../outside.txt", "src/../../outside.txt", "/etc/hosts"] {
        let error = compute(temp.path(), &request(&[escape])).unwrap_err();
        assert_eq!(error.code(), "fingerprint_path_escape", "{escape}");
    }
}

#[test]
fn a_symlink_pointing_outside_the_repository_is_refused() {
    let temp = tempfile::tempdir().unwrap();
    let outside = tempfile::tempdir().unwrap();
    std::fs::write(outside.path().join("secret.txt"), "secret\n").unwrap();
    write(temp.path(), "Cargo.lock", "one\n");
    std::os::unix::fs::symlink(
        outside.path().join("secret.txt"),
        temp.path().join("link.txt"),
    )
    .unwrap();

    let error = compute(temp.path(), &request(&["link.txt"])).unwrap_err();
    assert_eq!(error.code(), "fingerprint_symlink_escape");
    assert!(
        matches!(error, FingerprintError::SymlinkEscape { .. }),
        "{error:?}"
    );
}

#[test]
fn a_symlink_staying_inside_the_repository_is_followed() {
    let temp = tempfile::tempdir().unwrap();
    write(temp.path(), "real.txt", "real\n");
    std::os::unix::fs::symlink(temp.path().join("real.txt"), temp.path().join("link.txt")).unwrap();

    let computed = compute(temp.path(), &request(&["link.txt"])).unwrap();
    assert_eq!(computed.inputs.len(), 1);
    assert!(computed.inputs[0].digest.is_some());
}

#[test]
fn known_secret_files_are_not_hashed_by_default() {
    let temp = tempfile::tempdir().unwrap();
    write(temp.path(), ".env", "TOKEN=hunter2\n");

    let error = compute(temp.path(), &request(&[".env"])).unwrap_err();
    assert_eq!(error.code(), "fingerprint_secret_path");
}

#[test]
fn a_directory_is_refused_rather_than_hashed_inconsistently() {
    let temp = tempfile::tempdir().unwrap();
    std::fs::create_dir_all(temp.path().join("src")).unwrap();

    let error = compute(temp.path(), &request(&["src"])).unwrap_err();
    assert_eq!(error.code(), "fingerprint_not_a_file");
}

#[test]
fn the_kind_is_part_of_the_digest() {
    let temp = tempfile::tempdir().unwrap();
    write(temp.path(), "Cargo.lock", "one\n");

    let as_source = compute(temp.path(), &request(&["Cargo.lock"])).unwrap();
    let as_lockfile = compute(
        temp.path(),
        &FingerprintRequest {
            paths: vec!["Cargo.lock".to_owned()],
            kind: FingerprintKind::Lockfile,
        },
    )
    .unwrap();

    assert_ne!(as_source.digest, as_lockfile.digest);
}
