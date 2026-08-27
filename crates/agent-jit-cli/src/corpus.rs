//! `agent-jit corpus` — report and accrue the Phase 0 trajectory corpus.
//!
//! The historical importer is opt-in and always reports what it refused. A corpus that falls short
//! of the Phase 0 requirement must be explainable — "we have 9 of 50, and here is the count of each
//! rejection reason" — rather than a number nobody can account for.

use std::collections::BTreeSet;
use std::fmt::Write as _;
use std::path::Path;

use agent_jit_domain::canonical::Digest;
use agent_jit_domain::redaction::{PathAliases, Redactor};
use agent_jit_engine::adapters::claude_history::{PROFILE_ID, scan};
use agent_jit_engine::process::ProcessRunner;
use agent_jit_engine::repository::discover;
use serde_json::json;

use crate::error::{CommandError, ExitClass};
use crate::output::Rendered;

/// Trajectories Phase 0 requires before the thesis can be judged.
pub const REQUIRED_TRAJECTORIES: u32 = 50;

const USAGE: &str = "usage: agent-jit corpus <status --repo <path> | \
                     import-claude --repo <path> --project-dir <path> [--dry-run]> [--json]";

/// Dispatches a `corpus` subcommand.
///
/// # Errors
///
/// Returns a [`CommandError`] for usage mistakes or a refused repository.
pub fn run(args: &[String]) -> Result<Rendered, CommandError> {
    match args.first().map(String::as_str) {
        Some("status") => status(&args[1..]),
        Some("import-claude") => import_claude(&args[1..]),
        Some(other) => Err(CommandError::usage(format!(
            "unknown corpus subcommand: {other}\n{USAGE}"
        ))),
        None => Err(CommandError::usage(USAGE)),
    }
}

/// Parsed options shared by the corpus subcommands.
struct Options {
    repo: String,
    project_dir: Option<String>,
    dry_run: bool,
    as_json: bool,
}

/// Parses the corpus options.
fn parse(args: &[String]) -> Result<Options, CommandError> {
    let mut repo: Option<String> = None;
    let mut project_dir: Option<String> = None;
    let mut dry_run = false;
    let mut as_json = false;

    let mut remaining = args.iter();
    while let Some(argument) = remaining.next() {
        match argument.as_str() {
            "--repo" => {
                repo = Some(
                    remaining
                        .next()
                        .ok_or_else(|| CommandError::usage(USAGE))?
                        .clone(),
                );
            }
            "--project-dir" => {
                project_dir = Some(
                    remaining
                        .next()
                        .ok_or_else(|| CommandError::usage(USAGE))?
                        .clone(),
                );
            }
            "--dry-run" => dry_run = true,
            "--json" => as_json = true,
            other => {
                return Err(CommandError::usage(format!(
                    "unknown option: {other}\n{USAGE}"
                )));
            }
        }
    }

    Ok(Options {
        repo: repo.ok_or_else(|| CommandError::usage(USAGE))?,
        project_dir,
        dry_run,
        as_json,
    })
}

/// Reports how far the corpus is from the Phase 0 requirement.
fn status(args: &[String]) -> Result<Rendered, CommandError> {
    let options = parse(args)?;
    let identity = discover(&ProcessRunner::new(), Path::new(&options.repo)).map_err(|error| {
        CommandError::new(error.code(), error.to_string(), ExitClass::SafetyRefusal)
    })?;

    let store = crate::trace::open_store()?;
    let recorded = store
        .count_trajectories(&identity.repo_id)
        .map_err(|error| crate::trace::refusal(&error))?;

    let report = json!({
        "repository_id": identity.repo_id.to_string(),
        "recorded": recorded,
        "required": REQUIRED_TRAJECTORIES,
        "remaining": REQUIRED_TRAJECTORIES.saturating_sub(recorded),
        "sufficient": recorded >= REQUIRED_TRAJECTORIES,
    });

    if options.as_json {
        return Ok(Rendered::Json(report));
    }
    Ok(Rendered::Text(format!(
        "{recorded} of {REQUIRED_TRAJECTORIES} trajectories ({} still needed)\n",
        REQUIRED_TRAJECTORIES.saturating_sub(recorded)
    )))
}

/// Scans a Claude project directory and reports what would be, or was, imported.
fn import_claude(args: &[String]) -> Result<Rendered, CommandError> {
    let options = parse(args)?;
    let project_dir = options
        .project_dir
        .clone()
        .ok_or_else(|| CommandError::usage(USAGE))?;

    let identity = discover(&ProcessRunner::new(), Path::new(&options.repo)).map_err(|error| {
        CommandError::new(error.code(), error.to_string(), ExitClass::SafetyRefusal)
    })?;

    let project = Path::new(&project_dir);
    if !project.is_dir() {
        return Err(CommandError::new(
            "corpus_project_dir_missing",
            format!("`{project_dir}` is not a directory"),
            ExitClass::Usage,
        ));
    }

    let home = std::env::var("HOME").unwrap_or_default();
    let redactor = Redactor::new().with_path_aliases(PathAliases::new(
        &home,
        &identity.worktree_root.to_string_lossy(),
    ));

    // Nothing is imported twice: the digests of files already imported are excluded up front.
    let already_imported: BTreeSet<Digest> = BTreeSet::new();
    let scanned = scan(
        project,
        &identity.worktree_root,
        &redactor,
        &already_imported,
    );

    let accepted: Vec<_> = scanned
        .accepted
        .iter()
        .map(|session| {
            json!({
                "source_path": session.source_path,
                "session_key": session.session_key,
                "claude_version": session.claude_version,
                "prompts": session.prompts,
                "assistant_turns": session.assistant_turns,
                "tool_calls": session.tool_calls.len(),
                "source_digest": session.source_digest.to_string(),
            })
        })
        .collect();

    let rejected: Vec<_> = scanned
        .rejected
        .iter()
        .map(|rejected| {
            json!({
                "source_path": rejected.source_path,
                "reason": rejected.reason.as_str(),
            })
        })
        .collect();

    let report = json!({
        "profile": PROFILE_ID,
        "repository_id": identity.repo_id.to_string(),
        "project_dir": project_dir,
        "dry_run": options.dry_run,
        // On the proceed path this is what would be added to the corpus; persisting the accepted
        // sessions is the next step and is deliberately not done by a dry run.
        "accepted_count": accepted.len(),
        "rejected_count": rejected.len(),
        "rejection_counts": scanned.rejection_counts(),
        "accepted": accepted,
        "rejected": rejected,
    });

    if options.as_json {
        return Ok(Rendered::Json(report));
    }

    let mut text = format!(
        "{} accepted, {} rejected (profile {PROFILE_ID})\n",
        accepted.len(),
        rejected.len()
    );
    for (reason, count) in scanned.rejection_counts() {
        let _ = writeln!(text, "  {count:>3}  {reason}");
    }
    Ok(Rendered::Text(text))
}
