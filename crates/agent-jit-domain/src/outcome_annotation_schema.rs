//! JSON Schema details for manual outcome annotations.

use schemars::JsonSchema;

use crate::ids::TrajectoryId;
use crate::outcome_annotation::{
    AnnotationRevision, AnnotationStatus, EvidenceRef, MAX_ANNOTATION_ACTOR_BYTES,
    MAX_ANNOTATION_RATIONALE_BYTES, MAX_EVIDENCE_REFS, OutcomeAnnotation,
};

const EVIDENCE_REF_PATTERN: &str = concat!(
    r"^(?:[^./\\\x00][^/\\\x00]*|\.[^./\\\x00][^/\\\x00]*|\.\.[^/\\\x00][^/\\\x00]*)",
    r"(?:/(?:[^./\\\x00][^/\\\x00]*|\.[^./\\\x00][^/\\\x00]*|\.\.[^/\\\x00][^/\\\x00]*))*$",
);

impl JsonSchema for AnnotationRevision {
    fn schema_name() -> std::borrow::Cow<'static, str> {
        "AnnotationRevision".into()
    }

    fn json_schema(_generator: &mut schemars::SchemaGenerator) -> schemars::Schema {
        schemars::json_schema!({
            "type": "integer",
            "format": "uint32",
            "minimum": 1,
            "maximum": u32::MAX,
        })
    }
}

impl JsonSchema for EvidenceRef {
    fn schema_name() -> std::borrow::Cow<'static, str> {
        "EvidenceRef".into()
    }

    fn json_schema(_generator: &mut schemars::SchemaGenerator) -> schemars::Schema {
        schemars::json_schema!({
            "type": "string",
            "pattern": EVIDENCE_REF_PATTERN,
        })
    }
}

impl JsonSchema for OutcomeAnnotation {
    fn schema_name() -> std::borrow::Cow<'static, str> {
        "OutcomeAnnotation".into()
    }

    fn json_schema(generator: &mut schemars::SchemaGenerator) -> schemars::Schema {
        let trajectory_id = generator.subschema_for::<TrajectoryId>();
        let revision = generator.subschema_for::<AnnotationRevision>();
        let status = generator.subschema_for::<AnnotationStatus>();
        let evidence = generator.subschema_for::<EvidenceRef>();
        schemars::json_schema!({
            "type": "object",
            "additionalProperties": false,
            "required": [
                "trajectory_id", "revision", "status", "actor", "annotated_at_unix_ms",
                "rationale", "evidence",
            ],
            "properties": {
                "trajectory_id": trajectory_id,
                "revision": revision,
                "status": status,
                "actor": {
                    "type": "string",
                    "minLength": 1,
                    "maxLength": MAX_ANNOTATION_ACTOR_BYTES,
                    "x-agent-jit-max-utf8-bytes": MAX_ANNOTATION_ACTOR_BYTES,
                },
                "annotated_at_unix_ms": {"type": "integer", "format": "int64"},
                "rationale": {
                    "type": "string",
                    "minLength": 1,
                    "maxLength": MAX_ANNOTATION_RATIONALE_BYTES,
                    "x-agent-jit-max-utf8-bytes": MAX_ANNOTATION_RATIONALE_BYTES,
                },
                "evidence": {
                    "type": "array",
                    "items": evidence,
                    "maxItems": MAX_EVIDENCE_REFS,
                },
            },
        })
    }
}
