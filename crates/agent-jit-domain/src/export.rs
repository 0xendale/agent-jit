//! Deterministic corpus export manifest contract.

use std::collections::{BTreeMap, BTreeSet};

use serde::Deserialize;
use serde_json::Value;

use crate::canonical::{Digest, digest_of, digest_projection};
use crate::ids::{OutcomeAnnotationId, RepositoryId, TrajectoryId};

/// Export mode recorded in a manifest.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ExportMode {
    /// Canonical redacted records are emitted as separate files.
    FullRedacted,
    /// Only record identities, metrics, and digests are emitted.
    MetadataOnly,
}

/// Successful manifest validation result.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ValidatedManifest {
    /// Digest recomputed without the manifest's digest field.
    pub digest: Digest,
}

/// Why an export manifest was refused.
#[derive(Debug, thiserror::Error)]
pub enum ManifestError {
    /// Manifest shape or identifiers are invalid.
    #[error("export manifest is invalid: {0}")]
    Invalid(String),
    /// Declared digest does not match canonical content.
    #[error("export manifest digest does not match its content")]
    DigestMismatch,
    /// Canonicalization failed.
    #[error("export manifest cannot be canonicalized: {0}")]
    Canonical(#[from] crate::canonical::CanonicalError),
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Manifest {
    format: String,
    version: u32,
    repository_id: RepositoryId,
    mode: ExportMode,
    generated_at_unix_ms: i64,
    clock: String,
    schema_versions: BTreeMap<String, u32>,
    counts: BTreeMap<String, u64>,
    content_digests: BTreeMap<String, Digest>,
    records: Vec<ManifestRecord>,
    current_annotations: BTreeMap<TrajectoryId, OutcomeAnnotationId>,
    #[serde(rename = "manifest_digest")]
    digest: Digest,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct ManifestRecord {
    schema: String,
    version: u32,
    id: String,
    digest: Digest,
    #[serde(default)]
    path: Option<String>,
    #[serde(default)]
    metrics: Option<Value>,
}

/// Validates shape, deterministic ordering, mode rules, and digest of an export manifest.
///
/// # Errors
///
/// Returns [`ManifestError`] when any invariant fails.
pub fn validate_manifest(document: &Value) -> Result<ValidatedManifest, ManifestError> {
    let manifest: Manifest = serde_json::from_value(document.clone())
        .map_err(|error| ManifestError::Invalid(error.to_string()))?;
    if manifest.format != "agent_jit.corpus_export"
        || manifest.version != 1
        || manifest.generated_at_unix_ms != 0
        || manifest.clock != "fixed"
    {
        return Err(ManifestError::Invalid(
            "unsupported format, version, or clock semantics".to_owned(),
        ));
    }
    let mut previous: Option<(&str, &str)> = None;
    let mut seen = BTreeSet::new();
    let mut counts: BTreeMap<String, u64> = BTreeMap::new();
    let mut digest_groups: BTreeMap<String, Vec<Value>> = BTreeMap::new();
    let mut trajectories = BTreeSet::new();
    let mut annotations = BTreeSet::new();
    let repository_id = manifest.repository_id.to_string();
    let mut repository_seen = false;
    for record in &manifest.records {
        let key = (record.schema.as_str(), record.id.as_str());
        if previous.is_some_and(|value| value >= key) || !seen.insert(key) {
            return Err(ManifestError::Invalid(
                "records must be uniquely sorted by schema and id".to_owned(),
            ));
        }
        let path_valid = match manifest.mode {
            ExportMode::FullRedacted => record.path.as_deref().is_some_and(safe_path),
            ExportMode::MetadataOnly => record.path.is_none(),
        };
        if !path_valid || record.version == 0 {
            return Err(ManifestError::Invalid(
                "record path or version does not match export mode".to_owned(),
            ));
        }
        if manifest.schema_versions.get(&record.schema) != Some(&record.version)
            || record
                .metrics
                .as_ref()
                .is_some_and(|value| !value.is_object())
        {
            return Err(ManifestError::Invalid(
                "record metadata does not match manifest declarations".to_owned(),
            ));
        }
        *counts.entry(record.schema.clone()).or_default() += 1;
        digest_groups
            .entry(record.schema.clone())
            .or_default()
            .push(Value::String(record.digest.to_string()));
        if record.schema == "agent_jit.trajectory" {
            trajectories.insert(record.id.as_str());
        } else if record.schema == "agent_jit.outcome_annotation" {
            annotations.insert(record.id.as_str());
        } else if record.schema == "agent_jit.repository" && record.id == repository_id {
            repository_seen = true;
        }
        previous = Some(key);
    }
    let content_digests: BTreeMap<String, Digest> = digest_groups
        .into_iter()
        .map(|(schema, values)| digest_of(&Value::Array(values)).map(|digest| (schema, digest)))
        .collect::<Result<_, _>>()?;
    if !repository_seen || counts != manifest.counts || content_digests != manifest.content_digests
    {
        return Err(ManifestError::Invalid(
            "record counts or content digests do not match".to_owned(),
        ));
    }
    for (trajectory, annotation) in &manifest.current_annotations {
        if !trajectories.contains(trajectory.to_string().as_str())
            || !annotations.contains(annotation.to_string().as_str())
        {
            return Err(ManifestError::Invalid(
                "current annotation references an absent record".to_owned(),
            ));
        }
    }
    let digest = digest_projection(document, &["/manifest_digest"])?;
    if digest != manifest.digest {
        return Err(ManifestError::DigestMismatch);
    }
    Ok(ValidatedManifest { digest })
}

fn safe_path(path: &str) -> bool {
    !path.is_empty()
        && !path.starts_with('/')
        && path
            .split('/')
            .all(|part| !part.is_empty() && part != "." && part != "..")
}
