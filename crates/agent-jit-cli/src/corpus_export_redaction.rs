//! Mandatory defense-in-depth redaction for typed export snapshots.

use agent_jit_domain::canonical::to_value;
use agent_jit_domain::metrics::TraceMetrics;
use agent_jit_domain::redaction::Redactor;
use agent_jit_domain::schema::validate_document;
use agent_jit_store::ExportSnapshot;
use serde_json::Value;

use crate::error::{CommandError, ExitClass};

/// Redacts every string and revalidates each transformed typed payload.
pub(crate) fn redact_snapshot(
    snapshot: &mut ExportSnapshot,
    redactor: &Redactor,
) -> Result<(), CommandError> {
    for record in &mut snapshot.records {
        rewrite_strings(&mut record.document, redactor);
        let validated = validate_document(&record.document)
            .map_err(|error| invalid(format!("exported record is invalid: {error}")))?;
        record.schema = validated.schema_name.to_owned();
        record.version = validated.version;
        record.id = validated.id;
        record.digest = validated.digest;
        if let Some(metrics) = &mut record.metrics {
            rewrite_strings(metrics, redactor);
            let typed: TraceMetrics = serde_json::from_value(metrics.clone())
                .map_err(|error| invalid(format!("exported metrics are invalid: {error}")))?;
            *metrics = to_value(&typed)
                .map_err(|error| invalid(format!("exported metrics are invalid: {error}")))?;
        }
    }
    Ok(())
}

fn rewrite_strings(value: &mut Value, redactor: &Redactor) {
    match value {
        Value::String(text) => {
            let redacted = redactor.field(text);
            text.clone_from(redacted.value());
        }
        Value::Array(values) => {
            for value in values {
                rewrite_strings(value, redactor);
            }
        }
        Value::Object(values) => {
            for value in values.values_mut() {
                rewrite_strings(value, redactor);
            }
        }
        Value::Null | Value::Bool(_) | Value::Number(_) => {}
    }
}

fn invalid(message: impl Into<String>) -> CommandError {
    CommandError::new("export_failed", message, ExitClass::Internal)
}
