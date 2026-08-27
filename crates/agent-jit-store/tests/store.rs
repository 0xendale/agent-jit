#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
//! The store holds the only durable record of what was observed. It refuses anything it cannot
//! account for: a future schema, a symlinked database, a readable-by-others file, a half-applied
//! migration.

use std::os::unix::fs::PermissionsExt as _;
use std::path::Path;

use agent_jit_domain::envelope::{Envelope, Provenance};
use agent_jit_domain::ids::{RepositoryId, SessionId, TrajectoryId};
use agent_jit_domain::trace::{Repository, Runtime, Session, Trajectory};
use agent_jit_store::{CURRENT_SCHEMA_VERSION, FixedClock, Store, StoreError, StorePath};

fn store_path(directory: &Path) -> StorePath {
    StorePath::new(&directory.join("state.sqlite3")).unwrap()
}

fn open(directory: &Path) -> Store {
    let mut store = Store::open(&store_path(directory)).unwrap();
    store.migrate().unwrap();
    store
}

fn repository_record() -> Envelope<Repository> {
    let digest =
        agent_jit_domain::canonical::digest_of(&serde_json::json!({"repo": "pilot"})).unwrap();
    Envelope::new(
        RepositoryId::derived(&digest),
        Provenance::recorded_by("agent-jit/0.1.0"),
        Repository {
            git_common_dir: "/tmp/pilot/.git".to_owned(),
            identity_digest: digest,
            label: "pilot".to_owned(),
        },
    )
}

fn session_record(repository_id: RepositoryId, body: u8) -> Envelope<Session> {
    Envelope::new(
        SessionId::from_body(&format!("01J000000000000000000000{body:02}")).unwrap(),
        Provenance::recorded_by("agent-jit/0.1.0"),
        Session {
            repository_id,
            worktree_root: "/tmp/pilot".to_owned(),
            head_commit: "0".repeat(40),
            runtime: Runtime::ClaudeCode,
            runtime_version: "2.0.0".to_owned(),
            model_id: "claude-opus-5".to_owned(),
            recorder_version: "agent-jit/0.1.0".to_owned(),
            started_at_unix_ms: 1_756_000_000_000,
            ended_at_unix_ms: None,
        },
    )
}

#[test]
fn migrating_an_empty_database_reaches_the_current_schema() {
    let temp = tempfile::tempdir().unwrap();
    let mut store = Store::open(&store_path(temp.path())).unwrap();

    let report = store.migrate().unwrap();
    assert_eq!(report.from_version, 0);
    assert_eq!(report.to_version, CURRENT_SCHEMA_VERSION);
    assert!(report.applied > 0);
    assert_eq!(store.schema_version().unwrap(), CURRENT_SCHEMA_VERSION);
}

#[test]
fn migration_is_idempotent() {
    let temp = tempfile::tempdir().unwrap();
    let mut store = open(temp.path());

    let second = store.migrate().unwrap();
    assert_eq!(second.applied, 0);
    assert_eq!(second.from_version, CURRENT_SCHEMA_VERSION);
    assert_eq!(second.to_version, CURRENT_SCHEMA_VERSION);
}

#[test]
fn the_database_and_its_sidecars_are_private() {
    let temp = tempfile::tempdir().unwrap();
    let store = open(temp.path());
    drop(store);

    let database = temp.path().join("state.sqlite3");
    let mode = std::fs::metadata(&database).unwrap().permissions().mode() & 0o777;
    assert_eq!(mode, 0o600, "database mode was {mode:o}");

    for sidecar in ["state.sqlite3-wal", "state.sqlite3-shm"] {
        let path = temp.path().join(sidecar);
        if path.exists() {
            let mode = std::fs::metadata(&path).unwrap().permissions().mode() & 0o777;
            assert_eq!(mode, 0o600, "{sidecar} mode was {mode:o}");
        }
    }
}

#[test]
fn a_world_readable_database_is_refused() {
    let temp = tempfile::tempdir().unwrap();
    let store = open(temp.path());
    drop(store);

    let database = temp.path().join("state.sqlite3");
    std::fs::set_permissions(&database, std::fs::Permissions::from_mode(0o644)).unwrap();

    let error = Store::open(&StorePath::new(&database).unwrap()).unwrap_err();
    assert_eq!(error.code(), "store_permissions_too_open");

    // Refusing must not silently repair the file: the operator decides.
    let mode = std::fs::metadata(&database).unwrap().permissions().mode() & 0o777;
    assert_eq!(mode, 0o644);
}

#[test]
fn a_symlinked_database_is_refused_before_it_is_opened() {
    let temp = tempfile::tempdir().unwrap();
    let real = temp.path().join("real.sqlite3");
    std::fs::write(&real, b"").unwrap();
    let link = temp.path().join("state.sqlite3");
    std::os::unix::fs::symlink(&real, &link).unwrap();

    let error = StorePath::new(&link).unwrap_err();
    assert_eq!(error.code(), "store_path_symlink");
}

