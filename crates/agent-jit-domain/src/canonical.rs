//! Canonical JSON and reproducible digests.
//!
//! Every persisted record is digested through one canonical form so that decisions taken today can
//! be recomputed and audited later. The rules are deliberately narrow:
//!
//! * object keys are sorted by Unicode scalar value after NFC normalization;
//! * array order is significant and preserved;
//! * every string and key is NFC-normalized, and keys that collide after normalization are an error;
//! * numbers must be integers — a float in a record is a bug, not a rounding question;
//! * volatile fields (wall clock, durations, process ids) are excluded from the *projection* that
//!   is digested, while the raw record keeps them.

use std::collections::BTreeMap;
use std::fmt;
use std::str::FromStr;

use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};
use unicode_normalization::{IsNormalized, UnicodeNormalization, is_nfc_quick};

/// Length of a rendered [`Digest`] in hexadecimal characters.
const DIGEST_HEX_LEN: usize = 64;

/// A BLAKE3 digest over a canonical JSON byte string.
#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(try_from = "String", into = "String")]
pub struct Digest([u8; 32]);

impl Digest {
    /// Returns the raw digest bytes.
    #[must_use]
    pub const fn as_bytes(&self) -> &[u8; 32] {
        &self.0
    }

    /// Digests an arbitrary byte string.
    #[must_use]
    pub fn of_bytes(bytes: &[u8]) -> Self {
        Self(*blake3::hash(bytes).as_bytes())
    }
}

impl fmt::Display for Digest {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        for byte in self.0 {
            write!(f, "{byte:02x}")?;
        }
        Ok(())
    }
}

impl fmt::Debug for Digest {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "Digest({self})")
    }
}

impl FromStr for Digest {
    type Err = CanonicalError;

    fn from_str(text: &str) -> Result<Self, Self::Err> {
        if text.len() != DIGEST_HEX_LEN || !text.bytes().all(|b| b.is_ascii_hexdigit()) {
            return Err(CanonicalError::MalformedDigest {
                value: text.to_owned(),
            });
        }
        if text.bytes().any(|b| b.is_ascii_uppercase()) {
            return Err(CanonicalError::MalformedDigest {
                value: text.to_owned(),
            });
        }

        let mut bytes = [0_u8; 32];
        for (index, slot) in bytes.iter_mut().enumerate() {
            let start = index * 2;
            let pair = text
                .get(start..start + 2)
                .ok_or(CanonicalError::MalformedDigest {
                    value: text.to_owned(),
                })?;
            *slot = u8::from_str_radix(pair, 16).map_err(|_| CanonicalError::MalformedDigest {
                value: text.to_owned(),
            })?;
        }
        Ok(Self(bytes))
    }
}

impl TryFrom<String> for Digest {
    type Error = CanonicalError;

    fn try_from(value: String) -> Result<Self, Self::Error> {
        value.parse()
    }
}

impl From<Digest> for String {
    fn from(digest: Digest) -> Self {
        digest.to_string()
    }
}

impl schemars::JsonSchema for Digest {
    fn schema_name() -> std::borrow::Cow<'static, str> {
        "Digest".into()
    }

    fn json_schema(_generator: &mut schemars::SchemaGenerator) -> schemars::Schema {
        schemars::json_schema!({
            "type": "string",
            "pattern": "^[0-9a-f]{64}$",
            "description": "BLAKE3 digest of a canonical JSON byte string, lowercase hex.",
        })
    }
}

