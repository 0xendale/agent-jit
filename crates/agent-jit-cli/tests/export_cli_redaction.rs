#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
//! Defense-in-depth export redaction runtime contract.

mod corpus_support;

use std::path::{Path, PathBuf};

use agent_jit_domain::canonical::{canonical_string, digest_of, digest_projection};
use agent_jit_domain::export::validate_manifest;
use agent_jit_domain::schema::validate_document;
use corpus_support::{CANARY, bin, init_repo, json, record};
use rusqlite::{Connection, params};
use serde_json::Value;

const TOKEN_MARKER: &str = "[redacted:token]";

fn annotate(home: &Path, trajectory: &str) {
    bin(home)
        .args([
            "trace",
            "annotate",
            trajectory,
            "--outcome",
            "succeeded",
            "--actor",
            "operator",
            "--rationale",
            "review passed",
            "--json",
        ])
        .assert()
        .success();
}

fn seed_stored_secrets(home: &Path, trajectory: &str) {
    let connection = Connection::open(home.join("data/state.sqlite3")).unwrap();
    let mut trajectory_json: Value = serde_json::from_str(
        &connection
            .query_row(
                "SELECT record_json FROM trajectories WHERE trajectory_id=?1",
                [trajectory],
                |row| row.get::<_, String>(0),
            )
            .unwrap(),
    )
    .unwrap();
    trajectory_json["body"]["intent"] = Value::String(format!("inspect {CANARY}"));
    let trajectory_digest = digest_projection(
        &trajectory_json,
        &[
            "/provenance/recorded_at_unix_ms",
            "/body/started_at_unix_ms",
            "/body/duration_ms",
        ],
    )
    .unwrap();
    connection
        .execute(
            "UPDATE trajectories SET intent=?1,record_json=?2,digest=?3 WHERE trajectory_id=?4",
            params![
                format!("inspect {CANARY}"),
                canonical_string(&trajectory_json).unwrap(),
                trajectory_digest.to_string(),
                trajectory,
            ],
        )
        .unwrap();

    let (annotation_id, mut annotation_json): (String, Value) = connection
        .query_row(
            "SELECT annotation_id,record_json FROM outcome_annotations WHERE trajectory_id=?1",
            [trajectory],
            |row| {
                let text: String = row.get(1)?;
                Ok((row.get(0)?, serde_json::from_str(&text).unwrap()))
            },
        )
        .unwrap();
    annotation_json["body"]["rationale"] = Value::String(format!("review {CANARY}"));
    let annotation_digest = digest_projection(
        &annotation_json,
        &[
            "/provenance/recorded_at_unix_ms",
            "/body/annotated_at_unix_ms",
        ],
    )
    .unwrap();
    connection
        .execute(
            "UPDATE outcome_annotations SET rationale=?1,record_json=?2,digest=?3 \
             WHERE annotation_id=?4",
            params![
                format!("review {CANARY}"),
                canonical_string(&annotation_json).unwrap(),
                annotation_digest.to_string(),
                annotation_id,
            ],
        )
        .unwrap();

    let mut metrics: Value = serde_json::from_str(
        &connection
            .query_row(
                "SELECT record_json FROM trace_metrics WHERE trajectory_id=?1",
                [trajectory],
                |row| row.get::<_, String>(0),
            )
            .unwrap(),
    )
    .unwrap();
    metrics["provenance"]["computed_from"] = Value::String(format!("events {CANARY}"));
    let metrics_digest = digest_of(&metrics).unwrap();
    connection
        .execute(
            "UPDATE trace_metrics SET record_json=?1,digest=?2 WHERE trajectory_id=?3",
            params![
                canonical_string(&metrics).unwrap(),
                metrics_digest.to_string(),
                trajectory,
            ],
        )
        .unwrap();
}

fn exported(home: &Path, repo: &Path, output: &Path) -> Value {
    let result = bin(home)
        .args([
            "corpus",
            "export",
            "--repo",
            repo.to_str().unwrap(),
            "--output",
            output.to_str().unwrap(),
            "--mode",
            "full-redacted",
            "--json",
        ])
        .assert()
        .success();
    json(&result.get_output().stdout)
}

#[test]
fn given_stored_secrets_when_exported_then_artifacts_are_redacted_and_source_is_unchanged() {
    // Given
    let home = tempfile::tempdir().unwrap();
    let repo = tempfile::tempdir().unwrap();
    let output = tempfile::tempdir().unwrap();
    init_repo(repo.path());
    let (trajectory, _) = record(home.path(), repo.path());
    annotate(home.path(), &trajectory.to_string());
    seed_stored_secrets(home.path(), &trajectory.to_string());

    // When
    let report = exported(home.path(), repo.path(), output.path());

    // Then
    let manifest_path = PathBuf::from(report["manifest_file"].as_str().unwrap());
    let manifest: Value =
        serde_json::from_str(&std::fs::read_to_string(manifest_path).unwrap()).unwrap();
    validate_manifest(&manifest).unwrap();
    let mut exported_text = canonical_string(&manifest).unwrap();
    for item in manifest["records"].as_array().unwrap() {
        let document: Value = serde_json::from_str(
            &std::fs::read_to_string(output.path().join(item["path"].as_str().unwrap())).unwrap(),
        )
        .unwrap();
        let validated = validate_document(&document).unwrap();
        assert_eq!(validated.digest.to_string(), item["digest"]);
        exported_text.push_str(&canonical_string(&document).unwrap());
    }
    assert!(!exported_text.contains(CANARY));
    assert!(exported_text.matches(TOKEN_MARKER).count() >= 3);
    let connection = Connection::open(home.path().join("data/state.sqlite3")).unwrap();
    for table in ["trajectories", "outcome_annotations", "trace_metrics"] {
        let query = format!("SELECT record_json FROM {table} WHERE record_json LIKE '%sk-ant-%'");
        let source: String = connection.query_row(&query, [], |row| row.get(0)).unwrap();
        assert!(source.contains(CANARY));
    }
}
