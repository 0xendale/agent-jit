//! `agent-jit claude` — materialize the local plugin and launch Claude Code with it.
//!
//! agent-jit never edits the user's Claude configuration. `~/.claude/settings.json`, a project's
//! `.claude/`, `PATH`, and shell startup files are all left exactly as they are: the plugin is
//! rendered into private application data and handed to Claude explicitly with one `--plugin-dir`
//! argument. Uninstalling is therefore just deleting a directory agent-jit owns, and a user who
//! stops running `agent-jit claude run` is immediately back to an uninstrumented Claude.
//!
//! The templates are compiled into the binary, so a materialized plugin cannot drift from the
//! build that produced it.

use std::collections::BTreeMap;
use std::io::Write as _;
use std::os::unix::fs::{DirBuilderExt as _, OpenOptionsExt as _};
use std::path::{Path, PathBuf};
use std::process::Command;

use agent_jit_domain::canonical::Digest;
use agent_jit_engine::process::ProcessRunner;
use agent_jit_engine::repository::discover;
use serde_json::json;

use crate::app::AppPaths;
use crate::claude_args::{parse_flags, parse_run_arguments};
use crate::error::{CommandError, ExitClass};
use crate::output::Rendered;

pub(super) const USAGE: &str = "usage: agent-jit claude <materialize [--json] | \
                     run --repo <path> [--claude-bin <path>] [--json] -- <claude-args...> | \
                     uninstall [--json]>";

/// Placeholder replaced with the absolute path of the running binary.
const BIN_PLACEHOLDER: &str = "{{AGENT_JIT_BIN}}";

/// Directory, under private application data, holding materialized plugin versions.
const PLUGIN_ROOT: &str = "claude-plugin";

/// Prefix for a version directory being assembled. Never treated as an active version.
const STAGING_PREFIX: &str = ".staging-";

/// The plugin templates, compiled in so a materialized plugin matches the build exactly.
const TEMPLATES: &[(&str, &str)] = &[
    (
        ".claude-plugin/plugin.json",
        include_str!("../../../integrations/claude-code/.claude-plugin/plugin.json"),
    ),
    (
        "hooks/hooks.json",
        include_str!("../../../integrations/claude-code/hooks/hooks.json"),
    ),
    (
        ".mcp.json",
        include_str!("../../../integrations/claude-code/.mcp.json"),
    ),
];

/// Dispatches a `claude` subcommand.
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
            "unknown claude subcommand: {other}\n{USAGE}"
        ))),
        None => Err(CommandError::usage(USAGE)),
    }
}

/// A materialized plugin.
struct Materialized {
    directory: PathBuf,
    files: BTreeMap<String, String>,
}

/// Renders the plugin into private application data and returns where it landed.
fn materialize(paths: &AppPaths) -> Result<Materialized, CommandError> {
    let binary = current_binary()?;
    let rendered: Vec<(&str, String)> = TEMPLATES
        .iter()
        .map(|(relative, template)| (*relative, render(template, &binary)))
        .collect();

    // The version is the digest of what will be written, so identical inputs land in the same
    // directory and rematerializing is a no-op rather than a new copy.
    let version = version_of(&rendered);
    let root = paths.data.join(PLUGIN_ROOT);
    let destination = root.join(&version);

    let mut files = BTreeMap::new();
    for (relative, contents) in &rendered {
        files.insert(
            (*relative).to_owned(),
            Digest::of_bytes(contents.as_bytes()).to_string(),
        );
    }

    if destination.join(".mcp.json").exists() {
        return Ok(Materialized {
            directory: destination,
            files,
        });
    }

    // Assemble under a staging name, then move the finished directory into place: an interrupted
    // run leaves a staging directory, which is never a version, rather than a half-built plugin.
    let staging = root.join(format!("{STAGING_PREFIX}{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&staging);
    create_dir(&staging)?;

    for (relative, contents) in &rendered {
        let path = staging.join(relative);
        if let Some(parent) = path.parent() {
            create_dir(parent)?;
        }
        std::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .mode(0o600)
            .open(&path)
            .and_then(|mut file| file.write_all(contents.as_bytes()))
            .map_err(|error| {
                CommandError::new(
                    "claude_plugin_unwritable",
                    format!("`{}`: {error}", path.display()),
                    ExitClass::Internal,
                )
            })?;
    }

    create_dir(&root)?;
    std::fs::rename(&staging, &destination).map_err(|error| {
        let _ = std::fs::remove_dir_all(&staging);
        CommandError::new(
            "claude_plugin_unwritable",
            format!("`{}`: {error}", destination.display()),
            ExitClass::Internal,
        )
    })?;

    Ok(Materialized {
        directory: destination,
        files,
    })
}

/// Renders one template, substituting the binary path inside its JSON string values.
///
/// The substitution happens inside a JSON string, so a path containing spaces, quotes, or shell
/// metacharacters stays exactly one JSON string — and therefore exactly one argv element when
/// Claude runs it. `serde_json` does the escaping; this code never builds shell syntax.
fn render(template: &str, binary: &Path) -> String {
    let escaped = serde_json::to_string(&binary.to_string_lossy())
        .unwrap_or_else(|_| "\"agent-jit\"".to_owned());
    // Trim the surrounding quotes: the placeholder already sits inside a JSON string literal.
    let inner = escaped.trim_matches('"');
    template.replace(BIN_PLACEHOLDER, inner)
}

/// Derives the version directory name from the rendered content.
fn version_of(rendered: &[(&str, String)]) -> String {
    let mut manifest = serde_json::Map::new();
    for (relative, contents) in rendered {
        manifest.insert(
            (*relative).to_owned(),
            json!(Digest::of_bytes(contents.as_bytes()).to_string()),
        );
    }
    let digest = Digest::of_bytes(
        serde_json::to_string(&serde_json::Value::Object(manifest))
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
            "claude_binary_unresolvable",
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
                "claude_plugin_unwritable",
                format!("`{}`: {error}", path.display()),
                ExitClass::Internal,
            )
        })
}

