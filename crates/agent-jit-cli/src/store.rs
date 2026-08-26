//! `agent-jit store` — migrate the private database and check its health.

use agent_jit_store::{CURRENT_SCHEMA_VERSION, Store, StorePath};
use serde_json::json;

use crate::app::AppPaths;
use crate::error::{CommandError, ExitClass};
use crate::output::Rendered;

const USAGE: &str = "usage: agent-jit store <migrate | check> [--json]";

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
