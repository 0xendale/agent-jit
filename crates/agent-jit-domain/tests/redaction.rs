#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
//! Redaction runs before anything is persisted. These tests are the definition of "before".

use agent_jit_domain::redaction::{
    MAX_FIELD_BYTES, MAX_TRAJECTORY_BYTES, PathAliases, Redactor, SecretClass,
};

fn redactor() -> Redactor {
    Redactor::new()
}

#[test]
fn authorization_headers_and_bearer_tokens_are_removed() {
    let field = redactor().field("Authorization: Bearer sk-abcdef0123456789abcdef0123456789");
    assert!(!field.value().contains("sk-abcdef"), "{}", field.value());
    assert!(field.value().contains("[redacted:"), "{}", field.value());
    assert!(field.report().redactions > 0);
}

#[test]
fn known_token_shapes_are_removed_wherever_they_appear() {
    let slack_bot_token = format!(
        "xoxb-{}-{}-{}",
        "123456789012", "1234567890123", "abcdefghijklmnopqrstuvwx"
    );
    let samples = [
        "ghp_0123456789abcdefghijklmnopqrstuvwxyzAB",
        "github_pat_11ABCDEFG0123456789_abcdefghijklmnopqrstuvwxyz0123456789ABCDEFGH",
        slack_bot_token.as_str(),
        "AKIAIOSFODNN7EXAMPLE",
        "AIzaSyA0123456789abcdefghijklmnopqrstuv",
        "glpat-0123456789abcdefghij",
        "sk-ant-api03-0123456789abcdefghijklmnopqrstuvwxyz",
    ];
    for sample in samples {
        let field = redactor().field(&format!("the value is {sample} and then some"));
        assert!(
            !field.value().contains(sample),
            "token survived redaction: {}",
            field.value()
        );
    }
}

#[test]
fn credential_environment_assignments_keep_the_name_and_lose_the_value() {
    let field = redactor()
        .field("ANTHROPIC_API_KEY=super-secret-value AWS_SECRET_ACCESS_KEY=abc123 PATH=/usr/bin");
    assert!(
        field.value().contains("ANTHROPIC_API_KEY="),
        "{}",
        field.value()
    );
    assert!(
        !field.value().contains("super-secret-value"),
        "{}",
        field.value()
    );
    assert!(!field.value().contains("abc123"), "{}", field.value());
    // Structure that is not a credential must survive: it is what makes a trace useful.
    assert!(field.value().contains("PATH=/usr/bin"), "{}", field.value());
}

#[test]
fn private_keys_are_removed_whole() {
    let pem = "-----BEGIN OPENSSH PRIVATE KEY-----\nb3BlbnNzaC1rZXktdjEAAAAA\nSECRETMATERIAL\n-----END OPENSSH PRIVATE KEY-----";
    let field = redactor().field(&format!("here it is:\n{pem}\ndone"));
    assert!(
        !field.value().contains("SECRETMATERIAL"),
        "{}",
        field.value()
    );
    assert!(!field.value().contains("b3BlbnNzaC"), "{}", field.value());
    assert!(field.value().contains("done"));
}

#[test]
fn url_credentials_and_secret_query_parameters_are_removed() {
    let field = redactor().field(
        "cloning https://octocat:ghs_supersecrettoken@github.com/o/r.git?token=abc123def456&page=2",
    );
    assert!(
        !field.value().contains("ghs_supersecrettoken"),
        "{}",
        field.value()
    );
    assert!(!field.value().contains("abc123def456"), "{}", field.value());
    assert!(field.value().contains("github.com"), "{}", field.value());
    assert!(field.value().contains("page=2"), "{}", field.value());
}

#[test]
fn configured_literal_canaries_are_removed_anywhere() {
    let redactor = Redactor::new().with_canaries(vec!["CANARY-9c1f".to_owned()]);
    let field = redactor.field("output containing CANARY-9c1f in the middle");
    assert!(!field.value().contains("CANARY-9c1f"), "{}", field.value());
    assert!(field.report().classes_seen.contains(&SecretClass::Canary));
}

#[test]
fn mandatory_secret_classes_cannot_be_disabled_by_configuration() {
    // There is no constructor, setter, or flag that turns a mandatory class off.
    let redactor = Redactor::new().with_canaries(vec![]);
    let field = redactor.field("Authorization: Bearer sk-abcdef0123456789abcdef0123456789");
    assert!(!field.value().contains("sk-abcdef"));
}

#[test]
fn paths_become_stable_aliases() {
    let aliases = PathAliases::new("/Users/someone", "/Users/someone/code/project");
    let redactor = Redactor::new().with_path_aliases(aliases);
    let field = redactor.field(
        "failed at /Users/someone/code/project/src/main.rs:12 and /Users/someone/.cargo/bin",
    );

    assert!(
        field.value().contains("<repo>/src/main.rs:12"),
        "{}",
        field.value()
    );
    assert!(
        field.value().contains("<home>/.cargo/bin"),
        "{}",
        field.value()
    );
    assert!(
        !field.value().contains("/Users/someone"),
        "{}",
        field.value()
    );
}

#[test]
fn a_field_is_capped_and_reports_what_it_dropped() {
    let oversized = "a".repeat(MAX_FIELD_BYTES + 4096);
    let field = redactor().field(&oversized);

    assert!(field.value().len() <= MAX_FIELD_BYTES);
    assert!(field.report().truncated);
    assert_eq!(
        field.report().original_bytes,
        (MAX_FIELD_BYTES + 4096) as u64
    );
    assert_eq!(field.report().retained_bytes, field.value().len() as u64);
}

#[test]
fn truncation_never_splits_a_character() {
    let oversized = "é".repeat(MAX_FIELD_BYTES);
    let field = redactor().field(&oversized);
    assert!(field.value().is_char_boundary(field.value().len()));
    assert!(field.report().truncated);
}

#[test]
fn the_retained_digest_is_deterministic_and_covers_the_retained_bytes() {
    use agent_jit_domain::canonical::Digest;

    let first = redactor().field("Authorization: Bearer sk-abcdef0123456789abcdef0123456789 tail");
    let second = redactor().field("Authorization: Bearer sk-abcdef0123456789abcdef0123456789 tail");
    assert_eq!(first.report().digest, second.report().digest);
    assert_eq!(
        first.report().digest,
        Digest::of_bytes(first.value().as_bytes())
    );
}

#[test]
fn a_trajectory_budget_bounds_the_whole_run() {
    let mut budget = agent_jit_domain::redaction::TrajectoryBudget::new();
    let chunk = "x".repeat(MAX_FIELD_BYTES);

    let mut accepted = 0_usize;
    while budget.try_reserve(chunk.len() as u64) {
        accepted += chunk.len();
    }

    assert!(accepted <= MAX_TRAJECTORY_BYTES);
    assert!(budget.exhausted());
    assert!(budget.dropped_events() > 0);
}

#[test]
fn nothing_useful_is_lost_when_there_is_nothing_to_redact() {
    let text = "cargo test --workspace exited 1: 3 failed in crates/agent-jit-cli";
    let field = redactor().field(text);
    assert_eq!(field.value(), text);
    assert_eq!(field.report().redactions, 0);
    assert!(!field.report().truncated);
}
