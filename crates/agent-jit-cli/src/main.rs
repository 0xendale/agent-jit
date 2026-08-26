//! The `agent-jit` command line binary.
//!
//! The command tree is fixed up front: every command the product will ever expose is reserved
//! here, and the ones this build does not implement say `not_implemented` rather than returning a
//! plausible-looking success. Dispatch returns typed errors so no failure is silent.

use std::process::ExitCode;

mod app;
mod doctor;
mod error;
mod hook;
mod output;
mod repo;
mod schema;
mod store;

use error::{CommandError, ExitClass};
use output::Rendered;

/// Every command the binary reserves, with its one-line help.
const COMMANDS: &[(&str, &str)] = &[
    ("paths", "report the private paths agent-jit uses"),
    ("doctor", "check host support, paths, and store health"),
    ("schema", "generate or validate versioned record contracts"),
    ("store", "migrate and check the private state database"),
    ("repo", "inspect the identity of a repository"),
    ("hook", "ingest one Claude Code hook event"),
    (
        "trace",
        "inspect, annotate, and export recorded trajectories",
    ),
    ("corpus", "report qualifying-trajectory accrual"),
    ("group", "group trajectories that solved one intent"),
    ("candidate", "inspect and confirm candidate contracts"),
    ("phase0", "run the thesis gate and emit its report"),
    ("profile", "inspect the frozen repository command profile"),
    ("capability", "inspect, approve, and enable capabilities"),
    ("replay", "replay a capability against historical fixtures"),
    ("sandbox", "verify the pinned sandbox sidecar"),
    ("mcp", "serve the single jit.query MCP tool over stdio"),
    ("claude", "launch Claude Code with the recorder plugin"),
    ("benchmark", "run and report the paired benchmark"),
    ("install", "materialize the local Claude Code plugin"),
    (
        "uninstall",
        "remove the local plugin and, optionally, state",
    ),
];

fn main() -> ExitCode {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let json_mode = args.iter().any(|argument| argument == "--json");

    match run(&args) {
        Ok(rendered) => match output::emit(&rendered) {
            Ok(()) => ExitCode::SUCCESS,
            Err(error) => {
                output::emit_error(&error, json_mode);
                ExitCode::from(error.class.code())
            }
        },
        Err(error) => {
            output::emit_error(&error, json_mode);
            ExitCode::from(error.class.code())
        }
    }
}

/// Dispatches `args`.
///
/// # Errors
///
/// Returns a [`CommandError`] when the command is unknown, misused, unimplemented, or refuses.
fn run(args: &[String]) -> Result<Rendered, CommandError> {
    let Some(command) = args.first().map(String::as_str) else {
        return Err(CommandError::usage(usage()));
    };

    match command {
        "--version" | "-V" | "version" => Ok(Rendered::Text(format!(
            "agent-jit {}\n",
            env!("CARGO_PKG_VERSION")
        ))),
        "--help" | "-h" | "help" => Ok(Rendered::Text(usage())),
        "paths" => paths(&args[1..]),
        "doctor" => doctor::run(&args[1..]),
        "schema" => schema::run(&args[1..]),
        "hook" => hook::run(&args[1..]),
        "repo" => repo::run(&args[1..]),
        "store" => store::run(&args[1..]),
        reserved if COMMANDS.iter().any(|(name, _)| *name == reserved) => {
            Err(CommandError::not_implemented(reserved))
        }
        other => Err(CommandError::new(
            "unknown_command",
            format!("unknown command: {other}\n\n{}", usage()),
            ExitClass::Usage,
        )),
    }
}

/// Reports the private paths, creating them if needed.
fn paths(args: &[String]) -> Result<Rendered, CommandError> {
    let mut as_json = false;
    for argument in args {
        match argument.as_str() {
            "--json" => as_json = true,
            other => {
                return Err(CommandError::usage(format!(
                    "unknown option: {other}\nusage: agent-jit paths [--json]"
                )));
            }
        }
    }

    let paths = app::AppPaths::resolve()?;
    paths.ensure()?;

    if as_json {
        return Ok(Rendered::Json(paths.to_json()));
    }
    Ok(Rendered::Text(format!(
        "home      {}\n\
         data      {}\n\
         cache     {}\n\
         state_db  {}\n\
         spool     {}\n\
         logs      {}\n",
        paths.home.display(),
        paths.data.display(),
        paths.cache.display(),
        paths.state_db.display(),
        paths.spool.display(),
        paths.logs.display(),
    )))
}

/// Renders the top-level usage text.
fn usage() -> String {
    use std::fmt::Write as _;

    let mut text = String::from("usage: agent-jit <command> [options]\n\ncommands:\n");
    for (name, description) in COMMANDS {
        let _ = writeln!(text, "  {name:<11}{description}");
    }
    text.push_str("\noptions:\n  --json     emit machine-readable output on stdout\n");
    text.push_str("  --version  print the binary version\n");
    text.push_str("  --help     print this message\n");
    text
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
mod tests {
    use super::{COMMANDS, ExitClass, Rendered, run};

    #[test]
    fn version_is_rendered_with_binary_name() {
        let rendered = run(&["--version".to_owned()]).unwrap();
        assert_eq!(
            rendered,
            Rendered::Text(format!("agent-jit {}\n", env!("CARGO_PKG_VERSION")))
        );
    }

    #[test]
    fn unknown_command_uses_the_usage_class() {
        let error = run(&["nope".to_owned()]).unwrap_err();
        assert_eq!(error.code, "unknown_command");
        assert_eq!(error.class, ExitClass::Usage);
    }

    #[test]
    fn empty_arguments_render_usage_as_error() {
        let error = run(&[]).unwrap_err();
        assert_eq!(error.class, ExitClass::Usage);
        assert!(error.message.starts_with("usage: agent-jit"));
    }

    #[test]
    fn reserved_but_unimplemented_commands_say_so() {
        let error = run(&["phase0".to_owned()]).unwrap_err();
        assert_eq!(error.code, "not_implemented");
        assert_eq!(error.class, ExitClass::Unsupported);
    }

    #[test]
    fn the_command_list_has_no_duplicates() {
        let mut names: Vec<&str> = COMMANDS.iter().map(|(name, _)| *name).collect();
        names.sort_unstable();
        let count = names.len();
        names.dedup();
        assert_eq!(names.len(), count);
    }
}
