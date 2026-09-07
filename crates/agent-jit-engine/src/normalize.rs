//! The boundary between raw captured bytes and anything the product will keep.
//!
//! Two rules meet here. First, input is bounded *while being read*: a hook or an import can hand
//! us any number of bytes, and the recorder must not let that decide how much memory it uses. What
//! exceeds the limit is counted, not kept. Second, nothing crosses into storage as plain text —
//! text becomes [`Redacted`] here or not at all.

use std::io::Read;

use agent_jit_domain::canonical::Digest;
use agent_jit_domain::redaction::{Redacted, Redactor};

/// Size of the read buffer used while counting bytes past the limit.
const DRAIN_CHUNK: usize = 8192;

/// Why normalization failed.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum NormalizeError {
    /// The caller asked for a zero-byte limit, which would discard everything silently.
    #[error("a bounded read needs a positive limit")]
    LimitInvalid,
    /// The input could not be read.
    #[error("input could not be read: {reason}")]
    Unreadable {
        /// Reason reported by the operating system.
        reason: String,
    },
    /// The retained bytes were not valid UTF-8.
    #[error("input is not valid UTF-8 at byte {offset}")]
    InvalidUtf8 {
        /// Byte offset of the first invalid sequence.
        offset: usize,
    },
}

impl NormalizeError {
    /// Stable machine-readable code.
    #[must_use]
    pub const fn code(&self) -> &'static str {
        match self {
            Self::LimitInvalid => "normalize_limit_invalid",
            Self::Unreadable { .. } => "normalize_unreadable",
            Self::InvalidUtf8 { .. } => "normalize_invalid_utf8",
        }
    }
}

/// Bytes that were read under a limit, plus what was observed beyond it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BoundedInput {
    bytes: Vec<u8>,
    observed_bytes: u64,
    truncated: bool,
}

impl BoundedInput {
    /// The retained bytes.
    #[must_use]
    pub fn bytes(&self) -> &Vec<u8> {
        &self.bytes
    }

    /// How many bytes the source produced in total, including those not kept.
    #[must_use]
    pub const fn observed_bytes(&self) -> u64 {
        self.observed_bytes
    }

    /// Whether the source produced more than the limit allowed.
    #[must_use]
    pub const fn truncated(&self) -> bool {
        self.truncated
    }

    /// Digest of the retained bytes.
    #[must_use]
    pub fn digest(&self) -> Digest {
        Digest::of_bytes(&self.bytes)
    }

    /// Interprets the retained bytes as UTF-8.
    ///
    /// # Errors
    ///
    /// Returns [`NormalizeError::InvalidUtf8`] rather than substituting replacement characters:
    /// a payload that is not what it claimed to be is a fact worth recording, not one to paper
    /// over. Truncation at the limit can split a character, so callers that bound text should
    /// prefer [`Self::redact_text`], which trims to a character boundary first.
    pub fn as_text(&self) -> Result<&str, NormalizeError> {
        std::str::from_utf8(&self.bytes).map_err(|error| NormalizeError::InvalidUtf8 {
            offset: error.valid_up_to(),
        })
    }

    /// Redacts the retained bytes as text.
    ///
    /// # Errors
    ///
    /// Returns [`NormalizeError::InvalidUtf8`] when the retained bytes are not UTF-8 up to the
    /// last complete character.
    pub fn redact_text(&self, redactor: &Redactor) -> Result<Redacted<String>, NormalizeError> {
        // A cut at the byte limit can land inside a character; keep the valid prefix.
        let text = match std::str::from_utf8(&self.bytes) {
            Ok(text) => text,
            Err(error) if self.truncated && error.valid_up_to() > 0 => {
                std::str::from_utf8(&self.bytes[..error.valid_up_to()]).map_err(|inner| {
                    NormalizeError::InvalidUtf8 {
                        offset: inner.valid_up_to(),
                    }
                })?
            }
            Err(error) => {
                return Err(NormalizeError::InvalidUtf8 {
                    offset: error.valid_up_to(),
                });
            }
        };

        let redacted = redactor.field(text);
        if self.truncated {
            // The cap that applied was the read limit rather than the field cap; say so honestly.
            return Ok(redacted.mark_truncated(self.observed_bytes));
        }
        Ok(redacted)
    }
}

/// Reads at most `limit` bytes from `source`, counting — but not keeping — the rest.
///
/// # Errors
///
/// Returns [`NormalizeError`] when `limit` is zero or the source cannot be read.
pub fn read_bounded(source: &mut impl Read, limit: usize) -> Result<BoundedInput, NormalizeError> {
    if limit == 0 {
        return Err(NormalizeError::LimitInvalid);
    }

    let mut bytes = Vec::with_capacity(limit);
    let mut taken = source.take(limit as u64);
    taken
        .read_to_end(&mut bytes)
        .map_err(|error| NormalizeError::Unreadable {
            reason: error.to_string(),
        })?;

    // Count whatever remains without retaining it: memory stays bounded by `limit`.
    let mut observed_bytes = bytes.len() as u64;
    let mut truncated = false;
    let mut drain = [0_u8; DRAIN_CHUNK];
    loop {
        let read = source
            .read(&mut drain)
            .map_err(|error| NormalizeError::Unreadable {
                reason: error.to_string(),
            })?;
        if read == 0 {
            break;
        }
        truncated = true;
        observed_bytes = observed_bytes.saturating_add(read as u64);
    }

    Ok(BoundedInput {
        bytes,
        observed_bytes,
        truncated,
    })
}
