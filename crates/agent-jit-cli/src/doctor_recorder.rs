//! Recorder prerequisite checks without adding diagnostic trajectories to the corpus.

use std::fs;
use std::io::Write as _;
use std::os::unix::fs::PermissionsExt as _;
use std::path::Path;
use std::process::{Command, Stdio};

use agent_jit_domain::canonical::Digest;
use agent_jit_engine::host::HostSupport;
use agent_jit_engine::recorder::SegmentStore;
use agent_jit_store::{Store, StorePath};
use serde_json::{Value, json};

use crate::app::AppPaths;
use crate::error::CommandError;
use crate::output::Rendered;

pub fn run(as_json: bool) -> Result<Rendered, CommandError> {
    let mut checks = serde_json::Map::new();
    checks.insert(
        "host".into(),
        check(
            HostSupport::current()
                .map(|_| json!({}))
                .map_err(|_| "host_unsupported".into()),
        ),
    );
    checks.insert("git".into(), check(version("git", "git version ")));
    checks.insert("claude".into(), check(version("claude", "")));
    match AppPaths::resolve() {
        Ok(paths) => {
            checks.insert(
                "paths".into(),
                check(private_tree(&paths.home).map(|()| json!({}))),
            );
            checks.insert("store".into(), check(store_check(&paths)));
            checks.insert("plugin".into(), check(plugin_check()));
            checks.insert("hook_round_trip".into(), check(round_trip(&paths)));
        }
        Err(error) => {
            for name in ["paths", "store", "plugin", "hook_round_trip"] {
                checks.insert(name.into(), check(Err(error.code.clone())));
            }
        }
    }
    let healthy = checks.values().all(|value| value["status"] == "pass");
    let report =
        json!({"scope": "recorder", "status": if healthy {"pass"} else {"fail"}, "checks": checks});
    if !healthy {
        return Err(
            CommandError::refused("recorder_unhealthy", "recorder checks failed")
                .with_details(report),
        );
    }
    if as_json {
        Ok(Rendered::Json(report))
    } else {
        Ok(Rendered::Text("recorder: pass\n".into()))
    }
}

fn check(result: Result<Value, String>) -> Value {
    match result {
        Ok(details) => json!({"status": "pass", "details": details}),
        Err(code) => json!({"status": "fail", "code": code}),
    }
}

fn version(executable: &str, prefix: &str) -> Result<Value, String> {
    let output = Command::new(executable)
        .arg("--version")
        .stdin(Stdio::null())
        .output()
        .map_err(|_| "executable_unavailable".to_owned())?;
    if !output.status.success() || output.stdout.len() > 1024 {
        return Err("version_probe_failed".into());
    }
    let text = std::str::from_utf8(&output.stdout).map_err(|_| "version_invalid")?;
    let number = text
        .trim()
        .strip_prefix(prefix)
        .and_then(|text| text.split_whitespace().next())
        .ok_or("version_invalid")?;
    let parts: Vec<_> = number.split('.').collect();
    if parts.len() < 3 || !parts.iter().take(3).all(|part| part.parse::<u32>().is_ok()) {
        return Err("version_invalid".into());
    }
    Ok(json!({"version": number}))
}

pub(super) fn runtime_version(executable: &str) -> Result<String, String> {
    let report = version(executable, "")?;
    report["version"]
        .as_str()
        .map(str::to_owned)
        .ok_or_else(|| "version_invalid".into())
}

fn private_tree(path: &Path) -> Result<(), String> {
    let metadata = fs::symlink_metadata(path).map_err(|_| "private_path_unavailable")?;
    let expected = if metadata.is_dir() { 0o700 } else { 0o600 };
    if metadata.file_type().is_symlink() || metadata.permissions().mode() & 0o777 != expected {
        return Err("private_permissions_invalid".into());
    }
    if metadata.is_dir() {
        for entry in fs::read_dir(path).map_err(|_| "private_path_unreadable")? {
            private_tree(&entry.map_err(|_| "private_path_unreadable")?.path())?;
        }
    }
    Ok(())
}

