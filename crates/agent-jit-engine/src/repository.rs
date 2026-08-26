//! Repository discovery and identity.
//!
//! Identity comes from the Git *common* directory, not the worktree: `git worktree add` creates a
//! second checkout of one repository, and grouping runs from both must land in the same place.
//! Every record still carries the worktree root and commit it actually ran in, so a fingerprint
//! can distinguish them when that matters.

use std::path::{Component, Path, PathBuf};

use agent_jit_domain::canonical::{CanonicalError, digest_of};
use agent_jit_domain::ids::RepositoryId;
use serde_json::json;

use crate::process::{ProcessError, Runner};

/// Absolute path of the Git executable. Never resolved through `PATH`.
pub const GIT: &str = "/usr/bin/git";

/// A discovered repository.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RepositoryIdentity {
    /// Stable identifier derived from the Git common directory.
    pub repo_id: RepositoryId,
    /// Absolute path of the Git common directory shared by every worktree.
    pub git_common_dir: PathBuf,
    /// Absolute path of the worktree that was inspected.
    pub worktree_root: PathBuf,
    /// Commit checked out in that worktree.
    pub head_commit: String,
}

/// Why discovery failed.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum RepositoryError {
    /// The requested root contained `..`, so it could name anything.
    #[error("`{root}` contains a parent-directory component")]
    Traversal {
        /// The rejected path.
        root: String,
    },
    /// The requested root is itself a symlink, so it can be repointed at another repository.
    #[error("`{root}` is a symlink; pass the real directory")]
    RootIsSymlink {
        /// The rejected path.
        root: String,
    },
    /// The requested root does not exist or cannot be read.
    #[error("`{root}` cannot be read: {reason}")]
    RootUnreadable {
        /// The rejected path.
        root: String,
        /// Reason reported by the operating system.
        reason: String,
    },
    /// The path exists but is not inside a Git repository.
    #[error("`{root}` is not inside a Git repository")]
    NotARepository {
        /// The inspected path.
        root: String,
    },
    /// Git ran but produced something unusable.
    #[error("`git {argv}` produced no usable output: {detail}")]
    GitOutput {
        /// The Git arguments that were run.
        argv: String,
        /// What was wrong with the output.
        detail: String,
    },
    /// Git could not be run at all.
    #[error("{}: {source}", source.code())]
    Process {
        /// The underlying process failure.
        #[from]
        source: ProcessError,
    },
    /// The identity could not be digested.
    #[error("{}: {source}", source.code())]
    Canonical {
        /// The underlying canonicalization failure.
        #[from]
        source: CanonicalError,
    },
}

impl RepositoryError {
    /// Stable machine-readable code.
    #[must_use]
    pub const fn code(&self) -> &'static str {
        match self {
            Self::Traversal { .. } => "repo_path_traversal",
            Self::RootIsSymlink { .. } => "repo_root_symlink",
            Self::RootUnreadable { .. } => "repo_root_unreadable",
            Self::NotARepository { .. } => "repo_not_a_repository",
            Self::GitOutput { .. } => "repo_git_output",
            Self::Process { source } => source.code(),
            Self::Canonical { source } => source.code(),
        }
    }
}

/// Discovers the repository containing `root`.
///
/// # Errors
///
/// Returns [`RepositoryError`] when the path traverses upward, cannot be read, is not inside a
/// repository, or when Git cannot be run.
pub fn discover(runner: &impl Runner, root: &Path) -> Result<RepositoryIdentity, RepositoryError> {
    let root = validated_root(root)?;

    let worktree_root = git_path(runner, &root, "--show-toplevel")?;
    let common_dir_raw = git_line(runner, &root, &["rev-parse", "--git-common-dir"])?;
    // Git answers relatively (`.git`), and relative to the directory the query ran in.
    let common_dir = resolve(&root, Path::new(&common_dir_raw)).map_err(|reason| {
        RepositoryError::RootUnreadable {
            root: common_dir_raw.clone(),
            reason,
        }
    })?;
    let head_commit = git_line(runner, &root, &["rev-parse", "HEAD"])?;

    let identity = json!({
        "kind": "git_common_dir",
        "git_common_dir": common_dir.to_string_lossy(),
    });

    Ok(RepositoryIdentity {
        repo_id: RepositoryId::derived(&digest_of(&identity)?),
        git_common_dir: common_dir,
        worktree_root,
        head_commit,
    })
}

/// Rejects upward traversal, then resolves the path to something that exists.
fn validated_root(root: &Path) -> Result<PathBuf, RepositoryError> {
    if root
        .components()
        .any(|component| matches!(component, Component::ParentDir))
    {
        return Err(RepositoryError::Traversal {
            root: root.display().to_string(),
        });
    }

    // A symlink can be repointed between runs, so identity taken through one is not identity.
    if let Ok(metadata) = std::fs::symlink_metadata(root)
        && metadata.file_type().is_symlink()
    {
        return Err(RepositoryError::RootIsSymlink {
            root: root.display().to_string(),
        });
    }

    root.canonicalize()
        .map_err(|error| RepositoryError::RootUnreadable {
            root: root.display().to_string(),
            reason: error.to_string(),
        })
}

/// Runs `git rev-parse <flag>` and resolves the answer to an absolute path.
fn git_path(runner: &impl Runner, root: &Path, flag: &str) -> Result<PathBuf, RepositoryError> {
    let line = git_line(runner, root, &["rev-parse", flag])?;
    resolve(root, Path::new(&line))
        .map_err(|reason| RepositoryError::RootUnreadable { root: line, reason })
}

/// Runs a Git command and returns its single line of output.
fn git_line(runner: &impl Runner, root: &Path, argv: &[&str]) -> Result<String, RepositoryError> {
    let output = runner.run(GIT, argv, Some(root))?;
    if !output.success() {
        // Git reports "not a git repository" on stderr with a nonzero status.
        return Err(RepositoryError::NotARepository {
            root: root.display().to_string(),
        });
    }

    let line = output.stdout.trim().to_owned();
    if line.is_empty() {
        return Err(RepositoryError::GitOutput {
            argv: argv.join(" "),
            detail: "empty output".to_owned(),
        });
    }
    Ok(line)
}

/// Resolves `path` against `base` when relative, then canonicalizes it.
fn resolve(base: &Path, path: &Path) -> Result<PathBuf, String> {
    let joined = if path.is_absolute() {
        path.to_path_buf()
    } else {
        base.join(path)
    };
    joined.canonicalize().map_err(|error| error.to_string())
}