/// Every way canonicalization can refuse a value.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum CanonicalError {
    /// A number was not an integer, so its digest would depend on float formatting.
    #[error("non-integer number at `{pointer}`; metrics must be integers")]
    NonIntegerNumber {
        /// JSON pointer of the offending value.
        pointer: String,
    },
    /// Two object keys became identical once NFC-normalized.
    #[error("duplicate key `{key}` at `{pointer}` after NFC normalization")]
    DuplicateKey {
        /// JSON pointer of the containing object.
        pointer: String,
        /// The normalized key that collided.
        key: String,
    },
    /// A declared volatile pointer matched nothing, so the projection is not what it claims.
    #[error("volatile pointer `{pointer}` matched no value")]
    UnknownPointer {
        /// The pointer that matched nothing.
        pointer: String,
    },
    /// A volatile pointer was not a valid JSON pointer.
    #[error("`{pointer}` is not a valid JSON pointer")]
    MalformedPointer {
        /// The rejected pointer text.
        pointer: String,
    },
    /// A digest string was not 64 lowercase hex characters.
    #[error("`{value}` is not a lowercase 64-character hex digest")]
    MalformedDigest {
        /// The rejected text.
        value: String,
    },
    /// A record could not be represented as JSON at all.
    #[error("value is not representable as canonical JSON: {reason}")]
    NotRepresentable {
        /// Human-readable reason from the serializer.
        reason: String,
    },
}

impl CanonicalError {
    /// Stable machine-readable code, safe to match on in tests and to surface at the CLI boundary.
    #[must_use]
    pub const fn code(&self) -> &'static str {
        match self {
            Self::NonIntegerNumber { .. } => "canonical_non_integer_number",
            Self::DuplicateKey { .. } => "canonical_duplicate_key",
            Self::UnknownPointer { .. } => "canonical_unknown_pointer",
            Self::MalformedPointer { .. } => "canonical_malformed_pointer",
            Self::MalformedDigest { .. } => "canonical_malformed_digest",
            Self::NotRepresentable { .. } => "canonical_not_representable",
        }
    }
}

/// Renders `value` as a canonical JSON string.
///
/// # Errors
///
/// Returns [`CanonicalError`] when the value contains a float or keys that collide under NFC.
pub fn canonical_string(value: &Value) -> Result<String, CanonicalError> {
    let mut out = String::new();
    write_canonical(value, "", &mut out)?;
    Ok(out)
}

/// Serializes `record` and renders it as a canonical JSON string.
///
/// # Errors
///
/// Returns [`CanonicalError`] when the record cannot be serialized or is not canonicalizable.
pub fn canonical_string_of<T: Serialize>(record: &T) -> Result<String, CanonicalError> {
    canonical_string(&to_value(record)?)
}

/// Digests the canonical form of `value`.
///
/// # Errors
///
/// Returns [`CanonicalError`] when the value is not canonicalizable.
pub fn digest_of(value: &Value) -> Result<Digest, CanonicalError> {
    Ok(Digest::of_bytes(canonical_string(value)?.as_bytes()))
}

/// Digests the canonical form of `value` with every `volatile` pointer removed first.
///
/// Each pointer must match a value that is actually present: a projection that silently ignores a
/// stale pointer would quietly start digesting a volatile field.
///
/// # Errors
///
/// Returns [`CanonicalError`] when a pointer is malformed, matches nothing, or when the remaining
/// value is not canonicalizable.
pub fn digest_projection(value: &Value, volatile: &[&str]) -> Result<Digest, CanonicalError> {
    digest_of(&projection(value, volatile)?)
}

/// Returns a copy of `value` with every `volatile` pointer removed.
///
/// # Errors
///
/// Returns [`CanonicalError`] when a pointer is malformed or matches nothing.
pub fn projection(value: &Value, volatile: &[&str]) -> Result<Value, CanonicalError> {
    let mut projected = value.clone();
    for pointer in volatile {
        remove_pointer(&mut projected, pointer)?;
    }
    Ok(projected)
}

/// Serializes `record` into a [`Value`].
///
/// # Errors
///
/// Returns [`CanonicalError::NotRepresentable`] when serialization fails.
pub fn to_value<T: Serialize>(record: &T) -> Result<Value, CanonicalError> {
    serde_json::to_value(record).map_err(|error| CanonicalError::NotRepresentable {
        reason: error.to_string(),
    })
}

/// Normalizes `text` to NFC, borrowing when it already is.
fn nfc(text: &str) -> std::borrow::Cow<'_, str> {
    match is_nfc_quick(text.chars()) {
        IsNormalized::Yes => std::borrow::Cow::Borrowed(text),
        _ => std::borrow::Cow::Owned(text.nfc().collect()),
    }
}

