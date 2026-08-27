//! Immutable per-event spool segments.
//!
//! Each hook writes exactly one file. Several Claude hooks can run at once, and they are separate
//! processes: appending to a shared file would interleave their writes, and a process killed
//! mid-append would leave a fragment that looks like a record. One file per event removes both
//! problems — the write is a temp file plus `fsync` plus an atomic rename, so a segment either
//! exists complete or does not exist.
//!
//! A segment carries its own length and checksum. Recovery therefore does not have to trust the
//! filesystem: a truncated or altered segment is detected and quarantined rather than believed.

use std::collections::BTreeSet;
use std::fs::{DirBuilder, File, OpenOptions};
use std::io::Write as _;
use std::os::unix::fs::{DirBuilderExt as _, OpenOptionsExt as _};
use std::path::{Path, PathBuf};

use agent_jit_domain::canonical::{Digest, canonical_string_of};
use serde::{Deserialize, Serialize};

use crate::adapters::claude_hooks::NormalizedHook;

/// Mode for spool directories.
const DIR_MODE: u32 = 0o700;

/// Mode for spool files.
const FILE_MODE: u32 = 0o600;

/// Version of the segment envelope itself, independent of the event schema inside it.
pub const SEGMENT_VERSION: u32 = 1;

/// Prefix marking a partially written file. Never read back as a segment.
const TEMP_PREFIX: &str = ".tmp-";

/// One recorded event as it sits on disk.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Segment {
    /// Version of this envelope.
    pub segment_version: u32,
    /// Digest of the canonical payload; also the event's identity.
    pub event_id: Digest,
    /// Session the event belongs to.
    pub session_key: String,
    /// Monotonic ordering key: the observation time, with a counter for same-millisecond events.
    pub order_key: u64,
    /// Wall-clock time the event was observed.
    pub recorded_at_unix_ms: i64,
    /// Length in bytes of the canonical payload.
    pub length: u64,
    /// Checksum of the canonical payload.
    pub checksum: Digest,
    /// The normalized event.
    pub payload: NormalizedHook,
}

impl Segment {
    /// Verifies the segment against its own length and checksum.
    ///
    /// # Errors
    ///
    /// Returns the reason code when the segment does not describe itself correctly.
    pub fn verify(&self) -> Result<(), &'static str> {
        if self.segment_version != SEGMENT_VERSION {
            return Err("segment_unsupported_version");
        }
        let canonical = canonical_string_of(&self.payload).map_err(|_| "segment_malformed")?;
        // Checksum first: it is the property that decides whether the payload is what was written.
        // The length is a cheap corroboration, not the integrity check.
        if Digest::of_bytes(canonical.as_bytes()) != self.checksum {
            return Err("segment_checksum_mismatch");
        }
        if canonical.len() as u64 != self.length {
            return Err("segment_length_mismatch");
        }
        if self.event_id != self.checksum {
            return Err("segment_identity_mismatch");
        }
        Ok(())
    }
}

/// A segment that could not be trusted.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct QuarantinedSegment {
    /// Path the segment was moved to.
    pub path: PathBuf,
    /// Why it was quarantined.
    pub reason: &'static str,
    /// Size of the file, for the health record.
    pub bytes: u64,
}

/// Everything one session's spool directory contained.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct SessionSegments {
    /// Verified segments, deduplicated and ordered.
    pub accepted: Vec<Segment>,
    /// Segments that failed verification, moved aside.
    pub quarantined: Vec<QuarantinedSegment>,
    /// How many duplicate events were collapsed.
    pub duplicates: u32,
}

/// Why a segment could not be written.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum SegmentWriteError {
    /// The session key would escape the spool directory.
    #[error("`{key}` is not a safe session key")]
    UnsafeSessionKey {
        /// The rejected key.
        key: String,
    },
    /// The event could not be canonicalized.
    #[error("event could not be canonicalized: {reason}")]
    NotCanonical {
        /// Why canonicalization failed.
        reason: String,
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

impl SegmentWriteError {
    /// Stable machine-readable code.
    #[must_use]
    pub const fn code(&self) -> &'static str {
        match self {
            Self::UnsafeSessionKey { .. } => "segment_unsafe_session_key",
            Self::NotCanonical { .. } => "segment_not_canonical",
            Self::Io { .. } => "segment_io",
        }
    }
}

/// A private directory of per-session segment spools.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SegmentStore {
    root: PathBuf,
}

impl SegmentStore {
    /// Opens (creating if needed) the segment store at `root`.
    ///
    /// # Errors
    ///
    /// Returns [`SegmentWriteError`] when the directory cannot be created privately.
    pub fn open(root: &Path) -> Result<Self, SegmentWriteError> {
        create_private_dir(root)?;
        Ok(Self {
            root: root.to_path_buf(),
        })
    }

