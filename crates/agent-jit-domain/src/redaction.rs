//! Redaction and bounding, applied before anything is persisted.
//!
//! Traces are captured from real work, so they contain whatever the operator's terminal contained:
//! tokens, headers, keys, absolute paths. Redaction is therefore not a feature of the storage
//! layer — it is a property of the type that reaches it. Only a [`Redacted<T>`] can be stored, and
//! the only way to build one is to run text through a [`Redactor`], which always applies the
//! mandatory secret classes. There is no configuration that turns them off.
//!
//! Bounds are equally mandatory: a single field is capped at [`MAX_FIELD_BYTES`] and a whole
//! trajectory at [`MAX_TRAJECTORY_BYTES`]. What was dropped is recorded as counts, never as bytes.

use std::collections::BTreeSet;
use std::sync::LazyLock;

use regex::Regex;
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use crate::canonical::Digest;

/// Maximum bytes retained for one captured field or event payload: 64 KiB.
pub const MAX_FIELD_BYTES: usize = 64 * 1024;

/// Maximum bytes retained for one normalized trajectory: 8 MiB.
pub const MAX_TRAJECTORY_BYTES: usize = 8 * 1024 * 1024;

/// The same ceiling as a byte count, for budgets that track `u64` totals.
const MAX_TRAJECTORY_BYTES_U64: u64 = 8 * 1024 * 1024;

/// A class of secret the redactor removes.
#[derive(
    Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize, JsonSchema,
)]
#[serde(rename_all = "snake_case")]
pub enum SecretClass {
    /// An `Authorization` header or a bearer token.
    AuthorizationHeader,
    /// A provider token with a recognizable prefix.
    ProviderToken,
    /// A credential-shaped environment assignment.
    CredentialEnvironment,
    /// A PEM-encoded private key block.
    PrivateKey,
    /// Credentials embedded in a URL, or a secret query parameter.
    UrlCredential,
    /// A literal canary configured by the operator or a test.
    Canary,
}

impl SecretClass {
    /// Returns the `snake_case` name used in the replacement marker.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::AuthorizationHeader => "authorization",
            Self::ProviderToken => "token",
            Self::CredentialEnvironment => "credential_env",
            Self::PrivateKey => "private_key",
            Self::UrlCredential => "url_credential",
            Self::Canary => "canary",
        }
    }

    /// The marker that replaces removed material.
    #[must_use]
    pub fn marker(self) -> String {
        format!("[redacted:{}]", self.as_str())
    }
}

/// What redaction and bounding did to one field.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct RedactionReport {
    /// Size of the input before anything was removed.
    pub original_bytes: u64,
    /// Size of what is retained.
    pub retained_bytes: u64,
    /// Whether the cap dropped part of the input.
    pub truncated: bool,
    /// How many pieces of material were replaced.
    pub redactions: u32,
    /// Which classes were seen, sorted.
    pub classes_seen: BTreeSet<SecretClass>,
    /// Digest of the retained bytes.
    pub digest: Digest,
}

/// Text that has been through the redactor and the cap.
///
/// The inner value is private: constructing one without redacting is not possible from outside
/// this module, which is what makes "redaction happens before persistence" a type-level fact.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct Redacted<T> {
    value: T,
    report: RedactionReport,
}

impl<T> Redacted<T> {
    /// Returns the redacted value.
    pub const fn value(&self) -> &T {
        &self.value
    }

    /// Returns what redaction did.
    pub const fn report(&self) -> &RedactionReport {
        &self.report
    }

    /// Consumes the wrapper and returns the redacted value.
    pub fn into_value(self) -> T {
        self.value
    }
}

impl Redacted<String> {
    /// Records that the input was already cut short before redaction saw it.
    ///
    /// A read limit and the field cap are two different reasons for the same fact; the report must
    /// state the fact either way, and must not claim a smaller input than was observed.
    #[must_use]
    pub fn mark_truncated(mut self, observed_bytes: u64) -> Self {
        self.report.truncated = true;
        self.report.original_bytes = self.report.original_bytes.max(observed_bytes);
        self
    }
}

/// Stable aliases for absolute paths.
///
/// Absolute identity is rarely what a trace needs: `<repo>/src/main.rs` compares across machines
/// and checkouts, while `/Users/someone/code/project/src/main.rs` does not, and leaks a username.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct PathAliases {
    home: String,
    repository: String,
}

impl PathAliases {
    /// Builds aliases for a home directory and a repository worktree.
    #[must_use]
    pub fn new(home: &str, repository: &str) -> Self {
        Self {
            home: home.trim_end_matches('/').to_owned(),
            repository: repository.trim_end_matches('/').to_owned(),
        }
    }

