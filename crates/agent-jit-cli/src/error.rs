//! The error contract.
//!
//! Every failure the binary can produce is one stable object — `{code, message, retryable,
//! details?}` — and one exit class. Callers (including the operator's shell and, later, the
//! benchmark harness) branch on the exit class; machines read the code. Neither may need to parse
//! prose.

use std::fmt;

use serde_json::{Value, json};

/// Exit classes. The numbers are part of the contract.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u8)]
pub enum ExitClass {
    /// The command was invoked incorrectly.
    Usage = 2,
    /// The host, schema, or command is not supported by this build.
    Unsupported = 3,
    /// The command refused for a safety reason: a private path, a sandbox failure, a secret.
    SafetyRefusal = 4,
    /// A gate decided to stop. Not a defect: a documented outcome.
    #[cfg_attr(
        not(test),
        expect(
            dead_code,
            reason = "part of the published exit contract; first constructed by the Phase 0 gate"
        )
    )]
    GateStop = 5,
    /// The command hit a fault it does not attribute to the caller.
    Internal = 70,
}

impl ExitClass {
    /// Returns the process exit code for the class.
    #[must_use]
    pub const fn code(self) -> u8 {
        self as u8
    }

    /// Returns the stable name of the class.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Usage => "usage",
            Self::Unsupported => "unsupported",
            Self::SafetyRefusal => "safety_refusal",
            Self::GateStop => "gate_stop",
            Self::Internal => "internal",
        }
    }
}

/// A command failure.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CommandError {
    /// Stable machine-readable code, e.g. `repo_not_a_repository`.
    pub code: String,
    /// Human-readable message. Redacted: it never carries captured payload bytes.
    pub message: String,
    /// Whether retrying the same invocation could plausibly succeed.
    pub retryable: bool,
    /// Optional structured detail. Boxed so the error stays small in every `Result`.
    pub details: Option<Box<Value>>,
    /// Exit class.
    pub class: ExitClass,
}

impl CommandError {
    /// Builds an error with an explicit class.
    pub fn new(code: impl Into<String>, message: impl Into<String>, class: ExitClass) -> Self {
        Self {
            code: code.into(),
            message: message.into(),
            retryable: false,
            details: None,
            class,
        }
    }

    /// A usage error: the command was not invoked correctly.
    pub fn usage(message: impl Into<String>) -> Self {
        Self::new("usage", message, ExitClass::Usage)
    }

    /// A refusal: the command ran and rejected its input.
    pub fn refused(code: impl Into<String>, message: impl Into<String>) -> Self {
        Self::new(code, message, ExitClass::SafetyRefusal)
    }

    /// The command exists but this build does not implement it yet.
    pub fn not_implemented(command: &str) -> Self {
        Self::new(
            "not_implemented",
            format!("`agent-jit {command}` is reserved but not implemented in this build"),
            ExitClass::Unsupported,
        )
        .with_details(json!({"command": command}))
    }

    /// Marks the error as retryable.
    #[must_use]
    #[cfg_attr(
        not(test),
        expect(
            dead_code,
            reason = "part of the published error contract; first used by the busy-timeout store path"
        )
    )]
    pub const fn retryable(mut self) -> Self {
        self.retryable = true;
        self
    }

    /// Attaches structured detail.
    #[must_use]
    pub fn with_details(mut self, details: Value) -> Self {
        self.details = Some(Box::new(details));
        self
    }

    /// Renders the error as its stable JSON object.
    #[must_use]
    pub fn to_json(&self) -> Value {
        let mut error = json!({
            "code": self.code,
            "message": self.message,
            "retryable": self.retryable,
            "class": self.class.as_str(),
        });
        if let Some(details) = &self.details
            && let Some(object) = error.as_object_mut()
        {
            object.insert("details".to_owned(), (**details).clone());
        }
        json!({"error": error})
    }
}

impl fmt::Display for CommandError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        if self.code == "usage" {
            f.write_str(&self.message)
        } else {
            write!(f, "error: {}: {}", self.code, self.message)
        }
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
mod tests {
    use super::{CommandError, ExitClass};

    #[test]
    fn exit_codes_are_part_of_the_contract() {
        assert_eq!(ExitClass::Usage.code(), 2);
        assert_eq!(ExitClass::Unsupported.code(), 3);
        assert_eq!(ExitClass::SafetyRefusal.code(), 4);
        assert_eq!(ExitClass::GateStop.code(), 5);
        assert_eq!(ExitClass::Internal.code(), 70);
    }

    #[test]
    fn a_gate_stop_is_reported_as_a_decision_not_a_defect() {
        let error = CommandError::new(
            "phase0_stop",
            "JIT coverage below the 20% threshold",
            ExitClass::GateStop,
        );
        assert_eq!(error.to_json()["error"]["class"], "gate_stop");
        assert_eq!(error.to_json()["error"]["retryable"], false);
    }

    #[test]
    fn retryable_errors_say_so_in_the_stable_object() {
        let error =
            CommandError::refused("store_busy", "another writer holds the lock").retryable();
        let json = error.to_json();
        assert_eq!(json["error"]["retryable"], true);
        assert_eq!(json["error"]["code"], "store_busy");
    }

    #[test]
    fn details_are_rendered_when_present_and_omitted_otherwise() {
        let plain = CommandError::usage("usage: agent-jit");
        assert!(plain.to_json()["error"].get("details").is_none());

        let detailed = CommandError::not_implemented("phase0");
        assert_eq!(detailed.to_json()["error"]["details"]["command"], "phase0");
    }
}
