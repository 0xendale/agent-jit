//! `agent-jit doctor` — report what this build supports and where its state lives.

use agent_jit_engine::host::HostSupport;
use serde_json::json;

use crate::app::AppPaths;
use crate::error::CommandError;
use crate::output::Rendered;

/// Runs the doctor command.
///
/// # Errors
///
/// Returns a [`CommandError`] for unknown options or unusable private paths.
pub fn run(args: &[String]) -> Result<Rendered, CommandError> {
    let mut as_json = false;
    for argument in args {
        match argument.as_str() {
            "--json" => as_json = true,
            other => {
                return Err(CommandError::usage(format!(
                    "unknown option: {other}\nusage: agent-jit doctor [--json]"
                )));
            }
        }
    }

    // An unsupported host is reported, not thrown: `doctor` exists to explain the situation.
    let host = HostSupport::current();
    let paths = AppPaths::resolve()?;
    paths.ensure()?;

    let (os, arch, supported) = match host {
        Ok(support) => (support.os.to_owned(), support.arch.to_owned(), true),
        Err(_) => (
            std::env::consts::OS.to_owned(),
            std::env::consts::ARCH.to_owned(),
            false,
        ),
    };

    let report = json!({
        "version": env!("CARGO_PKG_VERSION"),
        "host": {"os": os, "arch": arch, "supported": supported},
        "paths": paths.to_json(),
        "store": {"present": paths.state_db.exists()},
    });

    if as_json {
        return Ok(Rendered::Json(report));
    }

    Ok(Rendered::Text(format!(
        "agent-jit  {}\n\
         host       {os}/{arch} ({})\n\
         home       {}\n\
         state_db   {} ({})\n",
        env!("CARGO_PKG_VERSION"),
        if supported {
            "supported"
        } else {
            "UNSUPPORTED"
        },
        paths.home.display(),
        paths.state_db.display(),
        if paths.state_db.exists() {
            "present"
        } else {
            "absent"
        },
    )))
}
