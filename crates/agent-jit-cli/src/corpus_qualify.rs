//! `agent-jit corpus qualify` — the Phase 0 acceptance instrument.
//!
//! The store vouches for every fact the verdict rests on: `export_snapshot` re-validates each
//! stored envelope's digest and its cross-record ownership in one transaction, and this command
//! only filters the snapshot that survived. A trajectory below the bar never joins the
//! denominator silently — every exclusion carries a stable reason.

use std::collections::{BTreeMap, BTreeSet};
use std::path::Path;

use agent_jit_domain::ids::TrajectoryId;
use agent_jit_engine::process::ProcessRunner;
use agent_jit_engine::repository::discover;
use agent_jit_store::ExportSnapshot;
use serde_json::{Value, json};

use crate::error::{CommandError, ExitClass};
use crate::output::Rendered;

const USAGE: &str =
    "usage: agent-jit corpus qualify --repo <path> [--min <n>] [--max <n>] [--json]";

/// The Phase 0 accrual target.
const DEFAULT_MIN: u32 = crate::corpus::REQUIRED_TRAJECTORIES;
/// The spec's Phase 0 window is 50-100.
const DEFAULT_MAX: u32 = 100;

const SESSION_SCHEMA: &str = "agent_jit.session";
const EVENT_SCHEMA: &str = "agent_jit.event";
const TRAJECTORY_SCHEMA: &str = "agent_jit.trajectory";
const ANNOTATION_SCHEMA: &str = "agent_jit.outcome_annotation";

/// Provenance sources that carry the experiment's meaning: captured live, or imported from an
/// exact-shape historical transcript. A derived record was manufactured from other records and
/// is never the evidence Phase 0 counts.
const MAIN_SESSION_SOURCES: &[&str] = &["recorded", "imported"];

/// Why one trajectory sits outside the qualifying corpus.
const MISSING_SESSION: &str = "missing_session";
const INCOMPLETE: &str = "incomplete";
const PROVENANCE: &str = "provenance";
const MISSING_METRICS: &str = "missing_metrics";
const RECORDER_LOSS: &str = "recorder_loss";
const MISSING_ANNOTATION: &str = "missing_annotation";
const OUTCOME_UNKNOWN: &str = "outcome_unknown";
const DUPLICATE_CONTENT: &str = "duplicate_content";

struct Options {
    repo: String,
    min: u32,
    max: u32,
    as_json: bool,
}

pub(crate) fn run(args: &[String]) -> Result<Rendered, CommandError> {
    let options = parse(args)?;
    let identity = discover(&ProcessRunner::new(), Path::new(&options.repo)).map_err(|error| {
        CommandError::new(error.code(), error.to_string(), ExitClass::SafetyRefusal)
    })?;
    let mut store = crate::trace::open_store()?;
    let snapshot = store
        .export_snapshot(&identity.repo_id)
        .map_err(|error| crate::trace::refusal(&error))?;

    let verdict = assess(&snapshot);
    let report = json!({
        "repository_id": identity.repo_id.to_string(),
        "min": options.min,
        "max": options.max,
        "recorded_trajectories": verdict.recorded,
        "qualifying_count": verdict.qualifying_count,
        "main_session_count": verdict.main_session_count,
        "duplicate_count": verdict.duplicate_count,
        "exclusion_counts": verdict.exclusion_counts,
        "sufficient": verdict.qualifying_count >= options.min,
    });
    let excluded = verdict.excluded.clone();

    if verdict.qualifying_count < options.min {
        return Err(stop(
            "corpus_insufficient",
            "the qualifying corpus is below the demanded minimum",
            options.min,
            options.max,
            &verdict,
            &excluded,
        ));
    }
    if verdict.qualifying_count > options.max {
        return Err(stop(
            "corpus_exceeds_max",
            "the qualifying corpus is above the demanded maximum",
            options.min,
            options.max,
            &verdict,
            &excluded,
        ));
    }
    if options.as_json {
        return Ok(Rendered::Json(report));
    }
    Ok(Rendered::Text(format!(
        "{} qualifying of {} recorded ({} excluded, {} duplicate), the accepted range is {}..{}\n",
        verdict.qualifying_count,
        verdict.recorded,
        verdict
            .recorded
            .saturating_sub(verdict.qualifying_count)
            .saturating_sub(verdict.duplicate_count),
        verdict.duplicate_count,
        options.min,
        options.max,
    )))
}