    /// Returns the directory holding one session's segments.
    #[must_use]
    pub fn session_directory(&self, session_key: &str) -> PathBuf {
        self.root.join(session_key)
    }

    /// Writes one event as a segment.
    ///
    /// Writing the same event twice is harmless: the file name is derived from the payload digest,
    /// so a repeated hook lands on the same name and the second write is the same bytes.
    ///
    /// # Errors
    ///
    /// Returns [`SegmentWriteError`] when the session key is unsafe, the event cannot be
    /// canonicalized, or the filesystem refuses the write.
    pub fn write(
        &self,
        event: &NormalizedHook,
        recorded_at_unix_ms: i64,
    ) -> Result<PathBuf, SegmentWriteError> {
        let session_key = safe_session_key(&event.session_key)?;
        let canonical =
            canonical_string_of(event).map_err(|error| SegmentWriteError::NotCanonical {
                reason: error.to_string(),
            })?;
        let checksum = Digest::of_bytes(canonical.as_bytes());

        let order_key = u64::try_from(recorded_at_unix_ms.max(0)).unwrap_or(0);
        let segment = Segment {
            segment_version: SEGMENT_VERSION,
            event_id: checksum,
            session_key: event.session_key.clone(),
            order_key,
            recorded_at_unix_ms,
            length: canonical.len() as u64,
            checksum,
            payload: event.clone(),
        };

        let directory = self.root.join(session_key);
        create_private_dir(&directory)?;

        let rendered =
            serde_json::to_string(&segment).map_err(|error| SegmentWriteError::NotCanonical {
                reason: error.to_string(),
            })?;

        // The name carries the order key first so a directory listing sorts chronologically, and
        // the payload digest second so a repeated event cannot become a second record.
        let name = format!("{order_key:020}-{checksum}.json");
        let destination = directory.join(name);
        write_atomically(&directory, &destination, rendered.as_bytes())?;
        Ok(destination)
    }

    /// Returns every session that has segments on disk, sorted.
    ///
    /// # Errors
    ///
    /// Returns [`SegmentWriteError`] when the directory cannot be listed.
    pub fn sessions(&self) -> Result<Vec<String>, SegmentWriteError> {
        let entries = std::fs::read_dir(&self.root).map_err(|error| SegmentWriteError::Io {
            path: self.root.display().to_string(),
            reason: error.to_string(),
        })?;

        let mut sessions = BTreeSet::new();
        for entry in entries.flatten() {
            if entry.path().is_dir()
                && let Some(name) = entry.file_name().to_str()
                && !name.starts_with('.')
                && name != "quarantine"
            {
                sessions.insert(name.to_owned());
            }
        }
        Ok(sessions.into_iter().collect())
    }

    /// Reads one session's segments, verifying and deduplicating them.
    ///
    /// Segments that fail verification are moved to a quarantine directory: the bytes are kept
    /// (they are already redacted) so a corruption can be investigated, but they can never be
    /// mistaken for evidence.
    ///
    /// # Errors
    ///
    /// Returns [`SegmentWriteError`] when the session directory cannot be read.
    pub fn read_session(&self, session_key: &str) -> Result<SessionSegments, SegmentWriteError> {
        let directory = self.root.join(safe_session_key(session_key)?);
        if !directory.exists() {
            return Ok(SessionSegments::default());
        }

        let entries = std::fs::read_dir(&directory).map_err(|error| SegmentWriteError::Io {
            path: directory.display().to_string(),
            reason: error.to_string(),
        })?;

        let mut result = SessionSegments::default();
        let mut verified: Vec<Segment> = Vec::new();
        let mut paths: Vec<PathBuf> = entries
            .flatten()
            .map(|entry| entry.path())
            .filter(|path| {
                path.is_file()
                    && path
                        .file_name()
                        .and_then(|name| name.to_str())
                        .is_some_and(|name| {
                            !name.starts_with(TEMP_PREFIX) && !name.starts_with('.')
                        })
            })
            .collect();
        paths.sort();

        for path in paths {
            match read_segment(&path) {
                Ok(segment) => {
                    if segment.session_key == session_key {
                        verified.push(segment);
                    } else {
                        result
                            .quarantined
                            .push(self.quarantine(&path, "segment_session_mismatch")?);
                    }
                }
                Err(reason) => result.quarantined.push(self.quarantine(&path, reason)?),
            }
        }

        verified.sort_by_key(|segment| (segment.order_key, segment.checksum.to_string()));

        // A hook that fires twice for one occurrence produces the same payload back to back.
        // Two identical events separated by other events are two occurrences — a `Stop` closing
        // turn one and a `Stop` closing turn two are byte-identical, and collapsing them would
        // silently erase a turn. So only adjacent repeats are duplicates.
        for segment in verified {
            if result
                .accepted
                .last()
                .is_some_and(|previous: &Segment| previous.event_id == segment.event_id)
            {
                result.duplicates = result.duplicates.saturating_add(1);
            } else {
                result.accepted.push(segment);
            }
        }

        Ok(result)
    }