    /// Rewrites absolute paths in `text` to their aliases.
    #[must_use]
    pub fn apply(&self, text: &str) -> String {
        let mut rewritten = text.to_owned();
        // The repository first: it is usually inside the home directory.
        if !self.repository.is_empty() {
            rewritten = rewritten.replace(&self.repository, "<repo>");
        }
        if !self.home.is_empty() {
            rewritten = rewritten.replace(&self.home, "<home>");
        }
        rewritten
    }
}

/// One mandatory redaction rule.
struct Rule {
    class: SecretClass,
    pattern: Regex,
    /// Replacement template; `$name` groups from `pattern` are preserved.
    replacement: &'static str,
}

/// The mandatory rules, compiled once.
///
/// Patterns are deliberately anchored on recognizable structure rather than entropy: a generic
/// "long random-looking string" rule would eat the command output that makes a trace useful, and
/// would still miss short credentials.
static RULES: LazyLock<Vec<Rule>> = LazyLock::new(|| {
    let rule = |class: SecretClass, pattern: &str, replacement: &'static str| Rule {
        class,
        pattern: Regex::new(pattern)
            .unwrap_or_else(|error| unreachable!("built-in redaction pattern is valid: {error}")),
        replacement,
    };

    vec![
        rule(
            SecretClass::PrivateKey,
            r"(?s)-----BEGIN [A-Z ]*PRIVATE KEY-----.*?-----END [A-Z ]*PRIVATE KEY-----",
            "[redacted:private_key]",
        ),
        rule(
            SecretClass::AuthorizationHeader,
            r"(?i)(?P<name>authorization\s*[:=]\s*)(bearer\s+|basic\s+|token\s+)?[A-Za-z0-9._~+/=\-]{8,}",
            "${name}[redacted:authorization]",
        ),
        rule(
            SecretClass::AuthorizationHeader,
            r"(?i)\bbearer\s+[A-Za-z0-9._~+/=\-]{8,}",
            "[redacted:authorization]",
        ),
        rule(
            SecretClass::UrlCredential,
            r"(?P<scheme>[a-zA-Z][a-zA-Z0-9+.\-]*://)(?P<user>[^/:@\s]+):[^/@\s]+@",
            "${scheme}${user}:[redacted:url_credential]@",
        ),
        rule(
            SecretClass::UrlCredential,
            r#"(?i)(?P<key>[?&](?:token|api[_-]?key|access[_-]?token|secret|password|signature|sig)=)[^&\s"']+"#,
            "${key}[redacted:url_credential]",
        ),
        rule(
            SecretClass::ProviderToken,
            r"\b(?:sk-ant-[A-Za-z0-9_\-]{8,}|sk-[A-Za-z0-9_\-]{16,}|gh[pousr]_[A-Za-z0-9]{16,}|github_pat_[A-Za-z0-9_]{20,}|xox[baprs]-[A-Za-z0-9\-]{10,}|glpat-[A-Za-z0-9_\-]{16,}|AKIA[0-9A-Z]{12,}|ASIA[0-9A-Z]{12,}|AIza[A-Za-z0-9_\-]{30,})",
            "[redacted:token]",
        ),
        rule(
            SecretClass::CredentialEnvironment,
            r#"(?P<name>\b[A-Z][A-Z0-9_]*(?:API[_-]?KEY|TOKEN|SECRET|PASSWORD|PASSWD|CREDENTIALS?|PRIVATE[_-]?KEY|SESSION[_-]?KEY)[A-Z0-9_]*\s*=\s*)(?:"[^"]*"|'[^']*'|[^\s]+)"#,
            "${name}[redacted:credential_env]",
        ),
    ]
});

/// Applies the mandatory redaction classes and the byte caps.
#[derive(Debug, Clone, Default)]
pub struct Redactor {
    canaries: Vec<String>,
    aliases: Option<PathAliases>,
}

impl Redactor {
    /// Builds a redactor with the mandatory classes only.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Adds literal canaries to remove, in addition to the mandatory classes.
    #[must_use]
    pub fn with_canaries(mut self, canaries: Vec<String>) -> Self {
        self.canaries = canaries.into_iter().filter(|c| !c.is_empty()).collect();
        self
    }

    /// Adds path aliasing.
    #[must_use]
    pub fn with_path_aliases(mut self, aliases: PathAliases) -> Self {
        self.aliases = Some(aliases);
        self
    }

