#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
//! Input is bounded before it is parsed, and redacted before it can be stored.

use agent_jit_domain::redaction::{MAX_FIELD_BYTES, PathAliases, Redactor};
use agent_jit_engine::normalize::{BoundedInput, NormalizeError, read_bounded};

#[test]
fn a_small_input_is_read_whole() {
    let source = b"{\"session_id\":\"abc\"}".to_vec();
    let input = read_bounded(&mut source.as_slice(), 1024).unwrap();

    assert_eq!(input.bytes(), source.as_slice());
    assert!(!input.truncated());
    assert_eq!(input.observed_bytes(), source.len() as u64);
}

#[test]
fn an_oversized_input_is_bounded_but_still_counted() {
    let oversized = vec![b'x'; 3 * MAX_FIELD_BYTES];
    let input = read_bounded(&mut oversized.as_slice(), MAX_FIELD_BYTES).unwrap();

    assert_eq!(input.bytes().len(), MAX_FIELD_BYTES);
    assert!(input.truncated());
    assert_eq!(input.observed_bytes(), oversized.len() as u64);
    // The retained buffer never grows past the limit: the rest is counted, not kept.
    assert!(input.bytes().capacity() <= MAX_FIELD_BYTES + 8192);
}

#[test]
fn a_bounded_input_reports_the_digest_of_what_it_kept() {
    use agent_jit_domain::canonical::Digest;

    let source = vec![b'y'; 128];
    let input = read_bounded(&mut source.as_slice(), 64).unwrap();
    assert_eq!(input.digest(), Digest::of_bytes(input.bytes()));
}

#[test]
fn invalid_utf8_is_reported_rather_than_silently_replaced() {
    let bytes = vec![0xff, 0xfe, 0xfd];
    let input = read_bounded(&mut bytes.as_slice(), 1024).unwrap();
    let error = input.as_text().unwrap_err();
    assert_eq!(error.code(), "normalize_invalid_utf8");
    assert!(
        matches!(error, NormalizeError::InvalidUtf8 { .. }),
        "{error:?}"
    );
}

#[test]
fn text_reaches_the_store_only_through_the_redactor() {
    let raw = b"Authorization: Bearer sk-abcdef0123456789abcdef0123456789".to_vec();
    let input = read_bounded(&mut raw.as_slice(), 1024).unwrap();
    let redacted = input.redact_text(&Redactor::new()).unwrap();

    assert!(
        !redacted.value().contains("sk-abcdef"),
        "{}",
        redacted.value()
    );
    assert!(redacted.report().redactions > 0);
}

#[test]
fn path_aliases_survive_the_normalization_boundary() {
    let raw = b"error at /Users/someone/code/project/src/main.rs".to_vec();
    let input = read_bounded(&mut raw.as_slice(), 1024).unwrap();
    let redactor = Redactor::new().with_path_aliases(PathAliases::new(
        "/Users/someone",
        "/Users/someone/code/project",
    ));

    let redacted = input.redact_text(&redactor).unwrap();
    assert_eq!(redacted.value(), "error at <repo>/src/main.rs");
}

#[test]
fn a_truncated_input_stays_truncated_after_redaction() {
    let raw = vec![b'z'; MAX_FIELD_BYTES * 2];
    let input = read_bounded(&mut raw.as_slice(), MAX_FIELD_BYTES).unwrap();
    let redacted = input.redact_text(&Redactor::new()).unwrap();

    assert!(redacted.report().truncated);
    assert!(redacted.value().len() <= MAX_FIELD_BYTES);
}

#[test]
fn a_zero_limit_is_refused_rather_than_silently_dropping_everything() {
    let raw = b"anything".to_vec();
    let error = read_bounded(&mut raw.as_slice(), 0).unwrap_err();
    assert_eq!(error.code(), "normalize_limit_invalid");
}

/// A reader that fails partway, to prove errors are reported rather than treated as end-of-input.
struct FailingReader {
    delivered: bool,
}

impl std::io::Read for FailingReader {
    fn read(&mut self, buffer: &mut [u8]) -> std::io::Result<usize> {
        if self.delivered {
            return Err(std::io::Error::other("device disappeared"));
        }
        self.delivered = true;
        let chunk = b"partial";
        buffer[..chunk.len()].copy_from_slice(chunk);
        Ok(chunk.len())
    }
}

#[test]
fn a_read_failure_is_reported_and_never_looks_like_a_complete_input() {
    let error = read_bounded(&mut FailingReader { delivered: false }, 1024).unwrap_err();
    assert_eq!(error.code(), "normalize_unreadable");
}

#[test]
fn bounded_input_never_reports_more_retained_than_observed() {
    for limit in [1_usize, 7, 64, 1024] {
        let raw = vec![b'q'; 500];
        let input: BoundedInput = read_bounded(&mut raw.as_slice(), limit).unwrap();
        assert!(input.bytes().len() as u64 <= input.observed_bytes());
        assert!(input.bytes().len() <= limit);
    }
}
