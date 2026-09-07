//! Rendering rules.
//!
//! Machine output goes to stdout, diagnostics go to stderr, and neither ever carries ANSI escapes:
//! this output is read by scripts, by the benchmark harness, and by evidence files that must stay
//! diff-able.

use std::io::{self, Write as _};

use serde_json::Value;

use crate::error::CommandError;

/// What a command produced.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Rendered {
    /// Human-readable text, already newline-terminated.
    Text(String),
    /// A machine-readable document.
    Json(Value),
}

impl Rendered {
    /// Renders the value as the bytes to write to stdout.
    ///
    /// # Errors
    ///
    /// Returns a [`CommandError`] when a JSON document cannot be serialized.
    pub fn to_stdout_bytes(&self) -> Result<String, CommandError> {
        match self {
            Self::Text(text) => Ok(text.clone()),
            Self::Json(value) => {
                let mut rendered = serde_json::to_string_pretty(value).map_err(|error| {
                    CommandError::new(
                        "output_unrenderable",
                        error.to_string(),
                        crate::error::ExitClass::Internal,
                    )
                })?;
                rendered.push('\n');
                Ok(rendered)
            }
        }
    }
}

/// Writes successful output to stdout.
pub fn emit(rendered: &Rendered) -> Result<(), CommandError> {
    let bytes = rendered.to_stdout_bytes()?;
    let mut out = io::stdout().lock();
    let _ = out.write_all(bytes.as_bytes());
    let _ = out.flush();
    Ok(())
}

/// Writes a failure, honouring JSON mode.
///
/// In JSON mode the stable error object goes to stdout so a caller parsing stdout always finds a
/// document; the human diagnostic still goes to stderr. Outside JSON mode stdout stays empty,
/// because a partial record is worse than none.
pub fn emit_error(error: &CommandError, json_mode: bool) {
    if json_mode && let Ok(document) = serde_json::to_string_pretty(&error.to_json()) {
        let mut out = io::stdout().lock();
        let _ = writeln!(out, "{document}");
        let _ = out.flush();
    }

    let mut err = io::stderr().lock();
    let _ = writeln!(err, "{error}");
    let _ = err.flush();
}
