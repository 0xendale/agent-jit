//! The process boundary.
//!
//! Everything the product runs goes through here, and it always runs as an absolute executable
//! plus an argument vector. There is no shell anywhere in the product: a shell would turn a
//! recorded argument into syntax, which is exactly the class of bug a compiled capability must not
//! be able to have. The child environment is stripped to a deterministic allowlist so a run cannot
//! inherit ambient credentials and cannot depend on the operator's locale.

use std::collections::BTreeMap;
use std::path::Path;
use std::process::Command;

/// Environment handed to every child process. Nothing else is inherited.
fn deterministic_environment() -> BTreeMap<&'static str, &'static str> {
    BTreeMap::from([
        // Pinned so text ordering, number formatting, and timestamps cannot vary by host.
        ("LC_ALL", "C"),
        ("LANG", "C"),
        ("TZ", "UTC"),
        // A minimal, fixed PATH: absolute executables are required anyway, but child processes
        // (git, in particular) look up helpers of their own.
        ("PATH", "/usr/bin:/bin:/usr/sbin:/sbin"),
        // Git must never open an editor, a pager, or a credential prompt.
        ("GIT_TERMINAL_PROMPT", "0"),
        ("GIT_PAGER", "cat"),
        ("GIT_OPTIONAL_LOCKS", "0"),
    ])
}

/// What a finished child process produced.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProcessOutput {
    /// Exit code, or `None` when the process was terminated by a signal.
    pub exit_code: Option<i32>,
    /// Captured standard output, lossily decoded as UTF-8.
    pub stdout: String,
    /// Captured standard error, lossily decoded as UTF-8.
    pub stderr: String,
}

impl ProcessOutput {
    /// Whether the process exited with status zero.
    #[must_use]
    pub fn success(&self) -> bool {
        self.exit_code == Some(0)
    }
}

/// Why a process could not be run.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum ProcessError {
    /// The executable was not an absolute path.
    #[error("`{executable}` is not an absolute path; PATH lookup may not decide what runs")]
    ExecutableNotAbsolute {
        /// The rejected executable.
        executable: String,
    },
    /// The process could not be spawned.
    #[error("could not spawn `{executable}`: {reason}")]
    SpawnFailed {
        /// The executable that failed to start.
        executable: String,
        /// Reason reported by the operating system.
        reason: String,
    },
}

impl ProcessError {
    /// Stable machine-readable code.
    #[must_use]
    pub const fn code(&self) -> &'static str {
        match self {
            Self::ExecutableNotAbsolute { .. } => "process_executable_not_absolute",
            Self::SpawnFailed { .. } => "process_spawn_failed",
        }
    }
}

/// Runs child processes.
///
/// Behind a trait so tests can drive the pipeline without real processes, while the real
/// implementation stays the only thing that ever touches [`Command`].
pub trait Runner {
    /// Runs `executable` with `args`, optionally in `working_directory`.
    ///
    /// # Errors
    ///
    /// Returns [`ProcessError`] when the executable is not absolute or cannot be spawned.
    fn run(
        &self,
        executable: &str,
        args: &[&str],
        working_directory: Option<&Path>,
    ) -> Result<ProcessOutput, ProcessError>;
}

/// The real process runner.
#[derive(Debug, Clone, Copy, Default)]
pub struct ProcessRunner;

impl ProcessRunner {
    /// Builds a runner.
    #[must_use]
    pub const fn new() -> Self {
        Self
    }
}

impl Runner for ProcessRunner {
    fn run(
        &self,
        executable: &str,
        args: &[&str],
        working_directory: Option<&Path>,
    ) -> Result<ProcessOutput, ProcessError> {
        if !Path::new(executable).is_absolute() {
            return Err(ProcessError::ExecutableNotAbsolute {
                executable: executable.to_owned(),
            });
        }

        let mut command = Command::new(executable);
        command.args(args).env_clear();
        for (name, value) in deterministic_environment() {
            command.env(name, value);
        }
        if let Some(directory) = working_directory {
            command.current_dir(directory);
        }

        let output = command
            .output()
            .map_err(|error| ProcessError::SpawnFailed {
                executable: executable.to_owned(),
                reason: error.to_string(),
            })?;

        Ok(ProcessOutput {
            exit_code: output.status.code(),
            stdout: String::from_utf8_lossy(&output.stdout).into_owned(),
            stderr: String::from_utf8_lossy(&output.stderr).into_owned(),
        })
    }
}
