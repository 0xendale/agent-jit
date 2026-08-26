//! The `agent-jit` command line binary.
//!
//! Command dispatch stays intentionally small: every subcommand parses its own
//! arguments and returns a typed exit code so failures are never silent.

use std::io::{self, Write as _};
use std::process::ExitCode;

/// Exit code used for every usage error (unknown command, missing argument).
const EXIT_USAGE: u8 = 2;

const USAGE: &str = "usage: agent-jit <command> [options]\n\
                     \n\
                     commands:\n\
                     \x20 help      print this message\n\
                     \n\
                     options:\n\
                     \x20 --version print the binary version\n\
                     \x20 --help    print this message\n";

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
            let mut err = io::stderr().lock();
            let _ = writeln!(err, "{error}");
            let _ = err.flush();
            ExitCode::from(EXIT_USAGE)
        }
    }
}

/// Dispatches `args` and returns the text to print on success.
///
/// # Errors
///
/// Returns a human-readable usage error when the command is missing or unknown.
fn run(args: &[String]) -> Result<String, String> {
    match args.first().map(String::as_str) {
        None => Err(USAGE.to_owned()),
        Some("--version" | "-V" | "version") => {
            Ok(format!("agent-jit {}\n", env!("CARGO_PKG_VERSION")))
        }
        Some("--help" | "-h" | "help") => Ok(USAGE.to_owned()),
        Some(other) => Err(format!("unknown command: {other}\n\n{USAGE}")),
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
            run(&args),
            Ok(format!("agent-jit {}\n", env!("CARGO_PKG_VERSION")))
        );
    }

    #[test]
    fn unknown_command_is_an_error() {
        let args = vec!["nope".to_owned()];
        let error = run(&args).unwrap_err();
        assert!(error.starts_with("unknown command: nope"), "{error}");
    }

    #[test]
    fn empty_arguments_render_usage_as_error() {
        assert!(run(&[]).unwrap_err().starts_with("usage: agent-jit"));
    }
}
