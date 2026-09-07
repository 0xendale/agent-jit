//! Append-only manual outcome annotation persistence.

use agent_jit_domain::envelope::Envelope;
use agent_jit_domain::ids::TrajectoryId;
use agent_jit_domain::outcome::OutcomeAnnotation;
use rusqlite::{OptionalExtension as _, TransactionBehavior, params};

use crate::{Store, StoreError, classify_write, decode, encode};

impl Store {
    /// Appends one immutable annotation and advances its trajectory's current pointer atomically.
    ///
    /// # Errors
    ///
    /// Returns a typed revision, duplicate, reference, encoding, or SQL error.
    pub fn append_outcome_annotation(
        &mut self,
        record: &Envelope<OutcomeAnnotation>,
    ) -> Result<(), StoreError> {
        let id = record.id().to_string();
        let trajectory = record.body().trajectory_id().to_string();
        let revision = record.body().revision().get();
        let (json, digest) = encode(record)?;
        let transaction = self
            .connection
            .transaction_with_behavior(TransactionBehavior::Immediate)?;

        let duplicate = transaction
            .query_row(
                "SELECT 1 FROM outcome_annotations WHERE annotation_id=?1",
                [&id],
                |_| Ok(()),
            )
            .optional()?
            .is_some();
        if duplicate {
            return Err(StoreError::AlreadyExists {
                kind: "outcome_annotation",
                id,
            });
        }

        let current: Option<i64> = transaction
            .query_row(
                "SELECT a.revision FROM outcome_annotation_current c \
                 JOIN outcome_annotations a ON a.annotation_id=c.annotation_id \
                 WHERE c.trajectory_id=?1",
                [&trajectory],
                |row| row.get(0),
            )
            .optional()?;
        let current = current
            .map(|value| {
                u32::try_from(value).map_err(|_| StoreError::RecordInvalid {
                    reason: format!("stored annotation revision {value} is outside u32"),
                })
            })
            .transpose()?
            .unwrap_or(0);
        let expected = current
            .checked_add(1)
            .ok_or(StoreError::AnnotationRevisionOverflow)?;
        if revision != expected {
            return Err(StoreError::AnnotationRevisionInvalid {
                current,
                found: revision,
            });
        }

        let inserted = transaction.execute(
            "INSERT INTO outcome_annotations (annotation_id,trajectory_id,revision,status,actor,\
             annotated_at_unix_ms,rationale,record_json,digest,written_at) \
             VALUES (?1,?2,?3,?4,?5,?6,?7,?8,?9,?10)",
            params![
                id,
                trajectory,
                revision,
                format!("{:?}", record.body().status()),
                record.body().actor(),
                record.body().annotated_at_unix_ms(),
                record.body().rationale(),
                json,
                digest,
                self.clock.now_unix_ms(),
            ],
        );
        classify_write(inserted, "outcome_annotation", &id)?;
        transaction.execute(
            "INSERT INTO outcome_annotation_current (trajectory_id,annotation_id,revision) \
             VALUES (?1,?2,?3) ON CONFLICT(trajectory_id) DO UPDATE SET \
             annotation_id=excluded.annotation_id,revision=excluded.revision",
            params![trajectory, id, revision],
        )?;
        transaction.commit()?;
        Ok(())
    }

    /// Lists a trajectory's annotation history in revision order.
    ///
    /// # Errors
    ///
    /// Returns [`StoreError`] when SQL or a stored contract is invalid.
    pub fn list_outcome_annotations(
        &self,
        trajectory_id: &TrajectoryId,
    ) -> Result<Vec<Envelope<OutcomeAnnotation>>, StoreError> {
        let mut statement = self.connection.prepare(
            "SELECT record_json FROM outcome_annotations WHERE trajectory_id=?1 \
             ORDER BY revision,annotation_id",
        )?;
        let rows =
            statement.query_map([trajectory_id.to_string()], |row| row.get::<_, String>(0))?;
        let mut annotations = Vec::new();
        for row in rows {
            annotations.push(decode(&row?)?);
        }
        Ok(annotations)
    }

    /// Loads the annotation currently selected for a trajectory.
    ///
    /// # Errors
    ///
    /// Returns [`StoreError`] when SQL or the stored contract is invalid.
    pub fn current_outcome_annotation(
        &self,
        trajectory_id: &TrajectoryId,
    ) -> Result<Option<Envelope<OutcomeAnnotation>>, StoreError> {
        let json = self
            .connection
            .query_row(
                "SELECT a.record_json FROM outcome_annotation_current c \
                 JOIN outcome_annotations a ON a.annotation_id=c.annotation_id \
                 WHERE c.trajectory_id=?1",
                [trajectory_id.to_string()],
                |row| row.get::<_, String>(0),
            )
            .optional()?;
        json.map(|value| decode(&value)).transpose()
    }
}
