//! `agent-jit trace` — inspect what the recorder captured.

use agent_jit_domain::ids::{RepositoryId, TrajectoryId};
use agent_jit_engine::process::ProcessRunner;
use agent_jit_engine::repository::discover;
use agent_jit_store::{Store, StorePath};
use serde_json::{Value, json};

use crate::app::AppPaths;
use crate::error::{CommandError, ExitClass};
use crate::output::Rendered;

const USAGE: &str = "usage: agent-jit trace <list --repo <path> | show <trajectory-id>> [--json]";

/// Dispatches a `trace` subcommand.
///
/// # Errors
///
/// Returns a [`CommandError`] for usage mistakes or when a record cannot be read.
pub fn run(args: &[String]) -> Result<Rendered, CommandError> {
    match args.first().map(String::as_str) {
        Some("list") => list(&args[1..]),
        Some("show") => show(&args[1..]),
        Some(other) => Err(CommandError::usage(format!(
            "unknown trace subcommand: {other}\n{USAGE}"
        ))),
        None => Err(CommandError::usage(USAGE)),
    }
}

/// Opens the store, requiring that it has been migrated.
pub(crate) fn open_store() -> Result<Store, CommandError> {
    let paths = AppPaths::resolve()?;
    paths.ensure()?;
    let path = StorePath::new(&paths.state_db).map_err(|error| refusal(&error))?;
    let store = Store::open(&path).map_err(|error| refusal(&error))?;

    if store.schema_version().map_err(|error| refusal(&error))? == 0 {
        return Err(CommandError::new(
            "store_not_migrated",
            "the database has not been migrated; run `agent-jit store migrate`",
            ExitClass::SafetyRefusal,
        ));
    }
    Ok(store)
}

/// Turns a store failure into a command error, preserving its code.
pub(crate) fn refusal(error: &agent_jit_store::StoreError) -> CommandError {
    CommandError::new(error.code(), error.to_string(), ExitClass::SafetyRefusal)
}

/// Lists the trajectories recorded for one repository.
fn list(args: &[String]) -> Result<Rendered, CommandError> {
    let mut root: Option<String> = None;
    let mut as_json = false;

    let mut remaining = args.iter();
    while let Some(argument) = remaining.next() {
        match argument.as_str() {
            "--repo" => {
                root = Some(
                    remaining
                        .next()
                        .ok_or_else(|| CommandError::usage(USAGE))?
                        .clone(),
                );
            }
            "--json" => as_json = true,
            other => {
                return Err(CommandError::usage(format!(
                    "unknown option: {other}\n{USAGE}"
                )));
            }
        }
    }

    let root = root.ok_or_else(|| CommandError::usage(USAGE))?;
    let identity =
        discover(&ProcessRunner::new(), std::path::Path::new(&root)).map_err(|error| {
            CommandError::new(error.code(), error.to_string(), ExitClass::SafetyRefusal)
        })?;

    let store = open_store()?;
    let trajectories = store
        .list_trajectories(&identity.repo_id)
        .map_err(|error| refusal(&error))?;

    let rows: Vec<Value> = trajectories
        .iter()
        .map(|trajectory| {
            json!({
                "trajectory_id": trajectory.id().to_string(),
                "intent": trajectory.body().intent,
                "active_duration_ms": trajectory.body().duration_ms,
                "events": trajectory.body().events.len(),
                "annotated": trajectory.body().outcome_id.is_some(),
            })
        })
        .collect();

    if as_json {
        return Ok(Rendered::Json(json!({
            "repository_id": identity.repo_id.to_string(),
            "count": rows.len(),
            "trajectories": rows,
        })));
    }

    Ok(Rendered::Text(render_list(&identity.repo_id, &rows)))
}

/// Renders the human-readable listing.
fn render_list(repository_id: &RepositoryId, rows: &[Value]) -> String {
    use std::fmt::Write as _;

    let mut text = format!("{repository_id}  {} trajectories\n", rows.len());
    for row in rows {
        let _ = writeln!(
            text,
            "  {}  {:>8} ms  {}",
            row["trajectory_id"].as_str().unwrap_or("?"),
            row["active_duration_ms"].as_i64().unwrap_or(0),
            row["intent"].as_str().unwrap_or("")
        );
    }
    text
}

/// Shows one trajectory and its events.
fn show(args: &[String]) -> Result<Rendered, CommandError> {
    let mut identifier: Option<String> = None;
    let mut as_json = false;

    for argument in args {
        match argument.as_str() {
            "--json" => as_json = true,
            other if other.starts_with("--") => {
                return Err(CommandError::usage(format!(
                    "unknown option: {other}\n{USAGE}"
                )));
            }
            other => identifier = Some(other.to_owned()),
        }
    }

    let identifier = identifier.ok_or_else(|| CommandError::usage(USAGE))?;
    let trajectory_id: TrajectoryId =
        identifier
            .parse()
            .map_err(|error: agent_jit_domain::ids::IdError| {
                CommandError::new(error.code(), error.to_string(), ExitClass::Usage)
            })?;

    let store = open_store()?;
    let trajectory = store
        .get_trajectory(&trajectory_id)
        .map_err(|error| refusal(&error))?
        .ok_or_else(|| {
            CommandError::new(
                "trace_not_found",
                format!("no trajectory `{trajectory_id}`"),
                ExitClass::Usage,
            )
        })?;

    let events = store
        .list_events(&trajectory.body().session_id)
        .map_err(|error| refusal(&error))?;

    let event_rows: Vec<Value> = events
        .iter()
        .map(|event| {
            json!({
                "sequence": event.body().sequence,
                "kind": format!("{:?}", event.body().kind),
                "tool_name": event.body().tool_name,
                "payload_bytes": event.body().payload_bytes,
            })
        })
        .collect();

    let report = json!({
        "trajectory_id": trajectory_id.to_string(),
        "session_id": trajectory.body().session_id.to_string(),
        "repository_id": trajectory.body().repository_id.to_string(),
        "head_commit": trajectory.body().head_commit,
        "intent": trajectory.body().intent,
        "active_duration_ms": trajectory.body().duration_ms,
        "outcome": trajectory
            .body()
            .outcome_id
            .map_or_else(|| Value::String("unannotated".to_owned()), |id| Value::String(id.to_string())),
        "events": event_rows,
    });

    if as_json {
        return Ok(Rendered::Json(report));
    }

    Ok(Rendered::Text(format!(
        "{trajectory_id}\n  intent           {}\n  active_duration  {} ms\n  events           {}\n  outcome          {}\n",
        trajectory.body().intent,
        trajectory.body().duration_ms,
        event_rows.len(),
        report["outcome"].as_str().unwrap_or("unannotated"),
    )))
}