/// `agent-jit claude materialize`.
fn materialize_command(args: &[String]) -> Result<Rendered, CommandError> {
    let as_json = parse_flags(args)?;
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

/// `agent-jit claude run`.
fn run_command(args: &[String]) -> Result<Rendered, CommandError> {
    let (repo, claude_bin, as_json, claude_args) = parse_run_arguments(args)?;

    // The repository is validated before Claude is launched: recording against something that is
    // not the repository the operator named would produce evidence nobody can attribute.
    let identity = discover(&ProcessRunner::new(), Path::new(&repo)).map_err(|error| {
        CommandError::new(error.code(), error.to_string(), ExitClass::SafetyRefusal)
    })?;

    let paths = AppPaths::resolve()?;
    paths.ensure()?;
    let materialized = materialize(&paths)?;

    let executable = claude_bin.unwrap_or_else(|| "claude".to_owned());
    let mut command = Command::new(&executable);
    if let Ok(version) = crate::doctor_recorder::runtime_version(&executable) {
        command.env("AGENT_JIT_CLAUDE_VERSION", version);
    } else {
        command.env_remove("AGENT_JIT_CLAUDE_VERSION");
    }
    command
        .current_dir(&identity.worktree_root)
        // Every argument the operator passed, in order, then exactly one addition.
        .args(&claude_args)
        .arg("--plugin-dir")
        .arg(&materialized.directory)
        // The recorder needs to find the same private state the launcher resolved.
        .env(crate::app::HOME_OVERRIDE, &paths.home);

    let status = command.status().map_err(|error| {
        CommandError::new(
            "claude_launch_failed",
            format!("could not run `{executable}`: {error}"),
            ExitClass::Internal,
        )
    })?;

    let report = json!({
        "repository_id": identity.repo_id.to_string(),
        "worktree_root": identity.worktree_root.to_string_lossy(),
        "plugin_dir": materialized.directory.to_string_lossy(),
        "claude_bin": executable,
        "exit_code": status.code(),
    });

    if !status.success() {
        return Err(CommandError::new(
            "claude_exited_nonzero",
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

/// `agent-jit claude uninstall`.
fn uninstall_command(args: &[String]) -> Result<Rendered, CommandError> {
    let as_json = parse_flags(args)?;
    let paths = AppPaths::resolve()?;
    let root = paths.data.join(PLUGIN_ROOT);

    // Only the directory agent-jit generated is removed. Recorded state is untouched: uninstalling
    // an integration must never destroy evidence.
    let removed = root.exists();
    if removed {
        std::fs::remove_dir_all(&root).map_err(|error| {
            CommandError::new(
                "claude_plugin_unremovable",
                format!("`{}`: {error}", root.display()),
                ExitClass::Internal,
            )
        })?;
    }

    let report = json!({
        "removed": removed,
        "plugin_root": root.to_string_lossy(),
        "state_preserved": true,
    });

    if as_json {
        return Ok(Rendered::Json(report));
    }
    Ok(Rendered::Text(if removed {
        format!("removed {}\n", root.display())
    } else {
        "nothing to remove\n".to_owned()
    }))
}
