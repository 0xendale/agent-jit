#![allow(dead_code)]

use std::path::Path;

use agent_jit_domain::canonical::digest_of;
use agent_jit_domain::envelope::{Envelope, Provenance};
use agent_jit_domain::ids::{EventId, OutcomeId, RepositoryId, SessionId, TrajectoryId};
use agent_jit_domain::metrics::{
    Measured, MetricProvenance, RecorderHealth, TraceMetrics, Unknown,
};
use agent_jit_domain::outcome::{CostMetrics, Outcome, OutcomeStatus};
use agent_jit_domain::trace::{Event, EventKind, Repository, Runtime, Session, Trajectory};
use agent_jit_store::{FixedClock, Store, StorePath};

pub const NOW: i64 = 1_800_000_000_000;
pub const DAY_MS: i64 = 86_400_000;

pub struct Fixture {
    pub store: Store,
    pub repository_id: RepositoryId,
}

impl Fixture {
    pub fn new(directory: &Path) -> Self {
        let path = StorePath::new(&directory.join("state.sqlite3")).unwrap();
        let mut store = Store::open(&path).unwrap().with_clock(FixedClock::at(NOW));
        store.migrate().unwrap();
        let identity = digest_of(&serde_json::json!({"repo": "fixture"})).unwrap();
        let repository = Envelope::new(
            RepositoryId::derived(&identity),
            Provenance::recorded_by("agent-jit/0.1.0"),
            Repository {
                git_common_dir: "/redacted/.git".to_owned(),
                identity_digest: identity,
                label: "fixture".to_owned(),
            },
        );
        let repository_id = *repository.id();
        store.put_repository(&repository).unwrap();
        Self {
            store,
            repository_id,
        }
    }

    pub fn add_session(&mut self, suffix: char, ended: i64) -> (SessionId, TrajectoryId) {
        let session_id = id::<SessionId>("ses", suffix);
        let trajectory_id = id::<TrajectoryId>("trj", suffix);
        let session = Envelope::new(
            session_id,
            Provenance::recorded_by("agent-jit/0.1.0"),
            Session {
                repository_id: self.repository_id,
                worktree_root: "/redacted".to_owned(),
                head_commit: "0".repeat(40),
                runtime: Runtime::ClaudeCode,
                runtime_version: "2.0.0".to_owned(),
                model_id: "model".to_owned(),
                recorder_version: "agent-jit/0.1.0".to_owned(),
                started_at_unix_ms: ended - 1_000,
                ended_at_unix_ms: Some(ended),
            },
        );
        self.store.put_session(&session).unwrap();
        let mut trajectory = Trajectory::sample();
        trajectory.session_id = session_id;
        trajectory.repository_id = self.repository_id;
        trajectory.intent = format!("intent {suffix}");
        trajectory.started_at_unix_ms = ended - 1_000;
        self.store
            .put_trajectory(&Envelope::new(
                trajectory_id,
                Provenance::recorded_by("agent-jit/0.1.0"),
                trajectory,
            ))
            .unwrap();
        (session_id, trajectory_id)
    }

    pub fn add_active_session(&mut self, suffix: char, started: i64) -> (SessionId, TrajectoryId) {
        let session_id = id::<SessionId>("ses", suffix);
        let trajectory_id = id::<TrajectoryId>("trj", suffix);
        self.store
            .put_session(&Envelope::new(
                session_id,
                Provenance::recorded_by("agent-jit/0.1.0"),
                Session {
                    repository_id: self.repository_id,
                    worktree_root: "/redacted".to_owned(),
                    head_commit: "0".repeat(40),
                    runtime: Runtime::ClaudeCode,
                    runtime_version: "2.0.0".to_owned(),
                    model_id: "model".to_owned(),
                    recorder_version: "agent-jit/0.1.0".to_owned(),
                    started_at_unix_ms: started,
                    ended_at_unix_ms: None,
                },
            ))
            .unwrap();
        let mut trajectory = Trajectory::sample();
        trajectory.session_id = session_id;
        trajectory.repository_id = self.repository_id;
        trajectory.intent = format!("intent {suffix}");
        trajectory.started_at_unix_ms = started;
        self.store
            .put_trajectory(&Envelope::new(
                trajectory_id,
                Provenance::recorded_by("agent-jit/0.1.0"),
                trajectory,
            ))
            .unwrap();
        (session_id, trajectory_id)
    }

    pub fn add_children(&mut self, suffix: char, session: SessionId, trajectory: TrajectoryId) {
        let payload = digest_of(&serde_json::json!({"payload": suffix})).unwrap();
        let event = Envelope::new(
            id::<EventId>("evt", suffix),
            Provenance::recorded_by("agent-jit/0.1.0"),
            Event {
                session_id: session,
                sequence: 0,
                kind: EventKind::ToolCall,
                tool_name: Some("Bash".to_owned()),
                argv: vec!["cargo".to_owned(), "test".to_owned()],
                exit_code: None,
                payload_digest: Some(payload),
                payload_bytes: 10,
                payload_truncated: false,
                started_at_unix_ms: NOW,
                duration_ms: 10,
            },
        );
        self.store.put_event(&event).unwrap();
        let outcome = Envelope::new(
            id::<OutcomeId>("out", suffix),
            Provenance::recorded_by("agent-jit/0.1.0"),
            Outcome {
                trajectory_id: trajectory,
                status: OutcomeStatus::Solved,
                note: None,
                metrics: CostMetrics {
                    tool_calls: 1,
                    agent_turns: 1,
                    estimated_input_tokens: 3,
                    estimated_output_tokens: 2,
                    duration_ms: 10,
                },
                confirmed_by_human: true,
            },
        );
        self.store.put_outcome(&outcome).unwrap();
        self.store
            .put_trace_metrics(&trajectory, &metrics(self.repository_id, session))
            .unwrap();
    }
}

fn metrics(repo: RepositoryId, session: SessionId) -> TraceMetrics {
    TraceMetrics {
        active_duration_ms: Measured::exact(10, "events"),
        wall_duration_ms: Measured::exact(10, "events"),
        turns: 1,
        agent_tool_calls: 1,
        failed_tool_calls: 0,
        observed_bytes: 10,
        estimated_input_tokens: Measured::estimated(3, "bytes"),
        exact_input_tokens: Measured::unknown(Unknown::NoVersionedSource),
        health: RecorderHealth {
            truncated_payloads: 0,
            quarantined_segments: 0,
            duplicates_collapsed: 0,
        },
        provenance: MetricProvenance {
            computed_from: "events".to_owned(),
            repository_id: repo,
            session_id: session,
            head_commit: "0".repeat(40),
            event_count: 1,
            adapter_schema: Some("fixture.v1".to_owned()),
            runtime_version: Some("2.0.0".to_owned()),
            model_id: Some("model".to_owned()),
        },
    }
}

fn id<T: std::str::FromStr>(prefix: &str, suffix: char) -> T
where
    T::Err: std::fmt::Debug,
{
    format!("{prefix}_01J0000000000000000000000{suffix}")
        .parse()
        .unwrap()
}
