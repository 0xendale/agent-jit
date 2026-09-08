//! Argument parsing shared by the runtime launcher commands.

use crate::error::CommandError;

pub(super) fn parse_flags(args: &[String], usage: &str) -> Result<bool, CommandError> {
    let mut as_json = false;
    for argument in args {
        match argument.as_str() {
            "--json" => as_json = true,
            other => {
                return Err(CommandError::usage(format!(
                    "unknown option: {other}\n{usage}"
                )));
            }
        }
    }
    Ok(as_json)
}

/// Parses `run`'s arguments, splitting agent-jit's own options from the runtime's at `--`.
pub(super) fn parse_run_arguments(
    args: &[String],
    bin_flag: &'static str,
    usage: &'static str,
) -> Result<(String, Option<String>, bool, Vec<String>), CommandError> {
    let (own, runtime_args) = match args.iter().position(|argument| argument == "--") {
        Some(index) => (&args[..index], args[index + 1..].to_vec()),
        None => (args, Vec::new()),
    };
    let mut repo = None;
    let mut runtime_bin = None;
    let mut as_json = false;
    let mut remaining = own.iter();
    while let Some(argument) = remaining.next() {
        match argument.as_str() {
            "--repo" => {
                repo = Some(
                    remaining
                        .next()
                        .ok_or_else(|| CommandError::usage(usage))?
                        .clone(),
                );
            }
            flag if flag == bin_flag => {
                runtime_bin = Some(
                    remaining
                        .next()
                        .ok_or_else(|| CommandError::usage(usage))?
                        .clone(),
                );
            }
            "--json" => as_json = true,
            other => {
                return Err(CommandError::usage(format!(
                    "unknown option: {other}\n{usage}"
                )));
            }
        }
    }
    Ok((
        repo.ok_or_else(|| CommandError::usage(usage))?,
        runtime_bin,
        as_json,
        runtime_args,
    ))
}
