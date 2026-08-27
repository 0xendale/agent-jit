//! `agent-jit store` — migrate the private database and check its health.

use std::path::Path;

use agent_jit_engine::process::ProcessRunner;
use agent_jit_engine::recorder::{Recorder, SegmentStore, finalize};
use agent_jit_engine::repository::discover;
use agent_jit_store::{CURRENT_SCHEMA_VERSION, Store, StorePath};
use serde_json::json;

use crate::app::AppPaths;
use crate::error::{CommandError, ExitClass};
use crate::output::Rendered;

const USAGE: &str = "usage: agent-jit store <migrate | check | recover> [--json]";

/// Recorder version stamped on records this build writes.
const RECORDER_VERSION: &str = concat!(env!("CARGO_PKG_NAME"), "/", env!("CARGO_PKG_VERSION"));

/// Dispatches a `store` subcommand.
///
/// # Errors
///
/// Returns a [`CommandError`] when arguments are wrong or the database is refused.
pub fn run(args: &[String]) -> Result<Rendered, CommandError> {
    let (subcommand, rest) = args
        .split_first()
        .ok_or_else(|| CommandError::usage(USAGE))?;
    let as_json = parse_options(rest)?;

    match subcommand.as_str() {
        "migrate" => migrate(as_json),
        "check" => check(as_json),
        "recover" => recover(as_json),
        other => Err(CommandError::usage(format!(
            "unknown store subcommand: {other}\n{USAGE}"
        ))),
    }
}

/// Parses the options every store subcommand accepts.
fn parse_options(args: &[String]) -> Result<bool, CommandError> {
    let mut as_json = false;
    for argument in args {
        match argument.as_str() {
            "--json" => as_json = true,
            other => {
                return Err(CommandError::usage(format!(
                    "unknown option: {other}\n{USAGE}"
                )));
            }
        }
    }
    Ok(as_json)
}

/// Opens the store at the resolved private path.
fn open() -> Result<Store, CommandError> {
    let paths = AppPaths::resolve()?;
    paths.ensure()?;

    let path = StorePath::new(&paths.state_db).map_err(|error| refusal(&error))?;
    Store::open(&path).map_err(|error| refusal(&error))
}

/// Turns a store failure into a command error, preserving its code.
fn refusal(error: &agent_jit_store::StoreError) -> CommandError {
    CommandError::new(error.code(), error.to_string(), ExitClass::SafetyRefusal)
}

/// Applies pending migrations.
fn migrate(as_json: bool) -> Result<Rendered, CommandError> {
    let mut store = open()?;
    let report = store.migrate().map_err(|error| refusal(&error))?;

    if as_json {
        return Ok(Rendered::Json(json!({
            "from_version": report.from_version,
            "to_version": report.to_version,
            "applied": report.applied,
        })));
    }
    Ok(Rendered::Text(format!(
        "migrated v{} -> v{} ({} applied)\n",
        report.from_version, report.to_version, report.applied
    )))
}

/// Reports database health.
fn check(as_json: bool) -> Result<Rendered, CommandError> {
    let store = open()?;
    let version = store.schema_version().map_err(|error| refusal(&error))?;
    if version == 0 {
        return Err(CommandError::new(
            "store_not_migrated",
            format!(
                "the database is at v0; run `agent-jit store migrate` to reach v{CURRENT_SCHEMA_VERSION}"
            ),
            ExitClass::SafetyRefusal,
        ));
    }

    let health = store.check().map_err(|error| refusal(&error))?;
    if as_json {
        return Ok(Rendered::Json(json!({
            "healthy": health.healthy,
            "integrity": health.integrity,
            "foreign_key_violations": health.foreign_key_violations,
            "schema_version": health.schema_version,
        })));
    }
    Ok(Rendered::Text(format!(
        "schema_version         {}\n\
         integrity              {}\n\
         foreign_key_violations {}\n\
         healthy                {}\n",
        health.schema_version, health.integrity, health.foreign_key_violations, health.healthy
    )))
}

/// Drains the spool: finalizes every complete session and reports what is still open.
///
/// Recovery is safe to run at any time, including after a crash. Sessions that are still open are
/// left alone; sessions that ended are written once — finalization derives its identifiers from
/// content, so running recovery twice converges rather than duplicating.
fn recover(as_json: bool) -> Result<Rendered, CommandError> {
    let paths = AppPaths::resolve()?;
    paths.ensure()?;

    let mut store = open()?;
    if store.schema_version().map_err(|error| refusal(&error))? == 0 {
        return Err(CommandError::new(
            "store_not_migrated",
            "the database has not been migrated; run `agent-jit store migrate`",
            ExitClass::SafetyRefusal,
        ));
    }

    let segments = SegmentStore::open(&paths.spool.join("segments")).map_err(|error| {
        CommandError::new(error.code(), error.to_string(), ExitClass::SafetyRefusal)
    })?;
    let recorder = Recorder::new(segments.clone());

    let sessions = segments.sessions().map_err(|error| {
        CommandError::new(error.code(), error.to_string(), ExitClass::SafetyRefusal)
    })?;

    let mut recovered = Vec::new();
    let mut pending = Vec::new();
    let mut quarantined = 0_u32;
    let mut skipped = Vec::new();

    for session_key in sessions {
        let aggregate = match recorder.aggregate(&session_key) {
            Ok(aggregate) => aggregate,
            Err(error) => {
                skipped.push(json!({"session": session_key, "code": error.code()}));
                continue;
            }
        };
        quarantined = quarantined
            .saturating_add(u32::try_from(aggregate.quarantined.len()).unwrap_or(u32::MAX));

        if !aggregate.complete {
            pending.push(json!({
                "session": session_key,
                "events": aggregate.events.len(),
                "turns": aggregate.turns,
            }));
            continue;
        }

        let identity = match discover(&ProcessRunner::new(), Path::new(&aggregate.cwd)) {
            Ok(identity) => identity,
            Err(error) => {
                skipped.push(json!({"session": session_key, "code": error.code()}));
                continue;
            }
        };

        let report =
            finalize(&mut store, &aggregate, &identity, RECORDER_VERSION).map_err(|error| {
                CommandError::new(error.code(), error.to_string(), ExitClass::Internal)
            })?;

        recovered.push(json!({
            "session": session_key,
            "trajectory_id": report.trajectory_id.to_string(),
            "events_written": report.events_written,
            "created": report.created,
        }));

        segments.discard_session(&session_key).map_err(|error| {
            CommandError::new(error.code(), error.to_string(), ExitClass::Internal)
        })?;
    }

    let report = json!({
        "recovered": recovered,
        "pending": pending,
        "skipped": skipped,
        "quarantined_segments": quarantined,
    });

    if as_json {
        return Ok(Rendered::Json(report));
    }
    Ok(Rendered::Text(format!(
        "recovered {} session(s), {} still open, {} skipped, {} quarantined segment(s)\n",
        recovered.len(),
        pending.len(),
        skipped.len(),
        quarantined
    )))
}
