//! `agent-jit hook ingest` — the only command Claude Code runs on its own critical path.
//!
//! Three rules govern this command, and they are why it looks different from the others:
//!
//! * **stdout stays empty.** Anything printed there would be fed back into Claude as context.
//! * **the exit status is zero**, even when ingestion fails. A recorder fault is the recorder's
//!   problem; it must never stop the user's work. Faults are counted in the health spool instead.
//! * **nothing unparsed is persisted.** A malformed payload produces a health counter, never a
//!   half-understood event.

use std::io::{self, Write as _};

use agent_jit_domain::redaction::{PathAliases, Redactor};
use agent_jit_engine::adapters::claude_hooks::{HookEventKind, normalize};
use agent_jit_engine::normalize::read_bounded;
use agent_jit_engine::recorder::SegmentStore;
use agent_jit_engine::spool::{Spool, SpoolKind};
use serde_json::json;

use crate::app::AppPaths;
use crate::error::CommandError;
use crate::output::Rendered;

/// Maximum bytes read from a hook's stdin. Anything beyond this is counted, not kept.
const HOOK_INPUT_LIMIT: usize = 1024 * 1024;

const USAGE: &str = "usage: agent-jit hook ingest --event <session-start|user-prompt-submit|pre-tool-use|post-tool-use|post-tool-use-failure|stop|session-end> [--claude-version <version>]";

/// Dispatches a `hook` subcommand.
///
/// # Errors
///
/// Returns a [`CommandError`] only for usage mistakes, which are the operator's problem rather
/// than Claude's. Ingestion failures are reported through the health spool and exit zero.
pub fn run(args: &[String]) -> Result<Rendered, CommandError> {
    match args.first().map(String::as_str) {
        Some("ingest") => match ingest(&args[1..]) {
            Ok(rendered) => Ok(rendered),
            Err(error) if error.code == "usage" => Err(error),
            Err(error) => {
                let _ = writeln!(
                    io::stderr().lock(),
                    "agent-jit: recorder unavailable: {}",
                    error.code
                );
                Ok(Rendered::Text(String::new()))
            }
        },
        Some(other) => Err(CommandError::usage(format!(
            "unknown hook subcommand: {other}\n{USAGE}"
        ))),
        None => Err(CommandError::usage(USAGE)),
    }
}

/// Reads one hook payload from stdin and appends it to the spool.
fn ingest(args: &[String]) -> Result<Rendered, CommandError> {
    let (kind, claude_version) = parse_arguments(args)?;

    let paths = AppPaths::resolve()?;
    paths.ensure()?;
    let spool = Spool::open(&paths.spool)
        .map_err(|error| CommandError::refused(error.code(), error.to_string()))?;
    let segments = SegmentStore::open(&paths.spool.join("segments"))
        .map_err(|error| CommandError::refused(error.code(), error.to_string()))?;

    match capture(kind, claude_version.as_deref(), &segments) {
        Ok(()) => Ok(Rendered::Text(String::new())),
        Err(fault) => {
            // Record the fault, tell the operator on stderr, and let Claude carry on.
            let line = json!({
                "code": fault.code,
                "detail": fault.detail,
                "event": kind.as_str(),
            });
            let _ = spool.append(SpoolKind::Health, &line.to_string());

            let mut err = io::stderr().lock();
            let _ = writeln!(err, "agent-jit: {}: {}", fault.code, fault.detail);
            let _ = err.flush();

            Ok(Rendered::Text(String::new()))
        }
    }
}

/// A recorder fault: something that stops this event from being recorded, but not Claude.
struct Fault {
    code: &'static str,
    detail: String,
}

/// Reads, normalizes, and spools one event.
fn capture(
    kind: HookEventKind,
    claude_version: Option<&str>,
    segments: &SegmentStore,
) -> Result<(), Fault> {
    let input = read_bounded(&mut io::stdin().lock(), HOOK_INPUT_LIMIT).map_err(|error| Fault {
        code: error.code(),
        detail: error.to_string(),
    })?;

    let text = input.as_text().map_err(|error| Fault {
        code: error.code(),
        detail: error.to_string(),
    })?;

    let hook = normalize(kind, text, &redactor(), claude_version).map_err(|error| Fault {
        code: error.code(),
        detail: error.to_string(),
    })?;

    // One immutable segment per event: concurrent hooks are separate processes, and a shared
    // append-only file would let a killed process leave a fragment that reads like a record.
    let observed_at = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |elapsed| {
            i64::try_from(elapsed.as_millis()).unwrap_or(i64::MAX)
        });

    segments
        .write(&hook, observed_at)
        .map(|_| ())
        .map_err(|error| Fault {
            code: error.code(),
            detail: error.to_string(),
        })
}

/// Builds the redactor used for hook payloads.
fn redactor() -> Redactor {
    let home = std::env::var("HOME").unwrap_or_default();
    Redactor::new().with_path_aliases(PathAliases::new(&home, ""))
}

/// Parses `--event` and `--claude-version`.
fn parse_arguments(args: &[String]) -> Result<(HookEventKind, Option<String>), CommandError> {
    let mut kind: Option<HookEventKind> = None;
    let mut claude_version = std::env::var("AGENT_JIT_CLAUDE_VERSION").ok();

    let mut remaining = args.iter();
    while let Some(argument) = remaining.next() {
        match argument.as_str() {
            "--event" => {
                let value = remaining.next().ok_or_else(|| CommandError::usage(USAGE))?;
                kind = Some(
                    value
                        .parse::<HookEventKind>()
                        .map_err(|error| CommandError::usage(format!("{error}\n{USAGE}")))?,
                );
            }
            "--claude-version" => {
                claude_version = Some(
                    remaining
                        .next()
                        .ok_or_else(|| CommandError::usage(USAGE))?
                        .clone(),
                );
            }
            other => {
                return Err(CommandError::usage(format!(
                    "unknown option: {other}\n{USAGE}"
                )));
            }
        }
    }

    let kind = kind.ok_or_else(|| CommandError::usage(USAGE))?;
    Ok((kind, claude_version))
}
