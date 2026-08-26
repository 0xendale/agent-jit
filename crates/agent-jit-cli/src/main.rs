//! The `agent-jit` command line binary.
//!
//! Command dispatch stays intentionally small: every subcommand parses its own arguments and
//! returns a typed error carrying a stable code, so a failure is never silent and never partial.

use std::io::{self, Write as _};
use std::process::ExitCode;

mod repo;
mod schema;

/// Exit code for a usage error (unknown command, missing argument).
const EXIT_USAGE: u8 = 2;
/// Exit code for a command that ran and refused its input.
const EXIT_FAILURE: u8 = 1;

const USAGE: &str = "usage: agent-jit <command> [options]\n\
                     \n\
                     commands:\n\
                     \x20 repo inspect --root <path>    report the identity of a repository\n\
                     \x20 schema generate --out <dir>   write the JSON Schemas for every contract\n\
                     \x20 schema validate <file>        validate one stored record document\n\
                     \x20 help                          print this message\n\
                     \n\
                     options:\n\
                     \x20 --version                     print the binary version\n\
                     \x20 --help                        print this message\n";

/// A command failure with a stable machine-readable code.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CommandError {
    /// Stable code, e.g. `schema_unsupported`.
    pub code: &'static str,
    /// Human-readable detail.
    pub message: String,
    /// Process exit code.
    pub exit: u8,
}

impl CommandError {
    /// A usage error: the command was not invoked correctly.
    pub fn usage(message: impl Into<String>) -> Self {
        Self {
            code: "usage",
            message: message.into(),
            exit: EXIT_USAGE,
        }
    }

    /// A refusal: the command ran and rejected its input.
    pub fn refused(code: &'static str, message: impl Into<String>) -> Self {
        Self {
            code,
            message: message.into(),
            exit: EXIT_FAILURE,
        }
    }
}

fn main() -> ExitCode {
    let args: Vec<String> = std::env::args().skip(1).collect();
    match run(&args) {
        Ok(rendered) => {
            let mut out = io::stdout().lock();
            let _ = out.write_all(rendered.as_bytes());
            let _ = out.flush();
            ExitCode::SUCCESS
        }
        Err(error) => {
            // Nothing is written to stdout on failure: a partial record is worse than none.
            let mut err = io::stderr().lock();
            if error.code == "usage" {
                let _ = writeln!(err, "{}", error.message);
            } else {
                let _ = writeln!(err, "error: {}: {}", error.code, error.message);
            }
            let _ = err.flush();
            ExitCode::from(error.exit)
        }
    }
}

/// Dispatches `args` and returns the text to print on success.
///
/// # Errors
///
/// Returns a [`CommandError`] when the command is unknown, misused, or refuses its input.
fn run(args: &[String]) -> Result<String, CommandError> {
    match args.first().map(String::as_str) {
        None => Err(CommandError::usage(USAGE)),
        Some("--version" | "-V" | "version") => {
            Ok(format!("agent-jit {}\n", env!("CARGO_PKG_VERSION")))
        }
        Some("--help" | "-h" | "help") => Ok(USAGE.to_owned()),
        Some("repo") => repo::run(&args[1..]),
        Some("schema") => schema::run(&args[1..]),
        Some(other) => Err(CommandError::usage(format!(
            "unknown command: {other}\n\n{USAGE}"
        ))),
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
mod tests {
    use super::run;

    #[test]
    fn version_is_rendered_with_binary_name() {
        let args = vec!["--version".to_owned()];
        assert_eq!(
            run(&args).unwrap(),
            format!("agent-jit {}\n", env!("CARGO_PKG_VERSION"))
        );
    }

    #[test]
    fn unknown_command_is_a_usage_error() {
        let error = run(&["nope".to_owned()]).unwrap_err();
        assert_eq!(error.code, "usage");
        assert!(
            error.message.starts_with("unknown command: nope"),
            "{error:?}"
        );
        assert_eq!(error.exit, 2);
    }

    #[test]
    fn empty_arguments_render_usage_as_error() {
        let error = run(&[]).unwrap_err();
        assert_eq!(error.code, "usage");
        assert!(error.message.starts_with("usage: agent-jit"));
    }
}
