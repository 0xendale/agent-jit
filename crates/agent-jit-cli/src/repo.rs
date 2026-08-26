//! `agent-jit repo` — inspect the identity of a repository without recording anything.

use std::path::Path;

use agent_jit_engine::host::HostSupport;
use agent_jit_engine::process::ProcessRunner;
use agent_jit_engine::repository::discover;

use crate::CommandError;

const USAGE: &str = "usage: agent-jit repo inspect --root <path> [--json]";

/// Dispatches a `repo` subcommand.
///
/// # Errors
///
/// Returns a [`CommandError`] when arguments are missing or the repository is refused.
pub fn run(args: &[String]) -> Result<String, CommandError> {
    match args.first().map(String::as_str) {
        Some("inspect") => inspect(&args[1..]),
        Some(other) => Err(CommandError::usage(format!(
            "unknown repo subcommand: {other}\n{USAGE}"
        ))),
        None => Err(CommandError::usage(USAGE)),
    }
}

/// Reports the identity of the repository containing `--root`.
fn inspect(args: &[String]) -> Result<String, CommandError> {
    let mut root: Option<&str> = None;
    let mut as_json = false;

    let mut remaining = args.iter();
    while let Some(argument) = remaining.next() {
        match argument.as_str() {
            "--root" => {
                root = Some(
                    remaining
                        .next()
                        .ok_or_else(|| CommandError::usage(USAGE))?
                        .as_str(),
                );
            }
            "--json" => as_json = true,
            _ => return Err(CommandError::usage(USAGE)),
        }
    }

    let root = root.ok_or_else(|| CommandError::usage(USAGE))?;

    let host = HostSupport::current()
        .map_err(|error| CommandError::refused(error.code(), error.to_string()))?;
    let identity = discover(&ProcessRunner::new(), Path::new(root))
        .map_err(|error| CommandError::refused(error.code(), error.to_string()))?;

    let report = serde_json::json!({
        "repo_id": identity.repo_id.to_string(),
        "git_common_dir": identity.git_common_dir.to_string_lossy(),
        "worktree_root": identity.worktree_root.to_string_lossy(),
        "head_commit": identity.head_commit,
        "host": {"os": host.os, "arch": host.arch},
    });

    if as_json {
        let mut rendered = serde_json::to_string_pretty(&report).map_err(|error| {
            CommandError::refused("repo_report_unrenderable", error.to_string())
        })?;
        rendered.push('\n');
        return Ok(rendered);
    }

    Ok(format!(
        "repo_id        {}\n\
         worktree_root  {}\n\
         git_common_dir {}\n\
         head_commit    {}\n\
         host           {}/{}\n",
        identity.repo_id,
        identity.worktree_root.display(),
        identity.git_common_dir.display(),
        identity.head_commit,
        host.os,
        host.arch,
    ))
}
