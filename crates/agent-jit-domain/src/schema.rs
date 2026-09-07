//! Schema generation and document validation.
//!
//! The JSON Schemas under `schemas/v1/` are generated from the Rust contracts and checked in, so a
//! contract change that is not reflected in the published schema fails the test suite instead of
//! surfacing later as a record that one side accepts and the other rejects.

use serde_json::Value;

use crate::canonical::Digest;
use crate::envelope::{Envelope, Record};
use crate::{benchmark, candidate, capability, outcome, trace};

const MAX_UTF8_BYTES_KEY: &str = "x-agent-jit-max-utf8-bytes";

/// Directory, relative to the workspace root, holding the checked-in schemas.
pub const SCHEMA_DIR: &str = "schemas/v1";

/// A generated JSON Schema together with the file it belongs in.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GeneratedSchema {
    /// Schema name, e.g. `agent_jit.trajectory`.
    pub schema_name: &'static str,
    /// Exact schema version.
    pub version: u32,
    /// File name inside [`SCHEMA_DIR`].
    pub file_name: String,
    /// Pretty-printed schema document, newline-terminated.
    pub contents: String,
}

/// A document that passed validation.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ValidatedRecord {
    /// Schema name declared by the document.
    pub schema_name: &'static str,
    /// Exact schema version.
    pub version: u32,
    /// Rendered record identifier.
    pub id: String,
    /// Digest of the record with volatile fields excluded.
    pub digest: Digest,
}

/// Why a document was refused.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum ValidationError {
    /// The document is not a JSON object with `schema` and integer `version`.
    #[error("document is missing a `schema` string and integer `version`")]
    NotAnEnvelope,
    /// No contract in this binary implements that schema name at that version.
    #[error("no contract implements `{schema}` v{version}")]
    Unsupported {
        /// Schema name found in the document.
        schema: String,
        /// Version found in the document.
        version: u32,
    },
    /// The schema matched but the record body did not.
    #[error("`{schema}` v{version} record is invalid: {reason}")]
    InvalidRecord {
        /// Schema name found in the document.
        schema: &'static str,
        /// Version found in the document.
        version: u32,
        /// Why the record was rejected.
        reason: String,
    },
}

impl ValidationError {
    /// Stable machine-readable code.
    #[must_use]
    pub const fn code(&self) -> &'static str {
        match self {
            Self::NotAnEnvelope => "schema_not_an_envelope",
            Self::Unsupported { .. } => "schema_unsupported",
            Self::InvalidRecord { .. } => "schema_invalid_record",
        }
    }
}

/// Applies `$macro` once per record contract, in schema-name order.
///
/// Every list of record kinds in this crate is generated from this one place, so a new contract
/// cannot be added to the product while being forgotten by schema generation or validation.
macro_rules! for_each_record {
    ($macro:ident) => {
        $macro! {
            benchmark::BenchmarkRecord,
            candidate::CandidateContract,
            capability::CapabilityVersion,
            trace::Event,
            candidate::Group,
            capability::Invocation,
            outcome::Outcome,
            outcome::OutcomeAnnotation,
            capability::ReplayResult,
            trace::Repository,
            trace::Session,
            trace::Trajectory,
            capability::Workflow,
        }
    };
}

/// Returns the generated schema for every record contract, ordered by schema name.
#[must_use]
pub fn generated_schemas() -> Vec<GeneratedSchema> {
    macro_rules! generate {
        ($($record:path),* $(,)?) => {
            vec![$(generate_one::<$record>()),*]
        };
    }

    let mut schemas: Vec<GeneratedSchema> = for_each_record!(generate);
    schemas.sort_by(|left, right| left.schema_name.cmp(right.schema_name));
    schemas
}

/// Generates the schema document for one record contract.
fn generate_one<T: Record>() -> GeneratedSchema {
    let schema = schemars::schema_for!(Envelope<T>);
    let mut contents = serde_json::to_string_pretty(&schema)
        .unwrap_or_else(|_| unreachable!("a generated schema is always serializable"));
    contents.push('\n');

    GeneratedSchema {
        schema_name: T::SCHEMA,
        version: T::VERSION,
        file_name: format!("{}.schema.json", T::SCHEMA.replace('.', "_")),
        contents,
    }
}