fn write_canonical(value: &Value, pointer: &str, out: &mut String) -> Result<(), CanonicalError> {
    match value {
        Value::Null => out.push_str("null"),
        Value::Bool(flag) => out.push_str(if *flag { "true" } else { "false" }),
        Value::Number(number) => {
            if number.is_f64() {
                return Err(CanonicalError::NonIntegerNumber {
                    pointer: pointer.to_owned(),
                });
            }
            out.push_str(&number.to_string());
        }
        Value::String(text) => write_json_string(&nfc(text), out),
        Value::Array(items) => {
            out.push('[');
            for (index, item) in items.iter().enumerate() {
                if index > 0 {
                    out.push(',');
                }
                write_canonical(item, &format!("{pointer}/{index}"), out)?;
            }
            out.push(']');
        }
        Value::Object(members) => write_canonical_object(members, pointer, out)?,
    }
    Ok(())
}

fn write_canonical_object(
    members: &Map<String, Value>,
    pointer: &str,
    out: &mut String,
) -> Result<(), CanonicalError> {
    let mut sorted: BTreeMap<String, &Value> = BTreeMap::new();
    for (key, member) in members {
        let normalized = nfc(key).into_owned();
        if sorted.insert(normalized.clone(), member).is_some() {
            return Err(CanonicalError::DuplicateKey {
                pointer: pointer.to_owned(),
                key: normalized,
            });
        }
    }

    out.push('{');
    for (index, (key, member)) in sorted.iter().enumerate() {
        if index > 0 {
            out.push(',');
        }
        write_json_string(key, out);
        out.push(':');
        write_canonical(
            member,
            &format!("{pointer}/{}", escape_pointer_token(key)),
            out,
        )?;
    }
    out.push('}');
    Ok(())
}

/// Writes `text` as a JSON string literal using the shortest legal escapes.
fn write_json_string(text: &str, out: &mut String) {
    out.push('"');
    for character in text.chars() {
        match character {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            '\u{08}' => out.push_str("\\b"),
            '\u{0c}' => out.push_str("\\f"),
            control if control < '\u{20}' => {
                use std::fmt::Write as _;
                let _ = write!(out, "\\u{:04x}", control as u32);
            }
            other => out.push(other),
        }
    }
    out.push('"');
}

fn escape_pointer_token(token: &str) -> String {
    token.replace('~', "~0").replace('/', "~1")
}

fn unescape_pointer_token(token: &str) -> String {
    token.replace("~1", "/").replace("~0", "~")
}

/// Removes the value at `pointer` from `value`, failing when it is absent.
fn remove_pointer(value: &mut Value, pointer: &str) -> Result<(), CanonicalError> {
    if pointer.is_empty() || !pointer.starts_with('/') {
        return Err(CanonicalError::MalformedPointer {
            pointer: pointer.to_owned(),
        });
    }

    let tokens: Vec<String> = pointer
        .split('/')
        .skip(1)
        .map(unescape_pointer_token)
        .collect();
    let (last, parents) = tokens
        .split_last()
        .ok_or(CanonicalError::MalformedPointer {
            pointer: pointer.to_owned(),
        })?;

    let mut cursor = value;
    for token in parents {
        cursor = descend(cursor, token).ok_or_else(|| CanonicalError::UnknownPointer {
            pointer: pointer.to_owned(),
        })?;
    }

    let removed = match cursor {
        Value::Object(members) => members.remove(last.as_str()).is_some(),
        Value::Array(items) => match last.parse::<usize>() {
            Ok(index) if index < items.len() => {
                items.remove(index);
                true
            }
            _ => false,
        },
        _ => false,
    };

    if removed {
        Ok(())
    } else {
        Err(CanonicalError::UnknownPointer {
            pointer: pointer.to_owned(),
        })
    }
}

fn descend<'a>(value: &'a mut Value, token: &str) -> Option<&'a mut Value> {
    match value {
        Value::Object(members) => members.get_mut(token),
        Value::Array(items) => token.parse::<usize>().ok().and_then(|i| items.get_mut(i)),
        _ => None,
    }
}
