//! Deterministic digest of logical persisted evidence.

use agent_jit_domain::benchmark::BenchmarkRecord;
use agent_jit_domain::candidate::{CandidateContract, Group};
use agent_jit_domain::canonical::{Digest, digest_of, to_value};
use agent_jit_domain::capability::{CapabilityVersion, Invocation, ReplayResult, Workflow};
use agent_jit_domain::envelope::{Envelope, Record};
use agent_jit_domain::metrics::TraceMetrics;
use agent_jit_domain::outcome::{Outcome, OutcomeAnnotation};
use agent_jit_domain::trace::{Event, Repository, Session, Trajectory};
use rusqlite::Connection;
use serde_json::{Value, json};

use crate::{Store, StoreError, decode};

impl Store {
    /// Digests logical records, mutable pointers, protection links, holds, and audits.
    ///
    /// Physical database layout, free pages, WAL state, and file size are excluded.
    ///
    /// # Errors
    ///
    /// Returns [`StoreError`] when logical evidence cannot be read or canonicalized.
    pub fn evidence_digest(&self) -> Result<Digest, StoreError> {
        let transaction = self.connection.unchecked_transaction()?;
        let records = record_rows(&transaction)?;
        let state = state_rows(&transaction)?;
        let digest = digest_of(&json!({"records": records, "state": state}))?;
        transaction.commit()?;
        Ok(digest)
    }
}

fn record_rows(connection: &Connection) -> Result<Vec<Value>, StoreError> {
    let mut statement = connection.prepare(
        "SELECT kind,id,record_json,digest FROM (\
         SELECT 'repository' kind,repo_id id,record_json,digest FROM repositories UNION ALL \
         SELECT 'session',session_id,record_json,digest FROM sessions UNION ALL \
         SELECT 'event',event_id,record_json,digest FROM events UNION ALL \
         SELECT 'trajectory',trajectory_id,record_json,digest FROM trajectories UNION ALL \
         SELECT 'outcome',outcome_id,record_json,digest FROM outcomes UNION ALL \
         SELECT 'group',group_id,record_json,digest FROM groups UNION ALL \
         SELECT 'candidate',candidate_id,record_json,digest FROM candidate_versions UNION ALL \
         SELECT 'workflow',workflow_id,record_json,digest FROM workflows UNION ALL \
         SELECT 'replay',replay_id,record_json,digest FROM replays UNION ALL \
         SELECT 'capability',capability_id,record_json,digest FROM capability_versions UNION ALL \
         SELECT 'invocation',invocation_id,record_json,digest FROM invocations UNION ALL \
         SELECT 'benchmark',benchmark_id,record_json,digest FROM benchmark_records UNION ALL \
         SELECT 'trace_metrics',trajectory_id,record_json,digest FROM trace_metrics UNION ALL \
         SELECT 'outcome_annotation',annotation_id,record_json,digest FROM outcome_annotations) \
         ORDER BY kind,id",
    )?;
    let rows = statement.query_map([], |row| {
        Ok((
            row.get::<_, String>(0)?,
            row.get::<_, String>(1)?,
            row.get::<_, String>(2)?,
            row.get::<_, String>(3)?,
        ))
    })?;
    let mut values = Vec::new();
    for row in rows {
        let (kind, id, record_json, stored_digest) = row?;
        let digest = validated_digest(&kind, &record_json)?;
        if digest != stored_digest {
            return Err(StoreError::RecordInvalid {
                reason: format!("{kind} `{id}` digest does not match its typed record"),
            });
        }
        values.push(json!({"kind": kind, "id": id, "digest": digest}));
    }
    Ok(values)
}

fn state_rows(connection: &Connection) -> Result<Vec<Value>, StoreError> {
    let mut statement = connection.prepare(
        "SELECT kind,a,b,c,d,e,f FROM (\
         SELECT 'annotation_current' kind,trajectory_id a,annotation_id b,CAST(revision AS TEXT) c,\
                '' d,'' e,'' f \
           FROM outcome_annotation_current UNION ALL \
         SELECT 'session_hold',session_id,actor,rationale,CAST(recorded_at_unix_ms AS TEXT),'','' \
           FROM session_holds UNION ALL \
         SELECT 'corpus_audit',printf('%020d',audit_id),action,COALESCE(session_id,''),actor,rationale,\
                CAST(recorded_at_unix_ms AS TEXT) \
           FROM corpus_audit UNION ALL \
         SELECT 'frozen_corpus',corpus_id,'','','','','' FROM frozen_corpora UNION ALL \
         SELECT 'frozen_corpus_trajectory',corpus_id,trajectory_id,'','','','' \
           FROM frozen_corpus_trajectories UNION ALL \
         SELECT 'group_member',group_id,trajectory_id,'','','','' FROM group_members UNION ALL \
         SELECT 'candidate_trajectory',candidate_id,trajectory_id,'','','','' \
           FROM candidate_trajectories UNION ALL \
         SELECT 'benchmark_trajectory',benchmark_id,trajectory_id,'','','','' \
           FROM benchmark_trajectories UNION ALL \
         SELECT 'capability_active',name,capability_id,COALESCE(approved_by,''),\
                CAST(approved_at AS TEXT),'','' FROM capability_active UNION ALL \
         SELECT 'health_event',printf('%020d',health_id),code,detail,CAST(observed_at AS TEXT),'','' \
           FROM health_events) ORDER BY kind,a,b,c,d,e,f",
    )?;
    let rows = statement.query_map([], |row| {
        Ok(json!([
            row.get::<_, String>(0)?,
            row.get::<_, String>(1)?,
            row.get::<_, String>(2)?,
            row.get::<_, String>(3)?,
            row.get::<_, String>(4)?,
            row.get::<_, String>(5)?,
            row.get::<_, String>(6)?,
        ]))
    })?;
    let mut values = Vec::new();
    for row in rows {
        values.push(row?);
    }
    Ok(values)
}

fn validated_digest(kind: &str, json: &str) -> Result<String, StoreError> {
    match kind {
        "repository" => record_digest::<Repository>(json),
        "session" => record_digest::<Session>(json),
        "event" => record_digest::<Event>(json),
        "trajectory" => record_digest::<Trajectory>(json),
        "outcome" => record_digest::<Outcome>(json),
        "group" => record_digest::<Group>(json),
        "candidate" => record_digest::<CandidateContract>(json),
        "workflow" => record_digest::<Workflow>(json),
        "replay" => record_digest::<ReplayResult>(json),
        "capability" => record_digest::<CapabilityVersion>(json),
        "invocation" => record_digest::<Invocation>(json),
        "benchmark" => record_digest::<BenchmarkRecord>(json),
        "outcome_annotation" => record_digest::<OutcomeAnnotation>(json),
        "trace_metrics" => {
            let metrics: TraceMetrics =
                serde_json::from_str(json).map_err(|error| StoreError::RecordInvalid {
                    reason: error.to_string(),
                })?;
            Ok(digest_of(&to_value(&metrics)?)?.to_string())
        }
        _ => Err(StoreError::RecordInvalid {
            reason: format!("unknown logical evidence kind `{kind}`"),
        }),
    }
}

fn record_digest<T: Record>(json: &str) -> Result<String, StoreError> {
    let record: Envelope<T> = decode(json)?;
    Ok(record.digest()?.to_string())
}