    /// Redacts and bounds one field.
    ///
    /// Redaction runs before truncation so a secret that straddles the cap cannot leave a usable
    /// prefix behind. Only a bounded window of the input is examined, so an oversized payload
    /// cannot force an unbounded amount of work.
    #[must_use]
    pub fn field(&self, text: &str) -> Redacted<String> {
        let original_bytes = text.len() as u64;

        // Examine at most twice the cap: enough for a secret straddling the boundary, bounded
        // enough that a huge payload cannot drive the cost of redaction.
        let window_limit = MAX_FIELD_BYTES.saturating_mul(2);
        let window = if text.len() > window_limit {
            &text[..floor_char_boundary(text, window_limit)]
        } else {
            text
        };

        let mut classes_seen = BTreeSet::new();
        let mut redactions = 0_u32;
        let mut working = window.to_owned();

        for canary in &self.canaries {
            if working.contains(canary.as_str()) {
                let hits = working.matches(canary.as_str()).count();
                redactions = redactions.saturating_add(u32::try_from(hits).unwrap_or(u32::MAX));
                working = working.replace(canary.as_str(), &SecretClass::Canary.marker());
                classes_seen.insert(SecretClass::Canary);
            }
        }

        for rule in RULES.iter() {
            let matches = rule.pattern.find_iter(&working).count();
            if matches > 0 {
                redactions = redactions.saturating_add(u32::try_from(matches).unwrap_or(u32::MAX));
                classes_seen.insert(rule.class);
                working = rule
                    .pattern
                    .replace_all(&working, rule.replacement)
                    .into_owned();
            }
        }

        if let Some(aliases) = &self.aliases {
            working = aliases.apply(&working);
        }

        let truncated_by_cap = working.len() > MAX_FIELD_BYTES;
        if truncated_by_cap {
            working.truncate(floor_char_boundary(&working, MAX_FIELD_BYTES));
        }
        let window_bytes = u64::try_from(window.len()).unwrap_or(u64::MAX);
        let truncated = truncated_by_cap || original_bytes > window_bytes;

        let digest = Digest::of_bytes(working.as_bytes());
        let retained_bytes = working.len() as u64;

        Redacted {
            value: working,
            report: RedactionReport {
                original_bytes,
                retained_bytes,
                truncated,
                redactions,
                classes_seen,
                digest,
            },
        }
    }

    /// Redacts and bounds a list of fields, such as an argument vector.
    #[must_use]
    pub fn argv(&self, argv: &[String]) -> Redacted<Vec<String>> {
        let mut values = Vec::with_capacity(argv.len());
        let mut original_bytes = 0_u64;
        let mut retained_bytes = 0_u64;
        let mut truncated = false;
        let mut redactions = 0_u32;
        let mut classes_seen = BTreeSet::new();

        for argument in argv {
            let redacted = self.field(argument);
            original_bytes = original_bytes.saturating_add(redacted.report.original_bytes);
            retained_bytes = retained_bytes.saturating_add(redacted.report.retained_bytes);
            truncated |= redacted.report.truncated;
            redactions = redactions.saturating_add(redacted.report.redactions);
            classes_seen.extend(redacted.report.classes_seen.iter().copied());
            values.push(redacted.value);
        }

        let digest = Digest::of_bytes(values.join("\u{0}").as_bytes());
        Redacted {
            value: values,
            report: RedactionReport {
                original_bytes,
                retained_bytes,
                truncated,
                redactions,
                classes_seen,
                digest,
            },
        }
    }
}

/// Tracks how much of one trajectory's byte budget has been used.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TrajectoryBudget {
    used: u64,
    dropped_events: u32,
}

impl Default for TrajectoryBudget {
    fn default() -> Self {
        Self::new()
    }
}

impl TrajectoryBudget {
    /// Builds a fresh budget.
    #[must_use]
    pub const fn new() -> Self {
        Self {
            used: 0,
            dropped_events: 0,
        }
    }

    /// Reserves `bytes`, returning whether the reservation fit inside the budget.
    ///
    /// A refused reservation counts as a dropped event rather than a silent omission: a trajectory
    /// that hit its ceiling is a different fact from one that did not, and Phase 0 needs to know.
    pub fn try_reserve(&mut self, bytes: u64) -> bool {
        let projected = self.used.saturating_add(bytes);
        if projected > MAX_TRAJECTORY_BYTES_U64 {
            self.dropped_events = self.dropped_events.saturating_add(1);
            return false;
        }
        self.used = projected;
        true
    }

    /// Bytes reserved so far.
    #[must_use]
    pub const fn used(&self) -> u64 {
        self.used
    }

    /// Whether the budget has refused at least one reservation.
    #[must_use]
    pub const fn exhausted(&self) -> bool {
        self.dropped_events > 0
    }

    /// How many events were refused.
    #[must_use]
    pub const fn dropped_events(&self) -> u32 {
        self.dropped_events
    }
}

/// Returns the largest character boundary at or below `limit`.
fn floor_char_boundary(text: &str, limit: usize) -> usize {
    if limit >= text.len() {
        return text.len();
    }
    let mut boundary = limit;
    while boundary > 0 && !text.is_char_boundary(boundary) {
        boundary -= 1;
    }
    boundary
}
