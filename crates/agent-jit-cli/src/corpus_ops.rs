//! Corpus hold, release, and retention commands.

use agent_jit_domain::ids::SessionId;
use agent_jit_store::{Clock, PruneMode, RetentionPolicy, SystemClock};
use serde_json::json;

use crate::error::{CommandError, ExitClass};
use crate::output::Rendered;

const HOLD_USAGE: &str = "usage: agent-jit corpus <hold|release> <session-id> --actor <actor> --rationale <reason> [--json]";
const PRUNE_USAGE: &str = "usage: agent-jit corpus prune [--max-age-days <days>] [--max-bytes <bytes>] [--apply] [--json]";
const DEFAULT_DAYS: i64 = 30;
const DEFAULT_BYTES: u64 = 1_073_741_824;
const DAY_MS: i64 = 86_400_000;

pub(crate) fn hold(args: &[String], release: bool) -> Result<Rendered, CommandError> {
    let (session, actor, rationale, as_json) = parse_hold(args)?;
    let mut store = crate::trace::open_store()?;
    let session_record = store
        .get_session(&session)
        .map_err(|error| crate::trace::refusal(&error))?
        .ok_or_else(|| {
            CommandError::new(
                "session_not_found",
                format!("no session `{session}`"),
                ExitClass::Usage,
            )
        })?;
    let redactor = crate::persistence_redaction::redactor(&session_record.body().worktree_root);
    let actor = required_text(
        Some(redactor.field(&actor).value().clone()),
        "corpus_actor_invalid",
    )?;
    let rationale = required_text(
        Some(redactor.field(&rationale).value().clone()),
        "corpus_rationale_invalid",
    )?;
    let before = store
        .active_hold(&session)
        .map_err(|error| crate::trace::refusal(&error))?;
    let changed = if release {
        store
            .release_session(&session, &actor, &rationale)
            .map_err(|error| crate::trace::refusal(&error))?;
        before.is_some()
    } else {
        let changed = before
            .as_ref()
            .is_none_or(|current| current.actor != actor || current.rationale != rationale);
        store
            .hold_session(&session, &actor, &rationale)
            .map_err(|error| crate::trace::refusal(&error))?;
        changed
    };
    let active = !release;
    let report = json!({
        "session_id": session.to_string(),
        "active": active,
        "changed": changed,
        "actor": actor,
        "rationale": rationale,
    });
    if as_json {
        Ok(Rendered::Json(report))
    } else {
        Ok(Rendered::Text(format!(
            "{session}: {} ({})\n",
            if active { "held" } else { "released" },
            if changed { "changed" } else { "no-op" }
        )))
    }
}

pub(crate) fn prune(args: &[String]) -> Result<Rendered, CommandError> {
    let mut days = DEFAULT_DAYS;
    let mut max_bytes = DEFAULT_BYTES;
    let mut apply = false;
    let mut as_json = false;
    let mut remaining = args.iter();
    while let Some(argument) = remaining.next() {
        match argument.as_str() {
            "--max-age-days" => days = parse_number(value(&mut remaining)?)?,
            "--max-bytes" => max_bytes = parse_number(value(&mut remaining)?)?,
            "--apply" => apply = true,
            "--json" => as_json = true,
            _ => return Err(CommandError::usage(PRUNE_USAGE)),
        }
    }
    let age_ms = days.checked_mul(DAY_MS).ok_or_else(|| {
        CommandError::refused(
            "retention_age_overflow",
            "retention age arithmetic overflowed",
        )
    })?;
    let policy = RetentionPolicy::new(SystemClock.now_unix_ms(), age_ms, max_bytes)
        .map_err(|error| crate::trace::refusal(&error))?;
    let mut store = crate::trace::open_store()?;
    let result = store
        .prune(
            policy,
            if apply {
                PruneMode::Apply
            } else {
                PruneMode::DryRun
            },
        )
        .map_err(|error| crate::trace::refusal(&error))?;
    let selected: Vec<String> = result
        .selected_sessions
        .iter()
        .map(ToString::to_string)
        .collect();
    let deleted: Vec<String> = result
        .deleted_sessions
        .iter()
        .map(ToString::to_string)
        .collect();
    let report = json!({
        "dry_run": !apply,
        "max_age_days": days,
        "max_bytes": max_bytes,
        "selected_sessions": selected,
        "deleted_sessions": deleted,
    });
    if as_json {
        Ok(Rendered::Json(report))
    } else {
        Ok(Rendered::Text(format!(
            "{} sessions selected; {} deleted{}\n",
            result.selected_sessions.len(),
            result.deleted_sessions.len(),
            if apply { "" } else { " (dry run)" }
        )))
    }
}

fn parse_hold(args: &[String]) -> Result<(SessionId, String, String, bool), CommandError> {
    let session = args
        .first()
        .ok_or_else(|| CommandError::usage(HOLD_USAGE))?
        .parse()
        .map_err(|error: agent_jit_domain::ids::IdError| {
            CommandError::new(error.code(), error.to_string(), ExitClass::Usage)
        })?;
    let mut actor = None;
    let mut rationale = None;
    let mut as_json = false;
    let mut remaining = args[1..].iter();
    while let Some(argument) = remaining.next() {
        match argument.as_str() {
            "--actor" => actor = Some(value(&mut remaining)?.to_owned()),
            "--rationale" => rationale = Some(value(&mut remaining)?.to_owned()),
            "--json" => as_json = true,
            _ => return Err(CommandError::usage(HOLD_USAGE)),
        }
    }
    let actor = required_text(actor, "corpus_actor_invalid")?;
    let rationale = required_text(rationale, "corpus_rationale_invalid")?;
    Ok((session, actor, rationale, as_json))
}

fn required_text(value: Option<String>, code: &str) -> Result<String, CommandError> {
    let value = value.ok_or_else(|| CommandError::usage(HOLD_USAGE))?;
    if value.is_empty() {
        Err(CommandError::refused(code, "value must not be empty"))
    } else {
        Ok(value)
    }
}

fn value<'a>(remaining: &mut impl Iterator<Item = &'a String>) -> Result<&'a str, CommandError> {
    remaining
        .next()
        .map(String::as_str)
        .ok_or_else(|| CommandError::usage(PRUNE_USAGE))
}

fn parse_number<T: std::str::FromStr>(value: &str) -> Result<T, CommandError> {
    value.parse().map_err(|_| CommandError::usage(PRUNE_USAGE))
}