/// A documented Phase 0 stop, not a defect.
fn stop(
    code: &str,
    reason: &str,
    min: u32,
    max: u32,
    verdict: &Verdict,
    excluded: &[Value],
) -> CommandError {
    CommandError::new(code, reason, ExitClass::GateStop).with_details(json!({
        "min": min,
        "max": max,
        "recorded_trajectories": verdict.recorded,
        "qualifying_count": verdict.qualifying_count,
        "main_session_count": verdict.main_session_count,
        "duplicate_count": verdict.duplicate_count,
        "exclusion_counts": verdict.exclusion_counts,
        "excluded": excluded,
    }))
}

/// One snapshot's verdict.
struct Verdict {
    recorded: u32,
    qualifying_count: u32,
    main_session_count: u32,
    duplicate_count: u32,
    exclusion_counts: BTreeMap<String, u32>,
    excluded: Vec<Value>,
}

/// The session-independent content shape per session: every event's classification with the
/// session identity removed, so a replayed transcript under a new id is still recognisable.
fn event_shapes_of(snapshot: &ExportSnapshot) -> BTreeMap<String, Vec<String>> {
    let mut event_shapes: BTreeMap<String, Vec<String>> = BTreeMap::new();
    for record in &snapshot.records {
        if record.schema != EVENT_SCHEMA {
            continue;
        }
        let body = &record.document["body"];
        let session = body["session_id"].as_str().unwrap_or_default().to_owned();
        event_shapes.entry(session).or_default().push(format!(
            "{}:{}:{}:{}",
            body["kind"].as_str().unwrap_or_default(),
            body["payload_bytes"].as_u64().unwrap_or_default(),
            body["payload_truncated"].as_bool().unwrap_or_default(),
            body["tool_name"].as_str().unwrap_or_default(),
        ));
    }
    event_shapes
}

fn assess(snapshot: &ExportSnapshot) -> Verdict {
    let mut verdict = Verdict {
        recorded: 0,
        qualifying_count: 0,
        main_session_count: 0,
        duplicate_count: 0,
        exclusion_counts: BTreeMap::new(),
        excluded: Vec::new(),
    };

    let mut sessions: BTreeMap<String, &Value> = BTreeMap::new();
    let mut annotation_status: BTreeMap<String, Value> = BTreeMap::new();
    let mut metrics_of: BTreeMap<String, Option<&Value>> = BTreeMap::new();
    for record in &snapshot.records {
        match record.schema.as_str() {
            SESSION_SCHEMA => {
                if let Some(id) = record.document["id"].as_str() {
                    sessions.insert(id.to_owned(), &record.document);
                }
            }
            ANNOTATION_SCHEMA => {
                if let Some(id) = record.document["id"].as_str() {
                    annotation_status
                        .insert(id.to_owned(), record.document["body"]["status"].clone());
                }
            }
            TRAJECTORY_SCHEMA => {
                if let Some(id) = record.document["id"].as_str() {
                    metrics_of.insert(id.to_owned(), record.metrics.as_ref());
                }
            }
            _ => {}
        }
    }

    // Session-independent content shape per session: every event's classification with the
    // session identity removed, so a replayed transcript under a new id is still recognisable.
    let event_shapes = event_shapes_of(snapshot);

    let mut pending: Vec<(String, String)> = Vec::new();
    for record in &snapshot.records {
        if record.schema != TRAJECTORY_SCHEMA {
            continue;
        }
        verdict.recorded += 1;
        let id = record.document["id"]
            .as_str()
            .unwrap_or_default()
            .to_owned();
        let session_id = record.document["body"]["session_id"]
            .as_str()
            .unwrap_or_default()
            .to_owned();
        if let Some(reason) = classify(
            &record.document,
            record.metrics.as_ref(),
            &sessions,
            snapshot,
            &annotation_status,
            &id,
        ) {
            *verdict
                .exclusion_counts
                .entry(reason.to_owned())
                .or_default() += 1;
            verdict.excluded.push(json!({
                "trajectory_id": id,
                "session_id": session_id,
                "reason": reason,
            }));
        } else {
            pending.push((id, session_id));
        }
    }

    // The first occurrence of a content shape qualifies; every later identical one is a duplicate.
    let mut seen: BTreeSet<String> = BTreeSet::new();
    for (id, session_id) in pending {
        let key = content_key(&session_id, &sessions, &event_shapes);
        if seen.contains(&key) {
            verdict.duplicate_count += 1;
            *verdict
                .exclusion_counts
                .entry(DUPLICATE_CONTENT.to_owned())
                .or_default() += 1;
            verdict.excluded.push(json!({
                "trajectory_id": id,
                "session_id": session_id,
                "reason": DUPLICATE_CONTENT,
            }));
            continue;
        }
        seen.insert(key);
        verdict.qualifying_count += 1;
        verdict.main_session_count += 1;
    }

    verdict
}