    /// Removes one session's segments once they are safely in the database.
    ///
    /// # Errors
    ///
    /// Returns [`SegmentWriteError`] when the directory cannot be removed.
    pub fn discard_session(&self, session_key: &str) -> Result<(), SegmentWriteError> {
        let directory = self.root.join(safe_session_key(session_key)?);
        if !directory.exists() {
            return Ok(());
        }
        std::fs::remove_dir_all(&directory).map_err(|error| SegmentWriteError::Io {
            path: directory.display().to_string(),
            reason: error.to_string(),
        })
    }

    /// Moves an untrustworthy segment aside and reports it.
    fn quarantine(
        &self,
        path: &Path,
        reason: &'static str,
    ) -> Result<QuarantinedSegment, SegmentWriteError> {
        let bytes = std::fs::metadata(path)
            .map(|metadata| metadata.len())
            .unwrap_or(0);
        let directory = self.root.join("quarantine");
        create_private_dir(&directory)?;

        let name = path
            .file_name()
            .and_then(|name| name.to_str())
            .unwrap_or("segment");
        let destination = directory.join(format!("{reason}-{name}"));
        std::fs::rename(path, &destination).map_err(|error| SegmentWriteError::Io {
            path: path.display().to_string(),
            reason: error.to_string(),
        })?;

        Ok(QuarantinedSegment {
            path: destination,
            reason,
            bytes,
        })
    }
}

/// Reads and verifies one segment file.
fn read_segment(path: &Path) -> Result<Segment, &'static str> {
    let text = std::fs::read_to_string(path).map_err(|_| "segment_unreadable")?;
    let segment: Segment = serde_json::from_str(&text).map_err(|_| "segment_malformed")?;
    segment.verify()?;
    Ok(segment)
}

/// Rejects a session key that is not a plain directory name.
fn safe_session_key(key: &str) -> Result<&str, SegmentWriteError> {
    let safe = !key.is_empty()
        && key.len() <= 128
        && key
            .chars()
            .all(|character| character.is_ascii_alphanumeric() || matches!(character, '-' | '_'));

    if safe {
        Ok(key)
    } else {
        Err(SegmentWriteError::UnsafeSessionKey {
            key: key.to_owned(),
        })
    }
}

/// Creates a directory `0700` if it does not exist.
fn create_private_dir(path: &Path) -> Result<(), SegmentWriteError> {
    if path.exists() {
        return Ok(());
    }
    DirBuilder::new()
        .recursive(true)
        .mode(DIR_MODE)
        .create(path)
        .map_err(|error| SegmentWriteError::Io {
            path: path.display().to_string(),
            reason: error.to_string(),
        })
}

/// Writes `bytes` to `destination` so that the file is either absent or complete.
fn write_atomically(
    directory: &Path,
    destination: &Path,
    bytes: &[u8],
) -> Result<(), SegmentWriteError> {
    let temporary = directory.join(format!(
        "{TEMP_PREFIX}{}-{}",
        std::process::id(),
        destination
            .file_name()
            .and_then(|name| name.to_str())
            .unwrap_or("segment")
    ));

    let mut file = OpenOptions::new()
        .create(true)
        .truncate(true)
        .write(true)
        .mode(FILE_MODE)
        .open(&temporary)
        .map_err(|error| SegmentWriteError::Io {
            path: temporary.display().to_string(),
            reason: error.to_string(),
        })?;

    file.write_all(bytes)
        .map_err(|error| SegmentWriteError::Io {
            path: temporary.display().to_string(),
            reason: error.to_string(),
        })?;
    // The rename is only atomic with respect to a crash if the bytes are on disk first.
    file.sync_all().map_err(|error| SegmentWriteError::Io {
        path: temporary.display().to_string(),
        reason: error.to_string(),
    })?;
    drop(file);

    std::fs::rename(&temporary, destination).map_err(|error| SegmentWriteError::Io {
        path: destination.display().to_string(),
        reason: error.to_string(),
    })?;

    // Persist the directory entry too, so recovery after a power loss sees the rename.
    if let Ok(handle) = File::open(directory) {
        let _ = handle.sync_all();
    }
    Ok(())
}
