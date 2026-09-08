//! `agent-jit opencode` — materialize the recorder bridge and launch `OpenCode` with it.
//!
//! agent-jit never edits the user's `OpenCode` configuration. `~/.config/opencode/`, a project's
//! `.opencode/`, `PATH`, and shell startup files are all left exactly as they are: the bridge is
//! rendered into private application data and handed to `OpenCode` through one `OPENCODE_CONFIG`
//! environment variable pointing at a config fragment agent-jit owns. The runtime's argv is
//! never rewritten — injection is environment-only. Uninstalling is deleting a directory
//! agent-jit owns, and a user who stops running `agent-jit opencode run` is immediately back to
//! an uninstrumented `OpenCode`.
//!
//! `OpenCode`'s plugin runtime cannot observe its own process exit, so the launcher synthesizes
//! the session-end event for every session the bridge marked, after the runtime has exited.

use std::collections::BTreeMap;
use std::io::Write as _;
use std::os::unix::fs::{DirBuilderExt as _, OpenOptionsExt as _};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

use agent_jit_domain::canonical::Digest;
use agent_jit_engine::process::ProcessRunner;
use agent_jit_engine::repository::discover;
use serde_json::json;

use crate::app::AppPaths;
use crate::claude_args::{parse_flags, parse_run_arguments};
use crate::error::{CommandError, ExitClass};
use crate::output::Rendered;

pub(super) const USAGE: &str = "usage: agent-jit opencode <materialize [--json] | \
                     run --repo <path> [--opencode-bin <path>] [--json] -- <opencode-args...> | \
                     uninstall [--json]>";

/// Placeholder replaced with the absolute path of the running binary inside the bridge.
const BIN_PLACEHOLDER: &str = "{{AGENT_JIT_BIN}}";

/// Placeholder replaced with the absolute bridge path inside the config fragment.
const BRIDGE_PATH_PLACEHOLDER: &str = "{{AGENT_JIT_BRIDGE_PATH}}";

/// Directory, under private application data, holding materialized bridge versions.
const PLUGIN_ROOT: &str = "opencode-plugin";

/// Prefix for a version directory being assembled. Never treated as an active version.
const STAGING_PREFIX: &str = ".staging-";

/// The bridge templates, compiled in so a materialized bridge matches the build exactly.
const TEMPLATES: [(&str, &str); 2] = [
    (
        "bridge.ts",
        include_str!("../../../integrations/opencode/bridge.ts"),
    ),
    (
        "opencode.json",
        include_str!("../../../integrations/opencode/opencode.json"),
    ),
];

/// Dispatches an `opencode` subcommand.
///
/// # Errors
///
/// Returns a [`CommandError`] for usage mistakes, a refused repository, or a failed launch.
pub fn run(args: &[String]) -> Result<Rendered, CommandError> {
    match args.first().map(String::as_str) {
        Some("materialize") => materialize_command(&args[1..]),
        Some("run") => run_command(&args[1..]),
        Some("uninstall") => uninstall_command(&args[1..]),
        Some(other) => Err(CommandError::usage(format!(
            "unknown opencode subcommand: {other}\n{USAGE}"
        ))),
        None => Err(CommandError::usage(USAGE)),
    }
}

/// A materialized bridge.
struct Materialized {
    directory: PathBuf,
    files: BTreeMap<String, String>,
}

