//! The append-only intake spool.
//!
//! Hooks run on the critical path of an interactive session, so ingestion does exactly one thing:
//! append one line to a private file and return. Parsing, correlation, and the database happen
//! later, in the recorder. That keeps hook latency bounded and makes a crash recoverable — a
//! half-written session leaves a spool that can be drained, not a corrupt transaction.

use std::fs::{DirBuilder, File, OpenOptions, Permissions};
use std::io::Write as _;
use std::os::unix::fs::{DirBuilderExt as _, OpenOptionsExt as _, PermissionsExt as _};
use std::path::{Path, PathBuf};

/// Mode for the spool directory.
const SPOOL_DIR_MODE: u32 = 0o700;

/// Mode for every spool file.
const SPOOL_FILE_MODE: u32 = 0o600;

/// Maximum bytes for one spooled line, after redaction and bounding upstream.
pub const MAX_LINE_BYTES: usize = 256 * 1024;

/// Size at which the active file is rotated.
pub const ROTATE_AT_BYTES: u64 = 64 * 1024 * 1024;

/// Which spool a line belongs to.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SpoolKind {
    /// Normalized hook events awaiting the recorder.
    Events,
    /// Recorder health counters: malformed input, refusals, faults.
    Health,
}

impl SpoolKind {
    /// Base file name for this spool.
    #[must_use]
    pub const fn base_name(self) -> &'static str {
        match self {
            Self::Events => "events",
            Self::Health => "health",
        }
    }
}

/// Why a spool operation failed.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum SpoolError {
    /// The spool path is a symlink and could redirect captured data elsewhere.
    #[error("`{path}` is a symlink; the spool must be a real path")]
    Symlink {
        /// The rejected path.
        path: String,
    },
    /// A line exceeded the spool's own ceiling.
    #[error("spool line is {bytes} bytes; the ceiling is {MAX_LINE_BYTES}")]
    LineTooLarge {
        /// Size of the offending line.
        bytes: usize,
    },
    /// The filesystem refused an operation.
    #[error("`{path}`: {reason}")]
    Io {
        /// The path involved.
        path: String,
        /// Reason reported by the operating system.
        reason: String,
    },
}

impl SpoolError {
    /// Stable machine-readable code.
    #[must_use]
    pub const fn code(&self) -> &'static str {
        match self {
            Self::Symlink { .. } => "spool_symlink",
            Self::LineTooLarge { .. } => "spool_line_too_large",
            Self::Io { .. } => "spool_io",
        }
    }
}

/// A private append-only spool directory.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Spool {
    directory: PathBuf,
}

impl Spool {
    /// Opens (creating if needed) the spool directory at `directory`.
    ///
    /// # Errors
    ///
    /// Returns [`SpoolError`] when the path is a symlink or cannot be created privately.
    pub fn open(directory: &Path) -> Result<Self, SpoolError> {
        if let Ok(metadata) = std::fs::symlink_metadata(directory)
            && metadata.file_type().is_symlink()
        {
            return Err(SpoolError::Symlink {
                path: directory.display().to_string(),
            });
        }

        if !directory.exists() {
            DirBuilder::new()
                .recursive(true)
                .mode(SPOOL_DIR_MODE)
                .create(directory)
                .map_err(|error| SpoolError::Io {
                    path: directory.display().to_string(),
                    reason: error.to_string(),
                })?;
        }

        Ok(Self {
            directory: directory.to_path_buf(),
        })
    }

    /// Appends one line to a spool.
    ///
    /// The line is written with a single `write_all` to a file opened `O_APPEND`, so concurrent
    /// hooks interleave whole lines rather than fragments.
    ///
    /// # Errors
    ///
    /// Returns [`SpoolError`] when the line exceeds the ceiling, the target is a symlink, or the
    /// write fails.
    pub fn append(&self, kind: SpoolKind, line: &str) -> Result<(), SpoolError> {
        if line.len() > MAX_LINE_BYTES {
            return Err(SpoolError::LineTooLarge { bytes: line.len() });
        }

        let path = self.active_path(kind)?;
        let mut file = Self::open_append(&path)?;

        let mut record = String::with_capacity(line.len() + 1);
        record.push_str(line.replace('\n', "\\n").as_str());
        record.push('\n');

        file.write_all(record.as_bytes())
            .map_err(|error| SpoolError::Io {
                path: path.display().to_string(),
                reason: error.to_string(),
            })
    }

    /// Returns the path of the file a spool is currently appending to.
    ///
    /// # Errors
    ///
    /// Returns [`SpoolError`] when a spool file is a symlink or cannot be inspected.
    pub fn active_path(&self, kind: SpoolKind) -> Result<PathBuf, SpoolError> {
        let mut index = 0_u32;
        loop {
            let candidate = self.path_for(kind, index);
            if let Ok(metadata) = std::fs::symlink_metadata(&candidate) {
                if metadata.file_type().is_symlink() {
                    return Err(SpoolError::Symlink {
                        path: candidate.display().to_string(),
                    });
                }
                if metadata.len() >= ROTATE_AT_BYTES {
                    index += 1;
                    continue;
                }
            }
            return Ok(candidate);
        }
    }

    /// Reads back every line of a spool, oldest file first.
    ///
    /// # Errors
    ///
    /// Returns [`SpoolError`] when a spool file cannot be read.
    pub fn read_lines(&self, kind: SpoolKind) -> Result<Vec<String>, SpoolError> {
        let mut lines = Vec::new();
        let mut index = 0_u32;
        loop {
            let path = self.path_for(kind, index);
            if !path.exists() {
                break;
            }
            let text = std::fs::read_to_string(&path).map_err(|error| SpoolError::Io {
                path: path.display().to_string(),
                reason: error.to_string(),
            })?;
            lines.extend(text.lines().map(str::to_owned));
            index += 1;
        }
        Ok(lines)
    }

    /// Returns the directory the spool lives in.
    #[must_use]
    pub fn directory(&self) -> &Path {
        &self.directory
    }

    /// Path of the `index`-th file of a spool.
    fn path_for(&self, kind: SpoolKind, index: u32) -> PathBuf {
        if index == 0 {
            self.directory.join(format!("{}.jsonl", kind.base_name()))
        } else {
            self.directory
                .join(format!("{}.{index}.jsonl", kind.base_name()))
        }
    }

    /// Opens a spool file for appending, creating it `0600`.
    fn open_append(path: &Path) -> Result<File, SpoolError> {
        let file = OpenOptions::new()
            .create(true)
            .append(true)
            .mode(SPOOL_FILE_MODE)
            .open(path)
            .map_err(|error| SpoolError::Io {
                path: path.display().to_string(),
                reason: error.to_string(),
            })?;

        // A file that already existed keeps its mode; tighten it rather than trusting it.
        let _ = std::fs::set_permissions(path, Permissions::from_mode(SPOOL_FILE_MODE));
        Ok(file)
    }
}