#[test]
fn a_future_schema_version_is_refused_rather_than_downgraded() {
    let temp = tempfile::tempdir().unwrap();
    let mut store = open(temp.path());
    store
        .force_schema_version_for_test(CURRENT_SCHEMA_VERSION + 1)
        .unwrap();
    drop(store);

    let error = Store::open(&store_path(temp.path())).unwrap_err();
    assert_eq!(error.code(), "store_schema_from_the_future");
}

#[test]
fn a_failed_migration_leaves_the_previous_schema_and_data_intact() {
    let temp = tempfile::tempdir().unwrap();
    let mut store = open(temp.path());
    let repository = repository_record();
    store.put_repository(&repository).unwrap();

    let error = store.apply_failing_migration_for_test().unwrap_err();
    assert_eq!(error.code(), "store_migration_failed");

    // Schema version unchanged, and the evidence written before the failure is still there.
    assert_eq!(store.schema_version().unwrap(), CURRENT_SCHEMA_VERSION);
    assert!(store.get_repository(repository.id()).unwrap().is_some());
    assert!(store.check().unwrap().healthy);
}

#[test]
fn integrity_and_foreign_keys_are_checked() {
    let temp = tempfile::tempdir().unwrap();
    let store = open(temp.path());

    let health = store.check().unwrap();
    assert!(health.healthy);
    assert_eq!(health.integrity, "ok");
    assert_eq!(health.foreign_key_violations, 0);
    assert_eq!(health.schema_version, CURRENT_SCHEMA_VERSION);
}

#[test]
fn a_session_referencing_an_unknown_repository_is_refused_by_sql() {
    let temp = tempfile::tempdir().unwrap();
    let mut store = open(temp.path());

    let orphan = session_record(repository_record().id().to_owned(), 1);
    let error = store.put_session(&orphan).unwrap_err();
    assert_eq!(error.code(), "store_constraint_violated");
}

#[test]
fn records_round_trip_through_the_store() {
    let temp = tempfile::tempdir().unwrap();
    let mut store = open(temp.path());

    let repository = repository_record();
    store.put_repository(&repository).unwrap();
    let session = session_record(repository.id().to_owned(), 1);
    store.put_session(&session).unwrap();

    let loaded = store.get_session(session.id()).unwrap().unwrap();
    assert_eq!(loaded, session);
    assert_eq!(
        store.get_repository(repository.id()).unwrap().unwrap(),
        repository
    );
}

#[test]
fn writing_the_same_record_twice_is_refused_because_records_are_immutable() {
    let temp = tempfile::tempdir().unwrap();
    let mut store = open(temp.path());

    let repository = repository_record();
    store.put_repository(&repository).unwrap();
    let error = store.put_repository(&repository).unwrap_err();
    assert_eq!(error.code(), "store_already_exists");
}

#[test]
fn listing_is_deterministically_ordered() {
    let temp = tempfile::tempdir().unwrap();
    let mut store = open(temp.path());
    let repository = repository_record();
    store.put_repository(&repository).unwrap();

    // Insert out of identifier order.
    for body in [3_u8, 1, 2] {
        store
            .put_session(&session_record(repository.id().to_owned(), body))
            .unwrap();
    }

    let first: Vec<String> = store
        .list_sessions(repository.id())
        .unwrap()
        .iter()
        .map(|session| session.id().to_string())
        .collect();
    let second: Vec<String> = store
        .list_sessions(repository.id())
        .unwrap()
        .iter()
        .map(|session| session.id().to_string())
        .collect();

    assert_eq!(first, second);
    let mut sorted = first.clone();
    sorted.sort();
    assert_eq!(first, sorted, "listing must be ordered by identifier");
}

#[test]
fn a_second_writer_waits_rather_than_failing_immediately() {
    let temp = tempfile::tempdir().unwrap();
    let mut first = open(temp.path());
    let mut second = Store::open(&store_path(temp.path())).unwrap();

    let repository = repository_record();
    first.put_repository(&repository).unwrap();

    // Both connections see the same data and the busy timeout is configured, not left at zero.
    assert!(second.get_repository(repository.id()).unwrap().is_some());
    assert!(second.busy_timeout_ms() >= 1000);
    second
        .put_session(&session_record(repository.id().to_owned(), 9))
        .unwrap();
    assert_eq!(first.list_sessions(repository.id()).unwrap().len(), 1);
}

#[test]
fn a_fixed_clock_makes_write_timestamps_deterministic() {
    let temp = tempfile::tempdir().unwrap();
    let mut store = Store::open(&store_path(temp.path()))
        .unwrap()
        .with_clock(FixedClock::at(1_700_000_000_000));
    store.migrate().unwrap();

    let repository = repository_record();
    store.put_repository(&repository).unwrap();
    assert_eq!(
        store.written_at(repository.id()).unwrap(),
        Some(1_700_000_000_000)
    );
}