/// Validates one stored document against the contract it claims to implement.
///
/// # Errors
///
/// Returns [`ValidationError`] when the document is not an envelope, claims a schema or version
/// this binary does not implement, or fails the contract it claims.
pub fn validate_document(document: &Value) -> Result<ValidatedRecord, ValidationError> {
    let schema = document
        .get("schema")
        .and_then(Value::as_str)
        .ok_or(ValidationError::NotAnEnvelope)?;
    let version = document
        .get("version")
        .and_then(Value::as_u64)
        .and_then(|version| u32::try_from(version).ok())
        .ok_or(ValidationError::NotAnEnvelope)?;

    macro_rules! dispatch {
        ($($record:path),* $(,)?) => {
            $(
                if schema == <$record as Record>::SCHEMA {
                    return validate_as::<$record>(document, version);
                }
            )*
        };
    }

    for_each_record!(dispatch);

    Err(ValidationError::Unsupported {
        schema: schema.to_owned(),
        version,
    })
}

/// Validates `document` as record `T`, which already matched by schema name.
fn validate_as<T: Record>(
    document: &Value,
    version: u32,
) -> Result<ValidatedRecord, ValidationError> {
    if version != T::VERSION {
        return Err(ValidationError::Unsupported {
            schema: T::SCHEMA.to_owned(),
            version,
        });
    }

    let generated = serde_json::to_value(schemars::schema_for!(Envelope<T>))
        .unwrap_or_else(|_| unreachable!("a generated schema is always serializable"));
    validate_schema_extensions(&generated, document).map_err(|reason| {
        ValidationError::InvalidRecord {
            schema: T::SCHEMA,
            version,
            reason,
        }
    })?;

    let envelope: Envelope<T> = serde_json::from_value(document.clone()).map_err(|error| {
        ValidationError::InvalidRecord {
            schema: T::SCHEMA,
            version,
            reason: error.to_string(),
        }
    })?;

    let digest = envelope
        .digest()
        .map_err(|error| ValidationError::InvalidRecord {
            schema: T::SCHEMA,
            version,
            reason: error.to_string(),
        })?;

    Ok(ValidatedRecord {
        schema_name: T::SCHEMA,
        version: T::VERSION,
        id: envelope.id().to_string(),
        digest,
    })
}

fn validate_schema_extensions(schema: &Value, document: &Value) -> Result<(), String> {
    validate_extensions_at(schema, document, schema, "")
}

fn validate_extensions_at(
    schema: &Value,
    document: &Value,
    root: &Value,
    path: &str,
) -> Result<(), String> {
    if let Some(reference) = schema.get("$ref").and_then(Value::as_str) {
        let pointer = reference
            .strip_prefix('#')
            .ok_or_else(|| format!("unsupported non-local schema reference `{reference}`"))?;
        let target = root
            .pointer(pointer)
            .ok_or_else(|| format!("unresolved schema reference `{reference}`"))?;
        validate_extensions_at(target, document, root, path)?;
    }

    if let Some(limit) = schema.get(MAX_UTF8_BYTES_KEY) {
        let limit = limit
            .as_u64()
            .and_then(|value| usize::try_from(value).ok())
            .ok_or_else(|| format!("invalid `{MAX_UTF8_BYTES_KEY}` schema value"))?;
        if let Some(value) = document.as_str()
            && value.len() > limit
        {
            return Err(format!("string at `{path}` exceeds {limit} UTF-8 bytes"));
        }
    }

    if let (Some(properties), Some(object)) = (schema.get("properties"), document.as_object()) {
        let properties = properties
            .as_object()
            .ok_or_else(|| "schema `properties` must be an object".to_owned())?;
        for (name, property_schema) in properties {
            if let Some(value) = object.get(name) {
                validate_extensions_at(property_schema, value, root, &format!("{path}/{name}"))?;
            }
        }
    }

    if let (Some(items), Some(array)) = (schema.get("items"), document.as_array()) {
        for (index, value) in array.iter().enumerate() {
            validate_extensions_at(items, value, root, &format!("{path}/{index}"))?;
        }
    }

    Ok(())
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::validate_schema_extensions;

    #[test]
    fn utf8_byte_extension_is_enforced_independently_of_typed_deserialization() {
        let schema = json!({
            "$defs": {
                "bounded": {
                    "type": "string",
                    "x-agent-jit-max-utf8-bytes": 4
                }
            },
            "type": "object",
            "properties": {
                "values": {
                    "type": "array",
                    "items": {"$ref": "#/$defs/bounded"}
                }
            }
        });

        assert!(validate_schema_extensions(&schema, &json!({"values": ["éé"]})).is_ok());
        assert!(validate_schema_extensions(&schema, &json!({"values": ["ééé"]})).is_err());
    }
}
