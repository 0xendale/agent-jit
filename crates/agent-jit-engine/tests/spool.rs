#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
//! The spool is the hook's only write. It stays private, line-oriented, and bounded.

use std::os::unix::fs::PermissionsExt as _;

use agent_jit_engine::spool::{MAX_LINE_BYTES, Spool, SpoolKind};

#[test]
fn appending_creates_a_private_file_and_keeps_one_line_per_record() {
    let temp = tempfile::tempdir().unwrap();
    let spool = Spool::open(&temp.path().join("spool")).unwrap();

    spool.append(SpoolKind::Events, r#"{"a":1}"#).unwrap();
    spool.append(SpoolKind::Events, r#"{"a":2}"#).unwrap();

    let lines = spool.read_lines(SpoolKind::Events).unwrap();
    assert_eq!(lines, vec![r#"{"a":1}"#, r#"{"a":2}"#]);

    let path = spool.active_path(SpoolKind::Events).unwrap();
    let mode = std::fs::metadata(&path).unwrap().permissions().mode() & 0o777;
    assert_eq!(mode, 0o600, "spool file mode was {mode:o}");

    let directory_mode = std::fs::metadata(spool.directory())
        .unwrap()
        .permissions()
        .mode()
        & 0o777;
    assert_eq!(directory_mode, 0o700);
}

#[test]
fn events_and_health_go_to_separate_files() {
    let temp = tempfile::tempdir().unwrap();
    let spool = Spool::open(temp.path()).unwrap();

    spool.append(SpoolKind::Events, "event").unwrap();
    spool.append(SpoolKind::Health, "health").unwrap();

    assert_eq!(spool.read_lines(SpoolKind::Events).unwrap(), vec!["event"]);
    assert_eq!(spool.read_lines(SpoolKind::Health).unwrap(), vec!["health"]);
}

#[test]
fn an_embedded_newline_cannot_forge_a_second_record() {
    let temp = tempfile::tempdir().unwrap();
    let spool = Spool::open(temp.path()).unwrap();

    spool.append(SpoolKind::Events, "one\nforged").unwrap();
    let lines = spool.read_lines(SpoolKind::Events).unwrap();
    assert_eq!(lines.len(), 1, "{lines:?}");
    assert_eq!(lines[0], "one\\nforged");
}

#[test]
fn an_oversized_line_is_refused_rather_than_truncated_silently() {
    let temp = tempfile::tempdir().unwrap();
    let spool = Spool::open(temp.path()).unwrap();

    let error = spool
        .append(SpoolKind::Events, &"x".repeat(MAX_LINE_BYTES + 1))
        .unwrap_err();
    assert_eq!(error.code(), "spool_line_too_large");
    assert!(spool.read_lines(SpoolKind::Events).unwrap().is_empty());
}

#[test]
fn a_symlinked_spool_directory_is_refused() {
    let temp = tempfile::tempdir().unwrap();
    let real = temp.path().join("real");
    std::fs::create_dir_all(&real).unwrap();
    let link = temp.path().join("link");
    std::os::unix::fs::symlink(&real, &link).unwrap();

    let error = Spool::open(&link).unwrap_err();
    assert_eq!(error.code(), "spool_symlink");
}

#[test]
fn a_symlinked_spool_file_is_refused() {
    let temp = tempfile::tempdir().unwrap();
    let spool = Spool::open(temp.path()).unwrap();
    let elsewhere = temp.path().join("elsewhere.jsonl");
    std::fs::write(&elsewhere, b"").unwrap();
    std::os::unix::fs::symlink(&elsewhere, temp.path().join("events.jsonl")).unwrap();

    let error = spool.append(SpoolKind::Events, "x").unwrap_err();
    assert_eq!(error.code(), "spool_symlink");
}

#[test]
fn reopening_a_spool_appends_rather_than_replacing() {
    let temp = tempfile::tempdir().unwrap();
    Spool::open(temp.path())
        .unwrap()
        .append(SpoolKind::Events, "first")
        .unwrap();
    Spool::open(temp.path())
        .unwrap()
        .append(SpoolKind::Events, "second")
        .unwrap();

    assert_eq!(
        Spool::open(temp.path())
            .unwrap()
            .read_lines(SpoolKind::Events)
            .unwrap(),
        vec!["first", "second"]
    );
}
