//! Manual trace outcome annotation command.

use agent_jit_domain::envelope::{Envelope, Provenance, ProvenanceSource};
use agent_jit_domain::ids::{TrajectoryId, kind};
use agent_jit_domain::outcome::{
    AnnotationRevision, AnnotationStatus, EvidenceRef, OutcomeAnnotation,
};
use agent_jit_store::{IdSource, Store, SystemClock, SystemIdSource};
use serde_json::{Value, json};

use crate::error::{CommandError, ExitClass};
use crate::output::Rendered;

const USAGE: &str = "usage: agent-jit trace annotate <trajectory-id> --outcome <succeeded|failed|abandoned|unknown> --actor <actor> --rationale <reason> [--evidence <relative-path>]... [--json]";

pub(crate) fn run(args: &[String]) -> Result<Rendered, CommandError> {
    let options = parse(args)?;
    let mut store = crate::trace::open_store()?;
    let trajectory = store
        .get_trajectory(&options.trajectory)
        .map_err(|error| crate::trace::refusal(&error))?
        .ok_or_else(|| {
            CommandError::new(
                "trace_not_found",
                format!("no trajectory `{}`", options.trajectory),
                ExitClass::Usage,
            )
        })?;
    let session = store
        .get_session(&trajectory.body().session_id)
        .map_err(|error| crate::trace::refusal(&error))?
        .ok_or_else(|| {
            CommandError::refused("store_record_invalid", "trajectory session is missing")
        })?;
    let redactor = crate::persistence_redaction::redactor(&session.body().worktree_root);
    let actor = redactor.field(&options.actor).value().clone();
    let rationale = redactor.field(&options.rationale).value().clone();
    let evidence = options
        .evidence
        .iter()
        .map(|value| {
            let validated = EvidenceRef::new(value).map_err(|error| {
                CommandError::refused("evidence_ref_invalid", error.to_string())
            })?;
            let redacted = redactor.field(validated.as_str());
            EvidenceRef::new(redacted.value())
                .map_err(|error| CommandError::refused("evidence_ref_invalid", error.to_string()))
        })
        .collect::<Result<Vec<_>, _>>()?;
    let revision = store
        .current_outcome_annotation(&options.trajectory)
        .map_err(|error| crate::trace::refusal(&error))?
        .map_or_else(
            || AnnotationRevision::new(1),
            |current| current.body().revision().next(),
        )
        .map_err(annotation_error)?;
    let now = agent_jit_store::Clock::now_unix_ms(&SystemClock);
    let body = OutcomeAnnotation::new(
        options.trajectory,
        revision,
        options.status,
        &actor,
        now,
        &rationale,
        evidence,
    )
    .map_err(annotation_error)?;
    let id = SystemIdSource
        .next::<kind::OutcomeAnnotation>()
        .map_err(|error| crate::trace::refusal(&error))?;
    let provenance = Provenance {
        produced_by: format!("agent-jit/{}", env!("CARGO_PKG_VERSION")),
        source: ProvenanceSource::Confirmed,
        recorded_at_unix_ms: now,
        parents: Vec::new(),
    };
    store
        .append_outcome_annotation(&Envelope::new(id, provenance, body))
        .map_err(|error| crate::trace::refusal(&error))?;
    let report = json!({
        "trajectory_id": options.trajectory.to_string(),
        "annotation_id": id.to_string(),
        "revision": revision.get(),
        "status": status_name(options.status),
        "current": true,
    });
    if options.as_json {
        Ok(Rendered::Json(report))
    } else {
        Ok(Rendered::Text(format!(
            "{} revision {}: {}\n",
            options.trajectory,
            revision.get(),
            status_name(options.status)
        )))
    }
}

pub(crate) fn show_fields(
    store: &Store,
    trajectory: &TrajectoryId,
) -> Result<(Value, Value), CommandError> {
    let history = store
        .list_outcome_annotations(trajectory)
        .map_err(|error| crate::trace::refusal(&error))?;
    let rows: Vec<Value> = history.iter().map(annotation_json).collect();
    let current = store
        .current_outcome_annotation(trajectory)
        .map_err(|error| crate::trace::refusal(&error))?
        .as_ref()
        .map_or(Value::Null, annotation_json);
    Ok((Value::Array(rows), current))
}

struct Options {
    trajectory: TrajectoryId,
    status: AnnotationStatus,
    actor: String,
    rationale: String,
    evidence: Vec<String>,
    as_json: bool,
}

fn parse(args: &[String]) -> Result<Options, CommandError> {
    let target = args.first().ok_or_else(|| CommandError::usage(USAGE))?;
    let trajectory = target
        .parse()
        .map_err(|error: agent_jit_domain::ids::IdError| {
            CommandError::new(error.code(), error.to_string(), ExitClass::Usage)
        })?;
    let mut status = None;
    let mut actor = None;
    let mut rationale = None;
    let mut evidence = Vec::new();
    let mut as_json = false;
    let mut remaining = args[1..].iter();
    while let Some(argument) = remaining.next() {
        match argument.as_str() {
            "--outcome" => status = Some(parse_status(value(&mut remaining)?)?),
            "--actor" => actor = Some(value(&mut remaining)?.clone()),
            "--rationale" => rationale = Some(value(&mut remaining)?.clone()),
            "--evidence" => evidence.push(value(&mut remaining)?.clone()),
            "--json" => as_json = true,
            _ => return Err(CommandError::usage(USAGE)),
        }
    }
    Ok(Options {
        trajectory,
        status: status.ok_or_else(|| CommandError::usage(USAGE))?,
        actor: actor.ok_or_else(|| CommandError::usage(USAGE))?,
        rationale: rationale.ok_or_else(|| CommandError::usage(USAGE))?,
        evidence,
        as_json,
    })
}

fn value<'a>(remaining: &mut impl Iterator<Item = &'a String>) -> Result<&'a String, CommandError> {
    remaining.next().ok_or_else(|| CommandError::usage(USAGE))
}

fn parse_status(value: &str) -> Result<AnnotationStatus, CommandError> {
    match value {
        "succeeded" => Ok(AnnotationStatus::Succeeded),
        "failed" => Ok(AnnotationStatus::Failed),
        "abandoned" => Ok(AnnotationStatus::Abandoned),
        "unknown" => Ok(AnnotationStatus::Unknown),
        _ => Err(CommandError::refused(
            "outcome_status_invalid",
            format!("unsupported annotation outcome `{value}`"),
        )),
    }
}

fn status_name(status: AnnotationStatus) -> &'static str {
    match status {
        AnnotationStatus::Succeeded => "succeeded",
        AnnotationStatus::Failed => "failed",
        AnnotationStatus::Abandoned => "abandoned",
        AnnotationStatus::Unknown => "unknown",
    }
}

fn annotation_json(record: &Envelope<OutcomeAnnotation>) -> Value {
    json!({
        "annotation_id": record.id().to_string(),
        "revision": record.body().revision().get(),
        "status": status_name(record.body().status()),
        "actor": record.body().actor(),
        "annotated_at_unix_ms": record.body().annotated_at_unix_ms(),
        "rationale": record.body().rationale(),
        "evidence": record.body().evidence().iter().map(EvidenceRef::as_str).collect::<Vec<_>>(),
    })
}

fn annotation_error(error: agent_jit_domain::outcome::OutcomeAnnotationError) -> CommandError {
    CommandError::refused(error.code(), error.to_string())
}
