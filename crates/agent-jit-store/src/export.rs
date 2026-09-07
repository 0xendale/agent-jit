//! Consistent, validated repository export snapshots.

use std::collections::BTreeMap;

use agent_jit_domain::canonical::{Digest, digest_of, to_value};
use agent_jit_domain::envelope::Envelope;
use agent_jit_domain::ids::{OutcomeAnnotationId, RepositoryId, TrajectoryId};
use agent_jit_domain::metrics::TraceMetrics;
use agent_jit_domain::outcome::OutcomeAnnotation;
use agent_jit_domain::schema::validate_document;
use rusqlite::{Transaction, params};
use serde_json::Value;

use crate::{Store, StoreError};

/// One validated envelope in an export snapshot.
#[derive(Debug, Clone)]
pub struct ExportRecord {
    /// Contract schema name.
    pub schema: String,
    /// Exact schema version.
    pub version: u32,
    /// Branded record identifier.
    pub id: String,
    /// Canonical semantic digest.
    pub digest: Digest,
    /// Complete redacted envelope.
    pub document: Value,
    /// Derived trajectory metrics, when this is a trajectory.
    pub metrics: Option<Value>,
}

/// Repository records and mutable annotation pointers read from one transaction.
#[derive(Debug, Clone)]
pub struct ExportSnapshot {
    /// Exported repository.
    pub repository_id: RepositoryId,
    /// Records sorted by schema and identifier.
    pub records: Vec<ExportRecord>,
    /// Explicit current annotation selected for each trajectory.
    pub current_annotations: BTreeMap<TrajectoryId, OutcomeAnnotationId>,
}

impl Store {
    /// Reads and validates one repository's export state in a consistent transaction.
    ///
    /// # Errors
    ///
    /// Returns a typed store error when SQL or any stored envelope is invalid.
    pub fn export_snapshot(
        &mut self,
        repository_id: &RepositoryId,
    ) -> Result<ExportSnapshot, StoreError> {
        let transaction = self.connection.transaction()?;
        let mut records = load_records(&transaction, repository_id)?;
        let metrics = load_metrics(&transaction, repository_id)?;
        for record in &mut records {
            record.metrics = metrics.get(&record.id).cloned();
        }
        records.sort_by(|left, right| (&left.schema, &left.id).cmp(&(&right.schema, &right.id)));
        let current_annotations = load_current(&transaction, repository_id)?;
        transaction.commit()?;
        Ok(ExportSnapshot {
            repository_id: *repository_id,
            records,
            current_annotations,
        })
    }
}

fn load_records(
    transaction: &Transaction<'_>,
    repository_id: &RepositoryId,
) -> Result<Vec<ExportRecord>, StoreError> {
    let mut statement = transaction.prepare(
        "SELECT record_json FROM repositories WHERE repo_id=?1 UNION ALL \
         SELECT record_json FROM sessions WHERE repo_id=?1 UNION ALL \
         SELECT e.record_json FROM events e JOIN sessions s ON s.session_id=e.session_id \
           WHERE s.repo_id=?1 UNION ALL \
         SELECT record_json FROM trajectories WHERE repo_id=?1 UNION ALL \
         SELECT o.record_json FROM outcomes o JOIN trajectories t \
           ON t.trajectory_id=o.trajectory_id WHERE t.repo_id=?1 UNION ALL \
         SELECT a.record_json FROM outcome_annotations a JOIN trajectories t \
           ON t.trajectory_id=a.trajectory_id WHERE t.repo_id=?1",
    )?;
    let rows = statement.query_map(params![repository_id.to_string()], |row| {
        row.get::<_, String>(0)
    })?;
    let mut records = Vec::new();
    for row in rows {
        let document: Value = serde_json::from_str(&row?).map_err(invalid)?;
        let validated = validate_document(&document).map_err(invalid)?;
        records.push(ExportRecord {
            schema: validated.schema_name.to_owned(),
            version: validated.version,
            id: validated.id,
            digest: validated.digest,
            document,
            metrics: None,
        });
    }
    Ok(records)
}

