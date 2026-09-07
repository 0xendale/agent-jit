//! Argument parsing for the Claude integration commands.

use crate::claude::USAGE;
use crate::error::CommandError;

pub(super) fn parse_flags(args: &[String]) -> Result<bool, CommandError> {
    let mut as_json = false;
    for argument in args {
        match argument.as_str() {
            "--json" => as_json = true,
            other => {
                return Err(CommandError::usage(format!(
                    "unknown option: {other}\n{USAGE}"
                )));
            }
        }
    }
    Ok(as_json)
}

pub(super) fn parse_run_arguments(
    args: &[String],
) -> Result<(String, Option<String>, bool, Vec<String>), CommandError> {
    let (own, claude_args) = match args.iter().position(|argument| argument == "--") {
        Some(index) => (&args[..index], args[index + 1..].to_vec()),
        None => (args, Vec::new()),
    };
    let mut repo = None;
    let mut claude_bin = None;
    let mut as_json = false;
    let mut remaining = own.iter();
    while let Some(argument) = remaining.next() {
        match argument.as_str() {
            "--repo" => {
                repo = Some(
                    remaining
                        .next()
                        .ok_or_else(|| CommandError::usage(USAGE))?
                        .clone(),
                );
            }
            "--claude-bin" => {
                claude_bin = Some(
                    remaining
                        .next()
                        .ok_or_else(|| CommandError::usage(USAGE))?
                        .clone(),
                );
            }
            "--json" => as_json = true,
            other => {
                return Err(CommandError::usage(format!(
                    "unknown option: {other}\n{USAGE}"
                )));
            }
        }
    }
    Ok((
        repo.ok_or_else(|| CommandError::usage(USAGE))?,
        claude_bin,
        as_json,
        claude_args,
    ))
}
