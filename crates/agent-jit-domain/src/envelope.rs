//! Versioned record envelopes.
//!
//! Nothing is persisted as a bare record. Every record travels inside an [`Envelope`] carrying its
//! schema name, an exact integer version, its branded identifier, and provenance. Reading a record
//! whose schema name or version is not the one this binary implements is an error, never a
//! best-effort interpretation: a silently reinterpreted record would corrupt every digest computed
//! from it.

use std::fmt;
use std::marker::PhantomData;

use schemars::JsonSchema;
use serde::de::DeserializeOwned;
use serde::{Deserialize, Serialize};

use crate::canonical::{CanonicalError, Digest, digest_projection, to_value};
use crate::ids::{Id, IdKind};

/// Pointer to the wall-clock field that every envelope carries and no digest may include.
const RECORDED_AT_POINTER: &str = "/provenance/recorded_at_unix_ms";

/// A schema name such as `agent_jit.trajectory`.
#[derive(
    Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize, JsonSchema,
)]
#[serde(transparent)]
pub struct SchemaName(&'static str);

impl SchemaName {
    /// Wraps a static schema name.
    #[must_use]
    pub const fn new(name: &'static str) -> Self {
        Self(name)
    }

    /// Returns the name as a string slice.
    #[must_use]
    pub const fn as_str(&self) -> &'static str {
        self.0
    }
}

impl fmt::Display for SchemaName {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.0)
    }
}

/// An exact schema version. Versions are integers; there is no range or compatibility window.
#[derive(
    Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize, JsonSchema,
)]
#[serde(transparent)]
pub struct SchemaVersion(u32);

impl SchemaVersion {
    /// Wraps a version number.
    #[must_use]
    pub const fn new(version: u32) -> Self {
        Self(version)
    }

    /// Returns the version number.
    #[must_use]
    pub const fn get(self) -> u32 {
        self.0
    }
}

impl fmt::Display for SchemaVersion {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", self.0)
    }
}

/// A record body that can travel inside an [`Envelope`].
pub trait Record: Serialize + DeserializeOwned + Clone + PartialEq + JsonSchema {
    /// Identifier kind that addresses this record.
    type Kind: IdKind;
    /// Schema name, for example `agent_jit.trajectory`.
    const SCHEMA: &'static str;
    /// Exact schema version implemented by this binary.
    const VERSION: u32;
    /// Body-relative JSON pointers excluded from the digest projection.
    ///
    /// Each pointer must resolve in a serialized body; a stale pointer is a defect that the
    /// contract tests catch, because it would silently pull a volatile field into a digest.
    const VOLATILE: &'static [&'static str] = &[];
}

/// Why a record could not be read.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum EnvelopeError {
    /// The schema name or version is not the one this binary implements.
    #[error(
        "schema_unsupported: expected `{expected_schema}` v{expected_version}, found `{found_schema}` v{found_version}"
    )]
    SchemaUnsupported {
        /// Schema name this binary implements.
        expected_schema: &'static str,
        /// Version this binary implements.
        expected_version: u32,
        /// Schema name found in the record.
        found_schema: String,
        /// Version found in the record.
        found_version: u32,
    },
    /// Canonicalization of the record failed.
    #[error("{}: {source}", source.code())]
    Canonical {
        /// The underlying canonicalization failure.
        #[from]
        source: CanonicalError,
    },
}

impl EnvelopeError {
    /// Stable machine-readable code.
    #[must_use]
    pub const fn code(&self) -> &'static str {
        match self {
            Self::SchemaUnsupported { .. } => "schema_unsupported",
            Self::Canonical { source } => source.code(),
        }
    }
}

/// Provenance recorded alongside every envelope.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct Provenance {
    /// Name and version of the component that produced the record, e.g. `agent-jit/0.1.0`.
    pub produced_by: String,
    /// How the record entered the system.
    pub source: ProvenanceSource,
    /// Wall-clock time the record was produced. Volatile: never part of a digest.
    pub recorded_at_unix_ms: i64,
    /// Digests of the records this one was derived from, in a stable order.
    #[serde(default)]
    pub parents: Vec<Digest>,
}

impl Provenance {
    /// Provenance for a record captured live by the recorder.
    #[must_use]
    pub fn recorded_by(produced_by: &str) -> Self {
        Self {
            produced_by: produced_by.to_owned(),
            source: ProvenanceSource::Recorded,
            recorded_at_unix_ms: 0,
            parents: Vec::new(),
        }
    }

    /// Provenance for a record derived from other records.
    #[must_use]
    pub fn derived_by(produced_by: &str, parents: Vec<Digest>) -> Self {
        Self {
            produced_by: produced_by.to_owned(),
            source: ProvenanceSource::Derived,
            recorded_at_unix_ms: 0,
            parents,
        }
    }
}