/// Renders the bridge into private application data and returns where it landed.
fn materialize(paths: &AppPaths) -> Result<Materialized, CommandError> {
    let binary = current_binary()?;
    let [(_, bridge_template), (_, config_template)] = TEMPLATES;
    let bridge = render_binary_path(bridge_template, &binary);

    // The version is derived from the binary-rendered bridge plus the untouched config template:
    // the config names the bridge by its versioned path, so the version must be computed from
    // inputs that do not yet include it. Identical inputs still land in the same directory.
    let version = version_of(&bridge, config_template);
    let root = paths.data.join(PLUGIN_ROOT);
    let destination = root.join(&version);
    let config = render_bridge_path(config_template, &destination.join("bridge.ts"));

    let mut files = BTreeMap::new();
    files.insert(
        "bridge.ts".to_owned(),
        Digest::of_bytes(bridge.as_bytes()).to_string(),
    );
    files.insert(
        "opencode.json".to_owned(),
        Digest::of_bytes(config.as_bytes()).to_string(),
    );

    if destination.join("bridge.ts").exists() {
        return Ok(Materialized {
            directory: destination,
            files,
        });
    }

    // Assemble under a staging name, then move the finished directory into place: an interrupted
    // run leaves a staging directory, which is never a version, rather than a half-built bridge.
    let staging = root.join(format!("{STAGING_PREFIX}{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&staging);
    create_dir(&staging)?;

    for (relative, contents) in [
        ("bridge.ts", bridge.as_str()),
        ("opencode.json", config.as_str()),
    ] {
        let path = staging.join(relative);
        std::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .mode(0o600)
            .open(&path)
            .and_then(|mut file| file.write_all(contents.as_bytes()))
            .map_err(|error| {
                CommandError::new(
                    "opencode_plugin_unwritable",
                    format!("`{}`: {error}", path.display()),
                    ExitClass::Internal,
                )
            })?;
    }

    create_dir(&root)?;
    std::fs::rename(&staging, &destination).map_err(|error| {
        let _ = std::fs::remove_dir_all(&staging);
        CommandError::new(
            "opencode_plugin_unwritable",
            format!("`{}`: {error}", destination.display()),
            ExitClass::Internal,
        )
    })?;

    Ok(Materialized {
        directory: destination,
        files,
    })
}

/// Substitutes the binary path into the bridge's TypeScript string literal.
///
/// The placeholder sits inside a string literal, so a path containing quotes or backslashes is
/// escaped by `serde_json` — escapes TypeScript accepts unchanged. The path stays exactly one
/// string — and therefore exactly one argv element — no matter what bytes it contains.
fn render_binary_path(template: &str, binary: &Path) -> String {
    let escaped = serde_json::to_string(&binary.to_string_lossy())
        .unwrap_or_else(|_| "\"agent-jit\"".to_owned());
    let inner = escaped.trim_matches('"');
    template.replace(BIN_PLACEHOLDER, inner)
}

/// Substitutes the bridge path into the config fragment's JSON string value.
fn render_bridge_path(template: &str, bridge: &Path) -> String {
    let escaped = serde_json::to_string(&bridge.to_string_lossy())
        .unwrap_or_else(|_| "\"bridge.ts\"".to_owned());
    let inner = escaped.trim_matches('"');
    template.replace(BRIDGE_PATH_PLACEHOLDER, inner)
}

/// Derives the version directory name from the inputs that do not depend on it.
fn version_of(bridge: &str, config_template: &str) -> String {
    let manifest = json!({
        "bridge.ts": Digest::of_bytes(bridge.as_bytes()).to_string(),
        "opencode.json": Digest::of_bytes(config_template.as_bytes()).to_string(),
    });
    let digest = Digest::of_bytes(
        serde_json::to_string(&manifest)
            .unwrap_or_default()
            .as_bytes(),
    )
    .to_string();
    format!("v{}", &digest[..16])
}

/// Returns the absolute path of the running binary.
fn current_binary() -> Result<PathBuf, CommandError> {
    std::env::current_exe().map_err(|error| {
        CommandError::new(
            "opencode_binary_unresolvable",
            format!("could not resolve the running binary: {error}"),
            ExitClass::Internal,
        )
    })
}

/// Creates a directory, reporting a typed error.
fn create_dir(path: &Path) -> Result<(), CommandError> {
    std::fs::DirBuilder::new()
        .recursive(true)
        .mode(0o700)
        .create(path)
        .map_err(|error| {
            CommandError::new(
                "opencode_plugin_unwritable",
                format!("`{}`: {error}", path.display()),
                ExitClass::Internal,
            )
        })
}

/// `agent-jit opencode materialize`.
fn materialize_command(args: &[String]) -> Result<Rendered, CommandError> {
    let as_json = parse_flags(args, USAGE)?;
    let paths = AppPaths::resolve()?;
    paths.ensure()?;
    let materialized = materialize(&paths)?;

    let report = json!({
        "plugin_dir": materialized.directory.to_string_lossy(),
        "files": materialized.files,
    });

    if as_json {
        return Ok(Rendered::Json(report));
    }
    Ok(Rendered::Text(format!(
        "materialized {} file(s) into {}\n",
        materialized.files.len(),
        materialized.directory.display()
    )))
}

/// `agent-jit opencode run`.
fn run_command(args: &[String]) -> Result<Rendered, CommandError> {
    let (repo, opencode_bin, as_json, opencode_args) =
        parse_run_arguments(args, "--opencode-bin", USAGE)?;

    // The repository is validated before `OpenCode` is launched: recording against something that
    // is not the repository the operator named would produce evidence nobody can attribute.
    let identity = discover(&ProcessRunner::new(), Path::new(&repo)).map_err(|error| {
        CommandError::new(error.code(), error.to_string(), ExitClass::SafetyRefusal)
    })?;

    let paths = AppPaths::resolve()?;
    paths.ensure()?;
    let materialized = materialize(&paths)?;

    let executable = opencode_bin.unwrap_or_else(|| "opencode".to_owned());
    let mut command = Command::new(&executable);
    let runtime_version = crate::doctor_recorder::runtime_version(&executable).ok();
    if let Some(version) = &runtime_version {
        command.env("AGENT_JIT_RUNTIME_VERSION", version);
    } else {
        command.env_remove("AGENT_JIT_RUNTIME_VERSION");
    }
    command
        .current_dir(&identity.worktree_root)
        .env("PWD", &identity.worktree_root)
        // Every argument the operator passed, in order. Injection is environment-only: the
        // runtime's argv is never rewritten.
        .args(&opencode_args)
        // The bridge needs the same private state the launcher resolved, and `OpenCode` needs the
        // config fragment that loads the bridge — without touching any user or project config.
        .env(crate::app::HOME_OVERRIDE, &paths.home)
        .env("AGENT_JIT_HOOK_ADAPTER", "opencode")
        .env(
            "OPENCODE_CONFIG",
            materialized.directory.join("opencode.json"),
        );

    let status = command.status().map_err(|error| {
        CommandError::new(
            "opencode_launch_failed",
            format!("could not run `{executable}`: {error}"),
            ExitClass::Internal,
        )
    })?;

    let sessions_ended =
        synthesize_session_ends(&paths, &materialized.directory, runtime_version.as_deref());

    let report = json!({
        "repository_id": identity.repo_id.to_string(),
        "worktree_root": identity.worktree_root.to_string_lossy(),
        "plugin_dir": materialized.directory.to_string_lossy(),
        "opencode_bin": executable,
        "exit_code": status.code(),
        "sessions_ended": sessions_ended,
    });

    if !status.success() {
        return Err(CommandError::new(
            "opencode_exited_nonzero",
            format!(
                "`{executable}` exited with {}",
                status
                    .code()
                    .map_or_else(|| "a signal".to_owned(), |code| code.to_string())
            ),
            ExitClass::Internal,
        )
        .with_details(report));
    }

    if as_json {
        return Ok(Rendered::Json(report));
    }
    Ok(Rendered::Text(String::new()))
}

/// Emits the session-end event for every session the bridge marked, then consumes the marker.
///
/// A plugin cannot observe its own process exit, so the launcher is the only party that knows
/// the runtime is gone. Markers whose ingestion fails are kept: the next run retries them, and
/// the recorder deduplicates by canonical digest.
fn synthesize_session_ends(
    paths: &AppPaths,
    plugin_dir: &Path,
    runtime_version: Option<&str>,
) -> usize {
    let sessions = plugin_dir.join("sessions");
    let Ok(entries) = std::fs::read_dir(&sessions) else {
        return 0;
    };

    let mut ended = 0;
    for entry in entries.flatten() {
        let marker = entry.path();
        if marker
            .extension()
            .is_none_or(|extension| extension != "json")
        {
            continue;
        }
        let Some(session_id) = marker
            .file_stem()
            .map(|stem| stem.to_string_lossy().to_string())
        else {
            continue;
        };
        let Ok(contents) = std::fs::read_to_string(&marker) else {
            continue;
        };
        let Ok(session) = serde_json::from_str::<serde_json::Value>(&contents) else {
            continue;
        };
        let Some(directory) = session.get("directory").and_then(serde_json::Value::as_str) else {
            continue;
        };
        let session_version = session
            .get("runtime_version")
            .and_then(serde_json::Value::as_str)
            .or(runtime_version);

        if deliver_session_end(paths, &session_id, directory, session_version) {
            // The marker is consumed only after a confirmed delivery.
            let _ = std::fs::remove_file(&marker);
            ended += 1;
        }
    }
    ended
}

/// Feeds one synthesized session-end event to the recorder and waits for its verdict.
fn deliver_session_end(
    paths: &AppPaths,
    session_id: &str,
    directory: &str,
    runtime_version: Option<&str>,
) -> bool {
    let payload = json!({
        "event": "session.ended",
        "session_id": session_id,
        "directory": directory,
        "reason": "process_exit",
    })
    .to_string();

    let attempt = (|| -> std::io::Result<std::process::ExitStatus> {
        let mut command =
            Command::new(std::env::current_exe().unwrap_or_else(|_| PathBuf::from("agent-jit")));
        command
            .env(crate::app::HOME_OVERRIDE, &paths.home)
            .env("AGENT_JIT_HOOK_ADAPTER", "opencode")
            .args(["hook", "ingest", "--event", "session-end"])
            .stdin(Stdio::piped())
            .stdout(Stdio::null())
            .stderr(Stdio::null());
        if let Some(version) = runtime_version {
            command.env("AGENT_JIT_RUNTIME_VERSION", version);
        } else {
            command.env_remove("AGENT_JIT_RUNTIME_VERSION");
        }
        let mut child = command.spawn()?;
        if let Some(mut stdin) = child.stdin.take() {
            stdin.write_all(payload.as_bytes())?;
        }
        child.wait()
    })();

    matches!(attempt, Ok(status) if status.success())
}

/// `agent-jit opencode uninstall`.
fn uninstall_command(args: &[String]) -> Result<Rendered, CommandError> {
    let as_json = parse_flags(args, USAGE)?;
    let paths = AppPaths::resolve()?;
    let root = paths.data.join(PLUGIN_ROOT);

    // Only the directory agent-jit generated is removed. Recorded state is untouched:
    // uninstalling an integration must never destroy evidence.
    let removed = root.exists();
    if removed {
        std::fs::remove_dir_all(&root).map_err(|error| {
            CommandError::new(
                "opencode_plugin_unremovable",
                format!("`{}`: {error}", root.display()),
                ExitClass::Internal,
            )
        })?;
    }

    let report = json!({"removed": removed});
    if as_json {
        return Ok(Rendered::Json(report));
    }
    Ok(Rendered::Text(if removed {
        "removed the generated plugin\n".to_owned()
    } else {
        "nothing to remove\n".to_owned()
    }))
}
