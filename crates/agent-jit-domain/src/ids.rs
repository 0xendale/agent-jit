//! Branded identifiers.
//!
//! Every record kind gets its own identifier type. A [`SessionId`] cannot be passed where a
//! [`TrajectoryId`] is expected, and the rendered form carries a kind prefix so a mistake in a
//! stored record is caught at parse time rather than becoming a silent join against nothing.
//!
//! The body is 26 Crockford base32 characters (the ULID alphabet): digits plus uppercase letters
//! with `I`, `L`, `O`, and `U` excluded so visually ambiguous identifiers cannot exist.

use std::cmp::Ordering;
use std::fmt;
use std::marker::PhantomData;
use std::str::FromStr;

use serde::{Deserialize, Deserializer, Serialize, Serializer};

/// Number of Crockford base32 characters in an identifier body.
pub const ID_BODY_LEN: usize = 26;

/// Crockford base32 alphabet: `0-9` and `A-Z` minus `I`, `L`, `O`, and `U`.
const ALPHABET: &[u8] = b"0123456789ABCDEFGHJKMNPQRSTVWXYZ";

/// A record kind that owns an identifier prefix.
pub trait IdKind: Copy + Clone + fmt::Debug + PartialEq + Eq + 'static {
    /// Lowercase prefix rendered before the underscore, for example `trj`.
    const PREFIX: &'static str;
    /// Human-readable kind name used in error messages.
    const NAME: &'static str;
}

/// Marker types for every identifier kind in the product.
pub mod kind {
    use super::IdKind;

    /// Declares a zero-sized marker type implementing [`IdKind`].
    macro_rules! declare_kind {
        ($(#[$doc:meta] $name:ident => $prefix:literal),* $(,)?) => {
            $(
                #[$doc]
                #[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
                pub struct $name;

                impl IdKind for $name {
                    const PREFIX: &'static str = $prefix;
                    const NAME: &'static str = stringify!($name);
                }
            )*
        };
    }

    declare_kind! {
        /// A Git repository observed by the recorder.
        Repository => "rep",
        /// One agent session against one repository.
        Session => "ses",
        /// One recorded event inside a session.
        Event => "evt",
        /// One normalized trajectory: intent, events, and outcome.
        Trajectory => "trj",
        /// The recorded result of a trajectory.
        Outcome => "out",
        /// A human-confirmed group of trajectories solving one intent.
        Group => "grp",
        /// A candidate capability contract inferred from a group.
        Candidate => "cnd",
        /// A compiled workflow DAG.
        Workflow => "wfl",
        /// One replay of a compiled workflow against a historical fixture.
        Replay => "rpl",
        /// An approved, versioned capability.
        Capability => "cap",
        /// One runtime invocation of a capability.
        Invocation => "inv",
        /// One benchmark record.
        Benchmark => "bmk",
    }
}

/// Why an identifier was rejected.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum IdError {
    /// The prefix belonged to a different record kind.
    #[error("expected a `{expected}` identifier (prefix `{expected_prefix}_`), found `{found}`")]
    PrefixMismatch {
        /// Kind name that was expected.
        expected: &'static str,
        /// Prefix that was expected.
        expected_prefix: &'static str,
        /// The rejected text.
        found: String,
    },
    /// The text was not `<prefix>_<26 Crockford base32 characters>`.
    #[error("`{found}` is not a well-formed {expected} identifier")]
    Malformed {
        /// Kind name that was expected.
        expected: &'static str,
        /// The rejected text.
        found: String,
    },
}

impl IdError {
    /// Stable machine-readable code.
    #[must_use]
    pub const fn code(&self) -> &'static str {
        match self {
            Self::PrefixMismatch { .. } => "id_prefix_mismatch",
            Self::Malformed { .. } => "id_malformed",
        }
    }
}

/// A branded identifier for record kind `K`.
#[derive(Clone, Copy)]
pub struct Id<K: IdKind> {
    body: [u8; ID_BODY_LEN],
    kind: PhantomData<K>,
}

impl<K: IdKind> Id<K> {
    /// Builds an identifier from an already-validated body.
    ///
    /// # Errors
    ///
    /// Returns [`IdError::Malformed`] when `body` is not 26 Crockford base32 characters.
    pub fn from_body(body: &str) -> Result<Self, IdError> {
        let bytes = body.as_bytes();
        let well_formed =
            bytes.len() == ID_BODY_LEN && bytes.iter().all(|byte| ALPHABET.contains(byte));
        if !well_formed {
            return Err(IdError::Malformed {
                expected: K::NAME,
                found: body.to_owned(),
            });
        }

        let mut stored = [0_u8; ID_BODY_LEN];
        stored.copy_from_slice(bytes);
        Ok(Self {
            body: stored,
            kind: PhantomData,
        })
    }

