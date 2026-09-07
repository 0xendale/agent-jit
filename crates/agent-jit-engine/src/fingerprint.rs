//! Computing dependency fingerprints from a worktree.
//!
//! Content decides the digest. Timestamps do not: a rebuild that rewrites identical bytes must not
//! invalidate a capability, and a file whose bytes changed must. Paths are repository-relative and
//! may not leave the worktree, directly or through a symlink — a fingerprint that could reach
//! outside the repository would let anything on the machine decide whether a capability runs.

use std::path::{Component, Path, PathBuf};

use agent_jit_domain::canonical::{CanonicalError, Digest};
use agent_jit_domain::fingerprint::{Fingerprint, FingerprintInput, FingerprintKind};

/// Paths that are never hashed, because their contents are secrets rather than dependencies.
const SECRET_PATHS: &[&str] = &[
    ".env",
    ".env.local",
    ".envrc",
    ".netrc",
    ".npmrc",
    ".pypirc",
    "id_rsa",
    "id_ed25519",
];

/// What to fingerprint.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FingerprintRequest {
    /// Repository-relative paths to observe. Order does not matter.
    pub paths: Vec<String>,
    /// Class the paths belong to.
    pub kind: FingerprintKind,
}

/// Why a fingerprint could not be computed.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum FingerprintError {
    /// The path left the worktree, or was absolute.
    #[error("`{path}` is not a repository-relative path inside the worktree")]
    PathEscape {
        /// The rejected path.
        path: String,
    },
    /// The path resolved through a symlink pointing outside the worktree.
    #[error("`{path}` is a symlink leaving the worktree")]
    SymlinkEscape {
        /// The rejected path.
        path: String,
    },
    /// The path names a known secret file.
    #[error("`{path}` holds credentials and is never hashed")]
    SecretPath {
        /// The rejected path.
        path: String,
    },
    /// The path exists but is not a regular file.
    #[error("`{path}` is not a regular file")]
    NotAFile {
        /// The rejected path.
        path: String,
    },
    /// The file exists but could not be read.
    #[error("`{path}` could not be read: {reason}")]
    Unreadable {
        /// The path that could not be read.
        path: String,
        /// Reason reported by the operating system.
        reason: String,
    },
    /// The inputs could not be digested.
    #[error("{}: {source}", source.code())]
    Canonical {
        /// The underlying canonicalization failure.
        #[from]
        source: CanonicalError,
    },
}

impl FingerprintError {
    /// Stable machine-readable code.
    #[must_use]
    pub const fn code(&self) -> &'static str {
        match self {
            Self::PathEscape { .. } => "fingerprint_path_escape",
            Self::SymlinkEscape { .. } => "fingerprint_symlink_escape",
            Self::SecretPath { .. } => "fingerprint_secret_path",
            Self::NotAFile { .. } => "fingerprint_not_a_file",
            Self::Unreadable { .. } => "fingerprint_unreadable",
            Self::Canonical { source } => source.code(),
        }
    }
}

/// Computes a fingerprint over `request` inside `worktree_root`.
///
/// # Errors
///
/// Returns [`FingerprintError`] when a path escapes the worktree, names a secret file, is not a
/// regular file, or cannot be read.
pub fn compute(
    worktree_root: &Path,
    request: &FingerprintRequest,
) -> Result<Fingerprint, FingerprintError> {
    let root = worktree_root
        .canonicalize()
        .map_err(|error| FingerprintError::Unreadable {
            path: worktree_root.display().to_string(),
            reason: error.to_string(),
        })?;

    let mut inputs = Vec::with_capacity(request.paths.len());
    for path in &request.paths {
        inputs.push(observe(&root, path, request.kind)?);
    }

    Ok(Fingerprint::new(inputs)?)
}

/// Observes one path, recording either its content digest or an absence marker.
fn observe(
    root: &Path,
    relative: &str,
    kind: FingerprintKind,
) -> Result<FingerprintInput, FingerprintError> {
    let candidate = Path::new(relative);
    if candidate.is_absolute()
        || candidate
            .components()
            .any(|component| matches!(component, Component::ParentDir))
    {
        return Err(FingerprintError::PathEscape {
            path: relative.to_owned(),
        });
    }

    if is_secret(relative) {
        return Err(FingerprintError::SecretPath {
            path: relative.to_owned(),
        });
    }

    let joined = root.join(candidate);
    let resolved = match joined.canonicalize() {
        Ok(resolved) => resolved,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            // Absence is a fact about the repository, not a reason to skip the input.
            return Ok(FingerprintInput {
                kind,
                key: relative.to_owned(),
                digest: None,
            });
        }
        Err(error) => {
            return Err(FingerprintError::Unreadable {
                path: relative.to_owned(),
                reason: error.to_string(),
            });
        }
    };

    if !resolved.starts_with(root) {
        // The path itself was well-formed, so only a symlink can have left the worktree.
        return Err(FingerprintError::SymlinkEscape {
            path: relative.to_owned(),
        });
    }

    let metadata = std::fs::metadata(&resolved).map_err(|error| FingerprintError::Unreadable {
        path: relative.to_owned(),
        reason: error.to_string(),
    })?;
    if !metadata.is_file() {
        return Err(FingerprintError::NotAFile {
            path: relative.to_owned(),
        });
    }

    let contents = std::fs::read(&resolved).map_err(|error| FingerprintError::Unreadable {
        path: relative.to_owned(),
        reason: error.to_string(),
    })?;

    Ok(FingerprintInput {
        kind,
        key: relative.to_owned(),
        digest: Some(Digest::of_bytes(&contents)),
    })
}

/// Whether `relative` names a file that holds credentials.
fn is_secret(relative: &str) -> bool {
    let name = Path::new(relative)
        .file_name()
        .map(|name| name.to_string_lossy().into_owned())
        .unwrap_or_default();
    SECRET_PATHS.contains(&name.as_str())
}

/// Returns the paths a fingerprint request observed, for reporting.
#[must_use]
pub fn observed_paths(fingerprint: &Fingerprint) -> Vec<PathBuf> {
    fingerprint
        .inputs
        .iter()
        .map(|input| PathBuf::from(&input.key))
        .collect()
}
