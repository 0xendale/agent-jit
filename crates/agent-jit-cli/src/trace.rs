//! `agent-jit trace` — inspect what the recorder captured.

use agent_jit_domain::ids::{RepositoryId, TrajectoryId};
use agent_jit_engine::process::ProcessRunner;
use agent_jit_engine::repository::discover;
use agent_jit_store::{Store, StorePath};
use serde_json::{Value, json};

use crate::app::AppPaths;
use crate::error::{CommandError, ExitClass};
use crate::output::Rendered;

const USAGE: &str = "usage: agent-jit trace <list --repo <path> | show <trajectory-id> | metrics <trajectory-id> | annotate ...> [--json]";

/// Dispatches a `trace` subcommand.
///
/// # Errors
///
/// Returns a [`CommandError`] for usage mistakes or when a record cannot be read.
pub fn run(args: &[String]) -> Result<Rendered, CommandError> {
    match args.first().map(String::as_str) {
        Some("list") => list(&args[1..]),
        Some("show") => show(&args[1..]),
        Some("metrics") => metrics(&args[1..]),
        Some("annotate") => crate::trace_annotation::run(&args[1..]),
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

    let mut rows = Vec::with_capacity(trajectories.len());
    for trajectory in &trajectories {
        let annotated = trajectory.body().outcome_id.is_some()
            || store
                .current_outcome_annotation(trajectory.id())
                .map_err(|error| refusal(&error))?
                .is_some();
        rows.push(json!({
            "trajectory_id": trajectory.id().to_string(),
            "intent": trajectory.body().intent,
            "active_duration_ms": trajectory.body().duration_ms,
            "events": trajectory.body().events.len(),
            "annotated": annotated,
        }));
    }

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

    let (annotation_history, current_annotation) =
        crate::trace_annotation::show_fields(&store, &trajectory_id)?;
    let current_revision = current_annotation.get("revision").and_then(Value::as_u64);
    let outcome = current_annotation
        .get("status")
        .cloned()
        .unwrap_or_else(|| {
            trajectory.body().outcome_id.map_or_else(
                || Value::String("unannotated".to_owned()),
                |id| Value::String(id.to_string()),
            )
        });
    let report = json!({
        "trajectory_id": trajectory_id.to_string(),
        "session_id": trajectory.body().session_id.to_string(),
        "repository_id": trajectory.body().repository_id.to_string(),
        "head_commit": trajectory.body().head_commit,
        "intent": trajectory.body().intent,
        "active_duration_ms": trajectory.body().duration_ms,
        "outcome": outcome,
        "events": event_rows,
        "annotation_history": annotation_history,
        "current_annotation": current_annotation,
    });

    if as_json {
        return Ok(Rendered::Json(report));
    }

    let outcome = report["outcome"].as_str().unwrap_or("unannotated");
    let rendered_outcome = current_revision.map_or_else(
        || outcome.to_owned(),
        |revision| format!("{outcome} (annotation revision {revision})"),
    );
    Ok(Rendered::Text(format!(
        "{trajectory_id}\n  intent           {}\n  active_duration  {} ms\n  events           {}\n  outcome          {}\n",
        trajectory.body().intent,
        trajectory.body().duration_ms,
        event_rows.len(),
        rendered_outcome,
    )))
}

/// Reports the stored cost metrics for one trajectory.
///
/// The numbers come from the `trace_metrics` table, which the recorder computed from stored events.
/// Every one of them is rendered with its measurement source, so a reader can tell an exact value
/// from an estimate and an estimate from a value that was never measured.
fn metrics(args: &[String]) -> Result<Rendered, CommandError> {
    let (trajectory_id, as_json) = parse_target(args)?;

    let store = open_store()?;
    let metrics = store
        .get_trace_metrics(&trajectory_id)
        .map_err(|error| refusal(&error))?
        .ok_or_else(|| {
            CommandError::new(
                "trace_not_found",
                format!("no stored metrics for `{trajectory_id}`"),
                ExitClass::Usage,
            )
        })?;

    if as_json {
        let rendered = serde_json::to_value(&metrics).map_err(|error| {
            CommandError::new("trace_unrenderable", error.to_string(), ExitClass::Internal)
        })?;
        return Ok(Rendered::Json(rendered));
    }

    Ok(Rendered::Text(format!(
        "{trajectory_id}\n\
         \x20 active_duration  {} ({})\n\
         \x20 wall_duration    {} ({})\n\
         \x20 turns            {}\n\
         \x20 tool_calls       {} ({} failed)\n\
         \x20 observed_bytes   {}\n\
         \x20 input_tokens     {} ({})\n\
         \x20 exact_tokens     {} ({})\n\
         \x20 lossy            {}\n",
        render_value(metrics.active_duration_ms.value()),
        metrics.active_duration_ms.source(),
        render_value(metrics.wall_duration_ms.value()),
        metrics.wall_duration_ms.source(),
        metrics.turns,
        metrics.agent_tool_calls,
        metrics.failed_tool_calls,
        metrics.observed_bytes,
        render_value(metrics.estimated_input_tokens.value()),
        metrics.estimated_input_tokens.source(),
        render_value(metrics.exact_input_tokens.value()),
        metrics.exact_input_tokens.source(),
        metrics.health.is_lossy(),
    )))
}

/// Renders a measured value, showing absence as `unknown` rather than as a number.
fn render_value<T: std::fmt::Display>(value: Option<T>) -> String {
    value.map_or_else(|| "unknown".to_owned(), |value| value.to_string())
}

/// Parses `<trajectory-id> [--json]`, shared by `show` and `metrics`.
fn parse_target(args: &[String]) -> Result<(TrajectoryId, bool), CommandError> {
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
    Ok((trajectory_id, as_json))
}