    /// Derives a deterministic identifier from a digest.
    ///
    /// Identity that must be recomputed from the same inputs on a later run — a repository, a
    /// group manifest — cannot use a random identifier. The body is the first 130 bits of the
    /// digest in Crockford base32, so the same inputs always address the same record.
    #[must_use]
    pub fn derived(digest: &crate::canonical::Digest) -> Self {
        let bytes = digest.as_bytes();
        let mut body = [0_u8; ID_BODY_LEN];
        for (index, slot) in body.iter_mut().enumerate() {
            let bit = index * 5;
            let byte = bit / 8;
            let offset = bit % 8;
            // Read five bits, spanning the byte boundary when necessary.
            let window = (u16::from(bytes[byte]) << 8) | u16::from(bytes[byte + 1]);
            let value = ((window >> (11 - offset)) & 0x1f) as usize;
            *slot = ALPHABET[value];
        }
        Self {
            body,
            kind: PhantomData,
        }
    }

    /// Returns the identifier body without its prefix.
    #[must_use]
    pub fn body(&self) -> &str {
        // The body is validated ASCII on construction, so this cannot fail.
        std::str::from_utf8(&self.body).unwrap_or("")
    }
}

impl<K: IdKind> fmt::Display for Id<K> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}_{}", K::PREFIX, self.body())
    }
}

impl<K: IdKind> fmt::Debug for Id<K> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}({self})", K::NAME)
    }
}

impl<K: IdKind> FromStr for Id<K> {
    type Err = IdError;

    fn from_str(text: &str) -> Result<Self, Self::Err> {
        let Some((prefix, body)) = text.split_once('_') else {
            return Err(IdError::Malformed {
                expected: K::NAME,
                found: text.to_owned(),
            });
        };

        if prefix != K::PREFIX {
            return Err(IdError::PrefixMismatch {
                expected: K::NAME,
                expected_prefix: K::PREFIX,
                found: text.to_owned(),
            });
        }

        Self::from_body(body).map_err(|_| IdError::Malformed {
            expected: K::NAME,
            found: text.to_owned(),
        })
    }
}

impl<K: IdKind> PartialEq for Id<K> {
    fn eq(&self, other: &Self) -> bool {
        self.body == other.body
    }
}

impl<K: IdKind> Eq for Id<K> {}

impl<K: IdKind> PartialOrd for Id<K> {
    fn partial_cmp(&self, other: &Self) -> Option<Ordering> {
        Some(self.cmp(other))
    }
}

impl<K: IdKind> Ord for Id<K> {
    fn cmp(&self, other: &Self) -> Ordering {
        self.body.cmp(&other.body)
    }
}

impl<K: IdKind> std::hash::Hash for Id<K> {
    fn hash<H: std::hash::Hasher>(&self, state: &mut H) {
        self.body.hash(state);
    }
}

impl<K: IdKind> Serialize for Id<K> {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        serializer.serialize_str(&self.to_string())
    }
}

impl<'de, K: IdKind> Deserialize<'de> for Id<K> {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let text = String::deserialize(deserializer)?;
        text.parse().map_err(|error: IdError| {
            serde::de::Error::custom(format!("{}: {error}", error.code()))
        })
    }
}

impl<K: IdKind> schemars::JsonSchema for Id<K> {
    fn schema_name() -> std::borrow::Cow<'static, str> {
        format!("{}Id", K::NAME).into()
    }

    fn json_schema(_generator: &mut schemars::SchemaGenerator) -> schemars::Schema {
        let pattern = format!("^{}_[0-9A-HJKMNP-TV-Z]{{{ID_BODY_LEN}}}$", K::PREFIX);
        schemars::json_schema!({
            "type": "string",
            "pattern": pattern,
            "description": format!("Identifier for a {} record.", K::NAME),
        })
    }
}

/// Declares the concrete identifier aliases used across the workspace.
macro_rules! alias {
    ($(#[$doc:meta] $alias:ident => $kind:ident),* $(,)?) => {
        $(
            #[$doc]
            pub type $alias = Id<kind::$kind>;
        )*
    };
}

alias! {
    /// Identifies a Git repository.
    RepositoryId => Repository,
    /// Identifies an agent session.
    SessionId => Session,
    /// Identifies a recorded event.
    EventId => Event,
    /// Identifies a normalized trajectory.
    TrajectoryId => Trajectory,
    /// Identifies a recorded outcome.
    OutcomeId => Outcome,
    /// Identifies a confirmed group.
    GroupId => Group,
    /// Identifies a candidate contract.
    CandidateId => Candidate,
    /// Identifies a compiled workflow.
    WorkflowId => Workflow,
    /// Identifies a replay run.
    ReplayId => Replay,
    /// Identifies an approved capability version.
    CapabilityId => Capability,
    /// Identifies a runtime invocation.
    InvocationId => Invocation,
    /// Identifies a benchmark record.
    BenchmarkId => Benchmark,
}
