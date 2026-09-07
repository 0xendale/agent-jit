#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
//! The process boundary never involves a shell, and the host contract is macOS arm64 only.

use agent_jit_engine::host::{HostError, HostSupport};
use agent_jit_engine::process::{ProcessError, ProcessRunner, Runner};

#[test]
fn arguments_are_passed_as_literal_argv_elements() {
    let runner = ProcessRunner::new();
    // If this were handed to a shell, the semicolon would start a second command and the glob
    // would expand. Both must survive as one literal argument.
    let hostile = "a; rm -rf /tmp/should-not-exist && echo pwned *";
    let output = runner.run("/bin/echo", &[hostile], None).unwrap();

    assert_eq!(output.stdout.trim_end(), hostile);
    assert_eq!(output.exit_code, Some(0));
}

#[test]
fn a_missing_executable_is_a_typed_error() {
    let runner = ProcessRunner::new();
    let error = runner
        .run("/definitely/not/an/executable", &[], None)
        .unwrap_err();
    assert_eq!(error.code(), "process_spawn_failed");
    assert!(
        matches!(error, ProcessError::SpawnFailed { .. }),
        "{error:?}"
    );
}

#[test]
fn a_relative_executable_name_is_refused_so_path_lookup_cannot_decide_what_runs() {
    let runner = ProcessRunner::new();
    let error = runner.run("echo", &["hi"], None).unwrap_err();
    assert_eq!(error.code(), "process_executable_not_absolute");
}

#[test]
fn a_nonzero_exit_is_reported_rather_than_thrown_away() {
    let runner = ProcessRunner::new();
    let output = runner.run("/bin/sh", &["-c", "exit 3"], None).unwrap();
    assert_eq!(output.exit_code, Some(3));
    assert!(!output.success());
}

#[test]
fn the_environment_is_stripped_to_a_deterministic_allowlist() {
    let runner = ProcessRunner::new();
    // SAFETY-adjacent: the child must not inherit ambient secrets from this process.
    let output = runner
        .run("/usr/bin/env", &[], Some(std::path::Path::new("/")))
        .unwrap();
    let names: Vec<&str> = output
        .stdout
        .lines()
        .filter_map(|line| line.split('=').next())
        .collect();

    assert!(names.contains(&"LC_ALL"), "{names:?}");
    assert!(names.contains(&"TZ"), "{names:?}");
    for forbidden in ["HOME", "ANTHROPIC_API_KEY", "AWS_SECRET_ACCESS_KEY"] {
        assert!(!names.contains(&forbidden), "{forbidden} leaked: {names:?}");
    }
}

#[test]
fn the_child_runs_with_a_pinned_locale_and_timezone() {
    let runner = ProcessRunner::new();
    let output = runner
        .run(
            "/bin/sh",
            &["-c", "printf '%s|%s' \"$LC_ALL\" \"$TZ\""],
            None,
        )
        .unwrap();
    assert_eq!(output.stdout, "C|UTC");
}

#[test]
fn the_current_host_is_supported() {
    // The suite only runs on the supported platform; if this fails, so should everything else.
    HostSupport::current().unwrap();
}

#[test]
fn every_other_platform_is_refused_with_a_typed_error() {
    for (os, arch) in [
        ("linux", "x86_64"),
        ("linux", "aarch64"),
        ("macos", "x86_64"),
        ("windows", "aarch64"),
    ] {
        let error = HostSupport::probe(os, arch).unwrap_err();
        assert_eq!(error.code(), "host_unsupported", "{os}/{arch}");
        assert!(
            matches!(error, HostError::Unsupported { .. }),
            "{os}/{arch}: {error:?}"
        );
    }

    assert!(HostSupport::probe("macos", "aarch64").is_ok());
}
