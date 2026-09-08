use std::io::{self, Write as _};
use std::process::Command;

pub fn installed_opencode() -> Option<String> {
    let output = Command::new("opencode").arg("--version").output().ok()?;
    if !output.status.success() {
        return None;
    }
    let text = std::str::from_utf8(&output.stdout).ok()?;
    text.split_whitespace().next().map(str::to_owned)
}

pub fn configured_model() -> Option<String> {
    std::env::var("AGENT_JIT_OPENCODE_TEST_MODEL")
        .ok()
        .filter(|model| !model.is_empty())
}

pub fn skip(reason: &str) {
    let _ = writeln!(io::stderr().lock(), "SKIP: {reason}");
}