fn load_metrics(
    transaction: &Transaction<'_>,
    repository_id: &RepositoryId,
) -> Result<BTreeMap<String, Value>, StoreError> {
    let mut statement = transaction.prepare(
        "SELECT m.trajectory_id,m.session_id,m.record_json,m.digest,t.session_id,t.head_commit,\
         s.repo_id,s.head_commit,s.runtime_version,s.model_id FROM trace_metrics m \
         JOIN trajectories t ON t.trajectory_id=m.trajectory_id \
         JOIN sessions s ON s.session_id=t.session_id \
         WHERE t.repo_id=?1 ORDER BY m.trajectory_id",
    )?;
    let rows = statement.query_map([repository_id.to_string()], |row| {
        Ok((
            row.get::<_, String>(0)?,
            row.get::<_, String>(1)?,
            row.get::<_, String>(2)?,
            row.get::<_, String>(3)?,
            row.get::<_, String>(4)?,
            row.get::<_, String>(5)?,
            row.get::<_, String>(6)?,
            row.get::<_, String>(7)?,
            row.get::<_, String>(8)?,
            row.get::<_, String>(9)?,
        ))
    })?;
    let mut metrics = BTreeMap::new();
    for row in rows {
        let (
            id,
            metric_session,
            document,
            stored_digest,
            trajectory_session,
            trajectory_commit,
            session_repository,
            session_commit,
            runtime_version,
            model_id,
        ) = row?;
        let typed: TraceMetrics = serde_json::from_str(&document).map_err(invalid)?;
        let value = to_value(&typed).map_err(invalid)?;
        let actual_digest = digest_of(&value).map_err(invalid)?;
        let declared_digest: Digest = stored_digest.parse().map_err(invalid)?;
        let provenance = &typed.provenance;
        if actual_digest != declared_digest
            || metric_session != trajectory_session
            || provenance.session_id.to_string() != trajectory_session
            || provenance.repository_id != *repository_id
            || session_repository != repository_id.to_string()
            || provenance.head_commit != trajectory_commit
            || provenance.head_commit != session_commit
            || provenance
                .runtime_version
                .as_deref()
                .is_some_and(|value| value != runtime_version)
            || provenance
                .model_id
                .as_deref()
                .is_some_and(|value| value != model_id)
        {
            return Err(invalid("trace metrics ownership or digest mismatch"));
        }
        metrics.insert(id, value);
    }
    Ok(metrics)
}

fn load_current(
    transaction: &Transaction<'_>,
    repository_id: &RepositoryId,
) -> Result<BTreeMap<TrajectoryId, OutcomeAnnotationId>, StoreError> {
    let mut statement = transaction.prepare(
        "SELECT c.trajectory_id,c.annotation_id,c.revision,a.trajectory_id,a.revision,\
         a.record_json,a.digest \
         FROM outcome_annotation_current c \
         JOIN outcome_annotations a ON a.annotation_id=c.annotation_id \
         JOIN trajectories t ON t.trajectory_id=c.trajectory_id WHERE t.repo_id=?1 \
         ORDER BY c.trajectory_id",
    )?;
    let rows = statement.query_map([repository_id.to_string()], |row| {
        Ok((
            row.get::<_, String>(0)?,
            row.get::<_, String>(1)?,
            row.get::<_, i64>(2)?,
            row.get::<_, String>(3)?,
            row.get::<_, i64>(4)?,
            row.get::<_, String>(5)?,
            row.get::<_, String>(6)?,
        ))
    })?;
    let mut current = BTreeMap::new();
    for row in rows {
        let (
            trajectory,
            annotation,
            revision,
            owner_trajectory,
            owner_revision,
            document,
            stored_digest,
        ) = row?;
        let trajectory: TrajectoryId = trajectory.parse().map_err(invalid)?;
        let annotation: OutcomeAnnotationId = annotation.parse().map_err(invalid)?;
        let owner_trajectory: TrajectoryId = owner_trajectory.parse().map_err(invalid)?;
        let revision = u32::try_from(revision).map_err(invalid)?;
        let owner_revision = u32::try_from(owner_revision).map_err(invalid)?;
        let typed: Envelope<OutcomeAnnotation> =
            serde_json::from_str(&document).map_err(invalid)?;
        let declared_digest: Digest = stored_digest.parse().map_err(invalid)?;
        let actual_digest = typed.digest().map_err(invalid)?;
        if actual_digest != declared_digest
            || typed.id() != &annotation
            || typed.body().trajectory_id() != trajectory
            || typed.body().trajectory_id() != owner_trajectory
            || typed.body().revision().get() != revision
            || typed.body().revision().get() != owner_revision
        {
            return Err(invalid("current annotation ownership mismatch"));
        }
        current.insert(trajectory, annotation);
    }
    Ok(current)
}

fn invalid(error: impl std::fmt::Display) -> StoreError {
    StoreError::RecordInvalid {
        reason: error.to_string(),
    }
}
