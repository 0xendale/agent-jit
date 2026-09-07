//! Deterministic, atomic corpus export command.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use agent_jit_domain::canonical::{Digest, canonical_string, digest_of, digest_projection};
use agent_jit_domain::export::validate_manifest;
use agent_jit_engine::process::ProcessRunner;
use agent_jit_engine::repository::discover;
use agent_jit_store::ExportSnapshot;
use serde_json::{Value, json};

use crate::corpus_export_io::{
    StageDir, create_private_dir_all, prepare_destination, write_private,
};
use crate::error::{CommandError, ExitClass};
use crate::output::Rendered;

const USAGE: &str = "usage: agent-jit corpus export --repo <path> --output <path> --mode <full-redacted|metadata-only> [--json]";

#[derive(Clone, Copy, PartialEq, Eq)]
enum Mode {
    Full,
    Metadata,
}

pub(crate) fn run(args: &[String]) -> Result<Rendered, CommandError> {
    let (repo, output, mode, as_json) = parse(args)?;
    let identity = discover(&ProcessRunner::new(), Path::new(&repo)).map_err(|error| {
        CommandError::new(error.code(), error.to_string(), ExitClass::SafetyRefusal)
    })?;
    let output = prepare_destination(&output, &identity.worktree_root)?;
    let mut store = crate::trace::open_store()?;
    let mut snapshot = store
        .export_snapshot(&identity.repo_id)
        .map_err(|error| crate::trace::refusal(&error))?;
    let redactor =
        crate::persistence_redaction::redactor(&identity.worktree_root.to_string_lossy());
    crate::corpus_export_redaction::redact_snapshot(&mut snapshot, &redactor)?;
    let stage = StageDir::create(&output)?;
    let (manifest, record_files) = materialize(stage.path(), &snapshot, mode)?;
    let files: Vec<String> = record_files
        .iter()
        .filter_map(|path| path.strip_prefix(stage.path()).ok())
        .map(|relative| output.join(relative).display().to_string())
        .collect();
    let report = json!({
        "manifest_digest": manifest.to_string(),
        "manifest_file": output.join("manifest.json").display().to_string(),
        "record_files": files,
    });
    let rendered = if as_json {
        Rendered::Json(report)
    } else {
        Rendered::Text(format!("{}\n", output.join("manifest.json").display()))
    };
    stage.publish(&output)?;
    Ok(rendered)
}

fn materialize(
    stage: &Path,
    snapshot: &ExportSnapshot,
    mode: Mode,
) -> Result<(Digest, Vec<PathBuf>), CommandError> {
    let mut records = Vec::new();
    let mut files = Vec::new();
    let mut versions = BTreeMap::new();
    let mut counts: BTreeMap<String, u64> = BTreeMap::new();
    let mut digest_groups: BTreeMap<String, Vec<String>> = BTreeMap::new();
    for record in &snapshot.records {
        versions.insert(record.schema.clone(), record.version);
        *counts.entry(record.schema.clone()).or_default() += 1;
        digest_groups
            .entry(record.schema.clone())
            .or_default()
            .push(record.digest.to_string());
        let relative = format!(
            "records/{}/{}.json",
            record.schema.replace('.', "_"),
            record.id
        );
        let mut item = json!({
            "schema": record.schema,
            "version": record.version,
            "id": record.id,
            "digest": record.digest.to_string(),
        });
        if let Some(metrics) = &record.metrics {
            item["metrics"] = metrics.clone();
        }
        if mode == Mode::Full {
            let path = stage.join(&relative);
            let parent = path
                .parent()
                .ok_or_else(|| internal("export path has no parent"))?;
            if !parent.exists() {
                create_private_dir_all(parent)?;
            }
            write_private(
                &path,
                &canonical_string(&record.document).map_err(canonical)?,
            )?;
            item["path"] = Value::String(relative);
            files.push(path);
        }
        records.push(item);
    }
    let content_digests: BTreeMap<String, String> = digest_groups
        .into_iter()
        .map(|(schema, digests)| {
            let values = Value::Array(digests.into_iter().map(Value::String).collect());
            digest_of(&values).map(|digest| (schema, digest.to_string()))
        })
        .collect::<Result<_, _>>()
        .map_err(canonical)?;
    let current: BTreeMap<String, String> = snapshot
        .current_annotations
        .iter()
        .map(|(trajectory, annotation)| (trajectory.to_string(), annotation.to_string()))
        .collect();
    let mut manifest = json!({
        "format": "agent_jit.corpus_export",
        "version": 1,
        "repository_id": snapshot.repository_id.to_string(),
        "mode": if mode == Mode::Full { "full_redacted" } else { "metadata_only" },
        "generated_at_unix_ms": 0,
        "clock": "fixed",
        "schema_versions": versions,
        "counts": counts,
        "content_digests": content_digests,
        "records": records,
        "current_annotations": current,
        "manifest_digest": "0000000000000000000000000000000000000000000000000000000000000000",
    });
    let digest = digest_projection(&manifest, &["/manifest_digest"]).map_err(canonical)?;
    manifest["manifest_digest"] = Value::String(digest.to_string());
    validate_manifest(&manifest).map_err(|error| internal(error.to_string()))?;
    write_private(
        &stage.join("manifest.json"),
        &canonical_string(&manifest).map_err(canonical)?,
    )?;
    Ok((digest, files))
}

fn parse(args: &[String]) -> Result<(String, PathBuf, Mode, bool), CommandError> {
    let mut repo = None;
    let mut output = None;
    let mut mode = None;
    let mut as_json = false;
    let mut remaining = args.iter();
    while let Some(argument) = remaining.next() {
        match argument.as_str() {
            "--repo" => repo = remaining.next().cloned(),
            "--output" => output = remaining.next().map(PathBuf::from),
            "--mode" => {
                mode = remaining
                    .next()
                    .map(String::as_str)
                    .map(parse_mode)
                    .transpose()?;
            }
            "--json" => as_json = true,
            _ => return Err(CommandError::usage(USAGE)),
        }
    }
    Ok((
        repo.ok_or_else(|| CommandError::usage(USAGE))?,
        output.ok_or_else(|| CommandError::usage(USAGE))?,
        mode.ok_or_else(|| CommandError::usage(USAGE))?,
        as_json,
    ))
}

fn parse_mode(value: &str) -> Result<Mode, CommandError> {
    match value {
        "full-redacted" => Ok(Mode::Full),
        "metadata-only" => Ok(Mode::Metadata),
        _ => Err(CommandError::usage(USAGE)),
    }
}

fn canonical(error: impl std::fmt::Display) -> CommandError {
    internal(error.to_string())
}
fn internal(message: impl Into<String>) -> CommandError {
    CommandError::new("export_failed", message, ExitClass::Internal)
}
