//! Dependency fingerprints.
//!
//! A compiled capability is only valid while the things it depends on are unchanged. A
//! fingerprint records *what was looked at* alongside the digest, including files that were
//! absent: "the lockfile did not exist" and "the lockfile exists and is empty" are different
//! worlds, and a capability validated in one must not silently run in the other.

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use crate::canonical::{CanonicalError, Digest, digest_of, to_value};

/// What class of dependency an input belongs to.
///
/// The kind participates in the digest, so the same file observed as a lockfile and as a source
/// path produces different fingerprints — they invalidate for different reasons.
#[derive(
    Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize, JsonSchema,
)]
#[serde(rename_all = "snake_case")]
pub enum FingerprintKind {
    /// The commit currently checked out.
    Head,
    /// The uncommitted diff in the worktree.
    DirtyDiff,
    /// A dependency lockfile.
    Lockfile,
    /// A repository configuration file.
    ConfigFile,
    /// A source path the capability reads.
    SourcePath,
    /// The pinned set of commands a capability may run.
    CommandProfile,
    /// The compiled capability version itself.
    CapabilityVersion,
    /// A runtime version the capability depends on, such as the sandbox sidecar.
    RuntimeVersion,
}

impl FingerprintKind {
    /// Returns the `snake_case` name of the kind.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Head => "head",
            Self::DirtyDiff => "dirty_diff",
            Self::Lockfile => "lockfile",
            Self::ConfigFile => "config_file",
            Self::SourcePath => "source_path",
            Self::CommandProfile => "command_profile",
            Self::CapabilityVersion => "capability_version",
            Self::RuntimeVersion => "runtime_version",
        }
    }
}

/// One observed dependency.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct FingerprintInput {
    /// Class of dependency.
    pub kind: FingerprintKind,
    /// Stable key: a repository-relative path, a version name, or another sorted identifier.
    pub key: String,
    /// Digest of the content, or `None` when the dependency was absent.
    pub digest: Option<Digest>,
}

/// A sorted set of observed dependencies and the digest over all of them.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct Fingerprint {
    /// Inputs, sorted by `(kind, key)` so the digest never depends on observation order.
    pub inputs: Vec<FingerprintInput>,
    /// Digest over the sorted inputs.
    pub digest: Digest,
}

impl Fingerprint {
    /// Builds a fingerprint from `inputs`, sorting them and digesting the result.
    ///
    /// # Errors
    ///
    /// Returns [`CanonicalError`] when the inputs cannot be canonicalized.
    pub fn new(mut inputs: Vec<FingerprintInput>) -> Result<Self, CanonicalError> {
        inputs.sort_by(|left, right| {
            (left.kind.as_str(), &left.key).cmp(&(right.kind.as_str(), &right.key))
        });
        let digest = digest_of(&to_value(&inputs)?)?;
        Ok(Self { inputs, digest })
    }

    /// Whether this fingerprint still matches `other`.
    #[must_use]
    pub fn matches(&self, other: &Self) -> bool {
        self.digest == other.digest
    }
}