fn store_check(paths: &AppPaths) -> Result<Value, String> {
    private_tree(&paths.data)?;
    let _probe = tempfile::NamedTempFile::new_in(&paths.data).map_err(|_| "store_unwritable")?;
    let path = StorePath::new(&paths.state_db).map_err(|error| error.code().to_owned())?;
    let store = Store::open(&path).map_err(|error| error.code().to_owned())?;
    let health = store.check().map_err(|error| error.code().to_owned())?;
    if !health.healthy || health.schema_version == 0 {
        return Err("store_unhealthy".into());
    }
    Ok(json!({"schema_version": health.schema_version, "integrity": health.integrity}))
}

fn plugin_check() -> Result<Value, String> {
    // Materializing first lets the doctor run against fresh state: the plugin it
    // verifies is the one the launcher would build, never an older leftover.
    let rendered =
        crate::claude::run(&["materialize".into(), "--json".into()]).map_err(|error| error.code)?;
    let Rendered::Json(report) = rendered else {
        return Err("plugin_report_invalid".into());
    };
    let directory = Path::new(
        report["plugin_dir"]
            .as_str()
            .ok_or("plugin_report_invalid")?,
    );
    private_tree(directory)?;
    for (relative, expected) in report["files"].as_object().ok_or("plugin_report_invalid")? {
        let bytes = fs::read(directory.join(relative)).map_err(|_| "plugin_incomplete")?;
        if expected.as_str() != Some(Digest::of_bytes(&bytes).to_string().as_str()) {
            return Err("plugin_integrity_failed".into());
        }
    }
    Ok(json!({"files_checked": 3}))
}

fn round_trip(paths: &AppPaths) -> Result<Value, String> {
    private_tree(&paths.spool)?;
    for entry in fs::read_dir(&paths.spool).map_err(|_| "spool_unreadable")? {
        let entry = entry.map_err(|_| "spool_unreadable")?;
        if entry.file_name().to_string_lossy().starts_with("health.")
            && entry.metadata().map_err(|_| "spool_unreadable")?.len() > 0
        {
            return Err("recorder_health_fault".into());
        }
    }
    let probe = tempfile::Builder::new()
        .prefix("doctor-")
        .tempdir_in(&paths.spool)
        .map_err(|_| "hook_probe_unwritable")?;
    let output = Command::new(std::env::current_exe().map_err(|_| "binary_unavailable")?)
        .env(crate::app::HOME_OVERRIDE, probe.path())
        .args(["hook", "ingest", "--event", "session-start", "--claude-version", "2.0.0"])
        .stdin(Stdio::piped()).stdout(Stdio::piped()).stderr(Stdio::piped()).spawn()
        .and_then(|mut child| {
            if let Some(mut stdin) = child.stdin.take() {
                stdin.write_all(br#"{"session_id":"doctor-probe","cwd":"/tmp","hook_event_name":"SessionStart","source":"startup"}"#)?;
            }
            child.wait_with_output()
        }).map_err(|_| "hook_probe_failed")?;
    if !output.status.success() || !output.stdout.is_empty() || !output.stderr.is_empty() {
        return Err("hook_probe_failed".into());
    }
    let segments = SegmentStore::open(&probe.path().join("cache/spool/segments"))
        .map_err(|error| error.code().to_owned())?;
    let read = segments
        .read_session("doctor-probe")
        .map_err(|error| error.code().to_owned())?;
    if read.accepted.len() != 1 || !read.quarantined.is_empty() {
        return Err("hook_round_trip_missing".into());
    }
    private_tree(probe.path())?;
    probe.close().map_err(|_| "hook_probe_cleanup_failed")?;
    Ok(json!({"events": 1, "stdout_bytes": 0, "trajectory_created": false}))
}