/// How a record entered the system.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum ProvenanceSource {
    /// Captured live from agent hooks.
    Recorded,
    /// Imported from an exact-shape historical record.
    Imported,
    /// Derived from other records already in the store.
    Derived,
    /// Confirmed by a human operator.
    Confirmed,
}

/// A record together with its schema identity and provenance.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Envelope<T: Record> {
    schema: &'static str,
    version: u32,
    id: Id<T::Kind>,
    provenance: Provenance,
    body: T,
    #[serde(skip)]
    marker: PhantomData<T>,
}

impl<T: Record> Envelope<T> {
    /// Wraps `body` with its schema identity, identifier, and provenance.
    #[must_use]
    pub fn new(id: Id<T::Kind>, provenance: Provenance, body: T) -> Self {
        Self {
            schema: T::SCHEMA,
            version: T::VERSION,
            id,
            provenance,
            body,
            marker: PhantomData,
        }
    }

    /// Returns the schema name.
    #[must_use]
    pub const fn schema(&self) -> SchemaName {
        SchemaName::new(T::SCHEMA)
    }

    /// Returns the exact schema version.
    #[must_use]
    pub const fn version(&self) -> SchemaVersion {
        SchemaVersion::new(T::VERSION)
    }

    /// Returns the record identifier.
    #[must_use]
    pub const fn id(&self) -> &Id<T::Kind> {
        &self.id
    }

    /// Returns the provenance.
    #[must_use]
    pub const fn provenance(&self) -> &Provenance {
        &self.provenance
    }

    /// Returns the provenance for mutation.
    pub const fn provenance_mut(&mut self) -> &mut Provenance {
        &mut self.provenance
    }

    /// Returns the record body.
    #[must_use]
    pub const fn body(&self) -> &T {
        &self.body
    }

    /// Returns the record body for mutation.
    pub const fn body_mut(&mut self) -> &mut T {
        &mut self.body
    }

    /// Consumes the envelope and returns the body.
    #[must_use]
    pub fn into_body(self) -> T {
        self.body
    }

    /// Returns every envelope-relative JSON pointer excluded from the digest projection.
    #[must_use]
    pub fn volatile_pointers() -> Vec<String> {
        let mut pointers = vec![RECORDED_AT_POINTER.to_owned()];
        pointers.extend(T::VOLATILE.iter().map(|pointer| format!("/body{pointer}")));
        pointers
    }

    /// Digests the envelope with volatile fields excluded.
    ///
    /// # Errors
    ///
    /// Returns [`EnvelopeError`] when the envelope cannot be canonicalized, or when a declared
    /// volatile pointer does not resolve.
    pub fn digest(&self) -> Result<Digest, EnvelopeError> {
        let value = to_value(self)?;
        let pointers = Self::volatile_pointers();
        let borrowed: Vec<&str> = pointers.iter().map(String::as_str).collect();
        Ok(digest_projection(&value, &borrowed)?)
    }
}

/// Wire shape used to validate schema identity before a body is trusted.
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct RawEnvelope<T> {
    schema: String,
    version: u32,
    id: String,
    provenance: Provenance,
    body: T,
}

impl<'de, T: Record> Deserialize<'de> for Envelope<T> {
    fn deserialize<D: serde::Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let raw = RawEnvelope::<T>::deserialize(deserializer)?;

        if raw.schema != T::SCHEMA || raw.version != T::VERSION {
            return Err(serde::de::Error::custom(EnvelopeError::SchemaUnsupported {
                expected_schema: T::SCHEMA,
                expected_version: T::VERSION,
                found_schema: raw.schema,
                found_version: raw.version,
            }));
        }

        let id: Id<T::Kind> = raw.id.parse().map_err(|error: crate::ids::IdError| {
            serde::de::Error::custom(format!("{}: {error}", error.code()))
        })?;

        Ok(Self::new(id, raw.provenance, raw.body))
    }
}

impl<T: Record> JsonSchema for Envelope<T> {
    fn schema_name() -> std::borrow::Cow<'static, str> {
        format!("Envelope_{}", T::schema_name()).into()
    }

    fn json_schema(generator: &mut schemars::SchemaGenerator) -> schemars::Schema {
        let body = generator.subschema_for::<T>();
        let id = generator.subschema_for::<Id<T::Kind>>();
        let provenance = generator.subschema_for::<Provenance>();
        schemars::json_schema!({
            "type": "object",
            "additionalProperties": false,
            "required": ["schema", "version", "id", "provenance", "body"],
            "properties": {
                "schema": {"type": "string", "const": T::SCHEMA},
                "version": {"type": "integer", "const": T::VERSION},
                "id": id,
                "provenance": provenance,
                "body": body,
            },
        })
    }
}
