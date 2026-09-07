//! Typed retention-protection links not represented by existing record bodies.

use agent_jit_domain::ids::{BenchmarkId, CandidateId, FrozenCorpusId, TrajectoryId};
use rusqlite::{TransactionBehavior, params};

use crate::{Store, StoreError, classify_write};

impl Store {
    /// Links a trajectory into a minimally materialized frozen corpus.
    ///
    /// # Errors
    ///
    /// Returns [`StoreError`] when the trajectory is absent or the link already exists.
    pub fn add_frozen_corpus_reference(
        &mut self,
        corpus_id: &FrozenCorpusId,
        trajectory_id: &TrajectoryId,
    ) -> Result<(), StoreError> {
        let transaction = self
            .connection
            .transaction_with_behavior(TransactionBehavior::Immediate)?;
        transaction.execute(
            "INSERT OR IGNORE INTO frozen_corpora (corpus_id) VALUES (?1)",
            [corpus_id.to_string()],
        )?;
        let inserted = transaction.execute(
            "INSERT INTO frozen_corpus_trajectories (corpus_id,trajectory_id) VALUES (?1,?2)",
            params![corpus_id.to_string(), trajectory_id.to_string()],
        );
        classify_write(
            inserted,
            "frozen_corpus_trajectory",
            &trajectory_id.to_string(),
        )?;
        transaction.commit()?;
        Ok(())
    }

    /// Links a persisted candidate to trajectory evidence.
    ///
    /// # Errors
    ///
    /// Returns [`StoreError`] when either record is absent or the link already exists.
    pub fn link_candidate_trajectory(
        &mut self,
        candidate_id: &CandidateId,
        trajectory_id: &TrajectoryId,
    ) -> Result<(), StoreError> {
        let result = self.connection.execute(
            "INSERT INTO candidate_trajectories (candidate_id,trajectory_id) VALUES (?1,?2)",
            params![candidate_id.to_string(), trajectory_id.to_string()],
        );
        classify_write(result, "candidate_trajectory", &trajectory_id.to_string())
    }

    /// Links a persisted benchmark to trajectory evidence.
    ///
    /// # Errors
    ///
    /// Returns [`StoreError`] when either record is absent or the link already exists.
    pub fn link_benchmark_trajectory(
        &mut self,
        benchmark_id: &BenchmarkId,
        trajectory_id: &TrajectoryId,
    ) -> Result<(), StoreError> {
        let result = self.connection.execute(
            "INSERT INTO benchmark_trajectories (benchmark_id,trajectory_id) VALUES (?1,?2)",
            params![benchmark_id.to_string(), trajectory_id.to_string()],
        );
        classify_write(result, "benchmark_trajectory", &trajectory_id.to_string())
    }
}
