#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
//! Canonical full-redacted and metadata-only export verification.

mod corpus_support;

use std::path::{Path, PathBuf};

use agent_jit_domain::canonical::{canonical_string, digest_projection};
use agent_jit_domain::export::validate_manifest;
use agent_jit_domain::schema::validate_document;
use corpus_support::{CANARY, bin, init_repo, json, record};
use serde_json::Value;

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
            "--evidence",
            "artifacts/result.json",
            "--json",
        ])
        .assert()
        .success();
}

fn export(home: &Path, repo: &Path, output: &Path, mode: &str) -> Value {
    let result = bin(home)
        .args([
            "corpus",
            "export",
            "--repo",
            repo.to_str().unwrap(),
            "--output",
            output.to_str().unwrap(),
            "--mode",
            mode,
            "--json",
        ])
        .assert()
        .success();
    json(&result.get_output().stdout)
}

fn read_canonical(path: &Path) -> Value {
    let text = std::fs::read_to_string(path).unwrap();
    let value: Value = serde_json::from_str(&text).unwrap();
    assert_eq!(canonical_string(&value).unwrap(), text);
    value
}

fn verify_manifest(report: &Value) -> Value {
    let path = PathBuf::from(report["manifest_file"].as_str().unwrap());
    let manifest = read_canonical(&path);
    let validated = validate_manifest(&manifest).unwrap();
    let recomputed = digest_projection(&manifest, &["/manifest_digest"]).unwrap();
    assert_eq!(validated.digest, recomputed);
    assert_eq!(report["manifest_digest"], recomputed.to_string());
    manifest
}

fn verify_full_records(root: &Path, manifest: &Value) {
    for record in manifest["records"].as_array().unwrap() {
        let relative = record["path"].as_str().unwrap();
        let document = read_canonical(&root.join(relative));
        let validated = validate_document(&document).unwrap();
        assert_eq!(validated.schema_name, record["schema"]);
        assert_eq!(validated.id, record["id"]);
        assert_eq!(validated.digest.to_string(), record["digest"]);
    }
}

fn scan_tree(root: &Path, forbidden: &[&str]) {
    let mut pending = vec![root.to_path_buf()];
    while let Some(path) = pending.pop() {
        for entry in std::fs::read_dir(path).unwrap() {
            let entry = entry.unwrap();
            let path = entry.path();
            let name = entry.file_name().to_string_lossy().into_owned();
            for value in forbidden {
                assert!(!name.contains(value), "unsafe emitted name {name:?}");
            }
            if path.is_dir() {
                pending.push(path);
            } else {
                let content = std::fs::read_to_string(path).unwrap();
                for value in forbidden {
                    assert!(!content.contains(value), "unsafe exported content");
                }
            }
        }
    }
}

#[test]
fn given_annotated_trace_when_full_export_repeats_then_all_digests_validate_and_current_is_explicit()
 {
    // Given
    let home = tempfile::tempdir().unwrap();
    let repo = tempfile::tempdir().unwrap();
    let first_dir = tempfile::tempdir().unwrap();
    let second_dir = tempfile::tempdir().unwrap();
    init_repo(repo.path());
    let (trajectory, _) = record(home.path(), repo.path());
    annotate(home.path(), &trajectory.to_string());
    let first = export(home.path(), repo.path(), first_dir.path(), "full-redacted");

    // When
    let second = export(home.path(), repo.path(), second_dir.path(), "full-redacted");

    // Then
    assert_eq!(first["manifest_digest"], second["manifest_digest"]);
    let first_manifest = verify_manifest(&first);
    let second_manifest = verify_manifest(&second);
    assert_eq!(first_manifest, second_manifest);
    verify_full_records(first_dir.path(), &first_manifest);
    verify_full_records(second_dir.path(), &second_manifest);
    let annotations: Vec<&Value> = second_manifest["records"]
        .as_array()
        .unwrap()
        .iter()
        .filter(|record| record["schema"] == "agent_jit.outcome_annotation")
        .collect();
    assert_eq!(annotations.len(), 1);
    assert_eq!(
        second_manifest["current_annotations"][trajectory.to_string()],
        annotations[0]["id"]
    );
    scan_tree(
        first_dir.path(),
        &[
            CANARY,
            repo.path().to_str().unwrap(),
            home.path().to_str().unwrap(),
        ],
    );
    scan_tree(
        second_dir.path(),
        &[
            CANARY,
            repo.path().to_str().unwrap(),
            home.path().to_str().unwrap(),
        ],
    );
}

#[test]
fn given_annotated_trace_when_metadata_export_runs_then_bodies_are_omitted_but_metrics_and_digests_remain()
 {
    // Given
    let home = tempfile::tempdir().unwrap();
    let repo = tempfile::tempdir().unwrap();
    let full_output = tempfile::tempdir().unwrap();
    let output = tempfile::tempdir().unwrap();
    init_repo(repo.path());
    let (trajectory, _) = record(home.path(), repo.path());
    annotate(home.path(), &trajectory.to_string());
    let full_report = export(
        home.path(),
        repo.path(),
        full_output.path(),
        "full-redacted",
    );
    let full_manifest = verify_manifest(&full_report);
    verify_full_records(full_output.path(), &full_manifest);

    // When
    let report = export(home.path(), repo.path(), output.path(), "metadata-only");

    // Then
    let manifest = verify_manifest(&report);
    assert_eq!(manifest["mode"], "metadata_only");
    assert!(report["record_files"].as_array().unwrap().is_empty());
    for record in manifest["records"].as_array().unwrap() {
        assert!(record.get("body").is_none());
        assert!(record["digest"].is_string());
        let full = full_manifest["records"]
            .as_array()
            .unwrap()
            .iter()
            .find(|full| full["id"] == record["id"])
            .unwrap();
        assert_eq!(record["digest"], full["digest"]);
    }
    let trajectory = manifest["records"]
        .as_array()
        .unwrap()
        .iter()
        .find(|record| record["schema"] == "agent_jit.trajectory")
        .unwrap();
    assert!(trajectory["metrics"]["agent_tool_calls"].is_number());
    scan_tree(
        output.path(),
        &[
            CANARY,
            repo.path().to_str().unwrap(),
            home.path().to_str().unwrap(),
        ],
    );
}