#[test]
fn trajectories_reference_sessions_and_are_stored_whole() {
    let temp = tempfile::tempdir().unwrap();
    let mut store = open(temp.path());
    let repository = repository_record();
    store.put_repository(&repository).unwrap();
    let session = session_record(repository.id().to_owned(), 1);
    store.put_session(&session).unwrap();

    let mut body = Trajectory::sample();
    body.session_id = session.id().to_owned();
    body.repository_id = repository.id().to_owned();
    let trajectory = Envelope::new(
        TrajectoryId::from_body("01J0000000000000000000000A").unwrap(),
        Provenance::recorded_by("agent-jit/0.1.0"),
        body,
    );

    store.put_trajectory(&trajectory).unwrap();
    assert_eq!(
        store.get_trajectory(trajectory.id()).unwrap().unwrap(),
        trajectory
    );
    assert_eq!(store.count_trajectories(repository.id()).unwrap(), 1);
}

#[test]
fn opening_a_file_that_is_not_a_database_fails_closed() {
    let temp = tempfile::tempdir().unwrap();
    let path = temp.path().join("state.sqlite3");
    std::fs::write(&path, b"this is not a database").unwrap();
    std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o600)).unwrap();

    let error = Store::open(&StorePath::new(&path).unwrap()).unwrap_err();
    assert!(
        matches!(
            error,
            StoreError::Sqlite { .. } | StoreError::NotADatabase { .. }
        ),
        "{error:?}"
    );
}

#[test]
fn trace_metrics_round_trip_with_their_provenance_intact() {
    use agent_jit_domain::metrics::{
        Measured, MetricProvenance, RecorderHealth, TraceMetrics, Unknown,
    };

    let temp = tempfile::tempdir().unwrap();
    let mut store = open(temp.path());

    let repository = repository_record();
    store.put_repository(&repository).unwrap();
    let session = session_record(repository.id().to_owned(), 1);
    store.put_session(&session).unwrap();

    let mut body = Trajectory::sample();
    body.session_id = session.id().to_owned();
    body.repository_id = repository.id().to_owned();
    let trajectory = Envelope::new(
        TrajectoryId::from_body("01J0000000000000000000000A").unwrap(),
        Provenance::recorded_by("agent-jit/0.1.0"),
        body,
    );
    store.put_trajectory(&trajectory).unwrap();

    let metrics = TraceMetrics {
        active_duration_ms: Measured::exact(3_000_i64, "stored_events"),
        wall_duration_ms: Measured::exact(3_603_100_i64, "stored_events"),
        turns: 2,
        agent_tool_calls: 2,
        failed_tool_calls: 0,
        observed_bytes: 2_400,
        estimated_input_tokens: Measured::estimated(600_u64, "utf8_bytes_div_4"),
        // No versioned usage source: this must survive as unknown, not as zero.
        exact_input_tokens: Measured::unknown(Unknown::NoVersionedSource),
        health: RecorderHealth {
            truncated_payloads: 0,
            quarantined_segments: 1,
            duplicates_collapsed: 2,
        },
        provenance: MetricProvenance {
            computed_from: "stored_events".to_owned(),
            repository_id: repository.id().to_owned(),
            session_id: session.id().to_owned(),
            head_commit: "0".repeat(40),
            event_count: 9,
            adapter_schema: Some("agent_jit.claude_hooks.v1".to_owned()),
            runtime_version: Some("2.1.223".to_owned()),
            model_id: None,
        },
    };

    store.put_trace_metrics(trajectory.id(), &metrics).unwrap();
    let loaded = store.get_trace_metrics(trajectory.id()).unwrap().unwrap();

    assert_eq!(loaded, metrics);
    assert_eq!(loaded.exact_input_tokens.value(), None);
    assert_eq!(
        loaded.exact_input_tokens.source(),
        "unknown:no_versioned_source"
    );
    assert_eq!(loaded.provenance.computed_from, "stored_events");
    assert!(loaded.health.is_lossy());
}

#[test]
fn metrics_for_an_unknown_trajectory_are_refused_by_sql() {
    use agent_jit_domain::metrics::{
        Measured, MetricProvenance, RecorderHealth, TraceMetrics, Unknown,
    };

    let temp = tempfile::tempdir().unwrap();
    let mut store = open(temp.path());

    let orphan = TrajectoryId::from_body("01J000000000000000000000ZZ").unwrap();
    let metrics = TraceMetrics {
        active_duration_ms: Measured::unknown(Unknown::NoStopObserved),
        wall_duration_ms: Measured::unknown(Unknown::NoStopObserved),
        turns: 0,
        agent_tool_calls: 0,
        failed_tool_calls: 0,
        observed_bytes: 0,
        estimated_input_tokens: Measured::unknown(Unknown::NoEventsObserved),
        exact_input_tokens: Measured::unknown(Unknown::NoVersionedSource),
        health: RecorderHealth {
            truncated_payloads: 0,
            quarantined_segments: 0,
            duplicates_collapsed: 0,
        },
        provenance: MetricProvenance {
            computed_from: "stored_events".to_owned(),
            repository_id: repository_record().id().to_owned(),
            session_id: SessionId::from_body("01J0000000000000000000000S").unwrap(),
            head_commit: "0".repeat(40),
            event_count: 0,
            adapter_schema: None,
            runtime_version: None,
            model_id: None,
        },
    };

    let error = store.put_trace_metrics(&orphan, &metrics).unwrap_err();
    assert_eq!(error.code(), "store_constraint_violated");
}