fn classify(
    document: &Value,
    metrics: Option<&Value>,
    sessions: &BTreeMap<String, &Value>,
    snapshot: &ExportSnapshot,
    annotation_status: &BTreeMap<String, Value>,
    id: &str,
) -> Option<&'static str> {
    let session_id = document["body"]["session_id"].as_str().unwrap_or_default();
    let Some(session) = sessions.get(session_id) else {
        return Some(MISSING_SESSION);
    };
    let session_source = session["provenance"]["source"].as_str().unwrap_or_default();
    let trajectory_source = document["provenance"]["source"]
        .as_str()
        .unwrap_or_default();
    if !MAIN_SESSION_SOURCES.contains(&session_source)
        || !MAIN_SESSION_SOURCES.contains(&trajectory_source)
    {
        return Some(PROVENANCE);
    }
    if session["body"]["ended_at_unix_ms"].as_i64().is_none() {
        return Some(INCOMPLETE);
    }
    let Some(metrics) = metrics else {
        return Some(MISSING_METRICS);
    };
    let health = &metrics["health"];
    let lossy = health["truncated_payloads"].as_u64().unwrap_or_default() > 0
        || health["quarantined_segments"].as_u64().unwrap_or_default() > 0;
    if lossy {
        return Some(RECORDER_LOSS);
    }

    let Ok(trajectory_id) = id.parse::<TrajectoryId>() else {
        return Some(MISSING_ANNOTATION);
    };
    let Some(annotation_id) = snapshot.current_annotations.get(&trajectory_id) else {
        return Some(MISSING_ANNOTATION);
    };
    if annotation_status
        .get(annotation_id.to_string().as_str())
        .and_then(Value::as_str)
        == Some("unknown")
    {
        return Some(OUTCOME_UNKNOWN);
    }
    None
}

/// The session-independent content key that recognises a replayed transcript: the intent, the
/// checked-out commit, and the ordered multiset of each event's classification.
fn content_key(
    session_id: &str,
    sessions: &BTreeMap<String, &Value>,
    event_shapes: &BTreeMap<String, Vec<String>>,
) -> String {
    let mut shapes = event_shapes.get(session_id).cloned().unwrap_or_default();
    shapes.sort();
    let (intent, head_commit) = sessions.get(session_id).map_or(("", ""), |session| {
        (
            session["body"]["intent"].as_str().unwrap_or_default(),
            session["body"]["head_commit"].as_str().unwrap_or_default(),
        )
    });
    format!(
        "{intent}#{head_commit}#{}#{}",
        shapes.len(),
        shapes.join("|")
    )
}

fn parse(args: &[String]) -> Result<Options, CommandError> {
    let mut repo = None;
    let mut min = None;
    let mut max = None;
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
            "--min" => {
                min = Some(
                    remaining
                        .next()
                        .ok_or_else(|| CommandError::usage(USAGE))?
                        .parse()
                        .map_err(|_| CommandError::usage(USAGE))?,
                );
            }
            "--max" => {
                max = Some(
                    remaining
                        .next()
                        .ok_or_else(|| CommandError::usage(USAGE))?
                        .parse()
                        .map_err(|_| CommandError::usage(USAGE))?,
                );
            }
            "--json" => as_json = true,
            _ => return Err(CommandError::usage(USAGE)),
        }
    }
    let min = min.unwrap_or(DEFAULT_MIN);
    let max = max.unwrap_or(DEFAULT_MAX);
    if min == 0 || min > max {
        return Err(CommandError::usage(USAGE));
    }
    Ok(Options {
        repo: repo.ok_or_else(|| CommandError::usage(USAGE))?,
        min,
        max,
        as_json,
    })
}
