#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
//! Recorder bench: what `hook ingest` costs, and whether `store recover` keeps what it was given.
//!
//! Ignored by default because it spawns thousands of processes. Run it explicitly:
//!
//! ```text
//! LC_ALL=C TZ=UTC cargo test -p agent-jit-cli --test recorder_bench -- --ignored --nocapture
//! ```
//!
//! Every child starts from an empty environment with its own `AGENT_JIT_HOME` under one temporary
//! directory, so no state outside that directory is read or written. Negative controls run first:
//! each plants a fault the detectors must report, and the run fails if one goes unreported. Secret
//! leakage, recorder loss, and store integrity are asserted. Latency and throughput are written to
//! a JSON report and never asserted.

use std::collections::{BTreeMap, BTreeSet};
use std::fs::{self, OpenOptions};
use std::io::{self, Write as _};
use std::os::unix::fs::{FileExt as _, OpenOptionsExt as _};
use std::os::unix::process::ExitStatusExt as _;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::Mutex;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::thread;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use agent_jit_domain::redaction::{MAX_FIELD_BYTES, MAX_TRAJECTORY_BYTES};
use agent_jit_engine::adapters::claude_hooks::HookEventKind;
use agent_jit_engine::recorder::Segment;
use rusqlite::{Connection, OpenFlags};
use serde_json::{Value, json};
use tempfile::TempDir;

const BINARY: &str = env!("CARGO_BIN_EXE_agent-jit");
const SAFE_PATH: &str = "/usr/bin:/bin:/usr/sbin:/sbin";
const RUNTIME_VERSION: &str = "2.0.0";
/// Prefix of every secret in the fixtures. No byte sequence like it may be persisted or printed.
const CANARY: &[u8] = b"CANARY-";
const SIGKILL: i32 = 9;
const REPORT_VERSION: u32 = 1;
const EVIDENCE_CLASS: &str = "engineering; not Phase 0 or Gate B evidence";
const SEGMENTS: &str = "cache/spool/segments";
const DATABASE: &str = "data/state.sqlite3";

const WARMUP_SESSIONS: usize = 2;
const COLD_HOMES: usize = 30;
const SEQUENTIAL_SESSIONS: usize = 125;
const BURST_SESSIONS: usize = 250;
const BURST_WORKERS: usize = 16;
const CRASH_INGEST_SESSIONS: usize = 100;
const CRASH_INGEST_WORKERS: usize = 8;
const CRASH_RECOVER_SESSIONS: usize = 200;
const CRASH_RECOVER_ATTEMPTS: usize = 40;
const OVERSIZE_EVENTS: usize = 72;
const OVERSIZE_FIELD_BYTES: usize = 70 * 1024;

/// The hooks of one bench session, in order.
const SCRIPT: [HookEventKind; 8] = [
    HookEventKind::SessionStart,
    HookEventKind::UserPromptSubmit,
    HookEventKind::PreToolUse,
    HookEventKind::PostToolUse,
    HookEventKind::PreToolUse,
    HookEventKind::PostToolUseFailure,
    HookEventKind::Stop,
    HookEventKind::SessionEnd,
];

#[test]
#[ignore = "spawns thousands of processes; run with --ignored"]
fn recorder_bench() {
    let ambient_before = Ambient::observe();
    let bench = Bench::new();

    let controls = run_controls(&bench);
    let undetected = controls.undetected();
    progress(&format!(
        "controls: undetected {undetected:?}, {} baseline violation(s)",
        controls.baseline.len()
    ));
    let mut violations: Vec<Value> = controls
        .baseline
        .iter()
        .map(|found| found.to_json("controls"))
        .collect();

    let mut phases = serde_json::Map::new();
    phases.insert("warm_up".to_owned(), warm_up(&bench));
    let runs: [fn(&Bench) -> PhaseResult; 6] = [
        phase_cold,
        phase_sequential,
        phase_burst,
        phase_crash_ingest,
        phase_crash_recover,
        phase_oversize,
    ];
    for run in runs {
        let started = Instant::now();
        let mut result = run(&bench);
        let wall_ms = u64::try_from(started.elapsed().as_millis()).unwrap_or(u64::MAX);
        progress(&format!(
            "{}: {} ({} violation(s), {wall_ms} ms)",
            result.name,
            result.report["headline"],
            result.violations.len()
        ));
        result.report["phase_wall_ms"] = json!(wall_ms);
        violations.extend(
            result
                .violations
                .iter()
                .map(|found| found.to_json(result.name)),
        );
        phases.insert(result.name.to_owned(), result.report);
    }

    let ambient_after = Ambient::observe();
    let ambient_unchanged = ambient_before == ambient_after;
    let report = json!({
        "report_version": REPORT_VERSION,
        "evidence_class": EVIDENCE_CLASS,
        "generated_at_unix_ms": unix_ms(),
        "machine": machine(),
        "toolchain": {
            "rustc": rustc_version(),
            "profile": if cfg!(debug_assertions) { "debug" } else { "release" },
        },
        "source": source_state(),
        "parameters": parameters(),
        "controls": controls.to_json(),
        "phases": phases,
        "ambient": {
            "agent_jit_home_set": ambient_before.home_set,
            "entries_before": ambient_before.entries.len(),
            "entries_after": ambient_after.entries.len(),
            "application_support_before": ambient_before.application_support,
            "application_support_after": ambient_after.application_support,
            "unchanged": ambient_unchanged,
        },
        "violations": violations,
        "passed": undetected.is_empty() && violations.is_empty() && ambient_unchanged,
    });
    let path = write_report(&report);
    print_summary(&path, &report);

    let report_canaries = canary_offsets(&fs::read(&path).unwrap()).len();
    assert_eq!(
        report_canaries,
        0,
        "the report holds canary bytes; report {}",
        path.display()
    );
    assert!(
        undetected.is_empty(),
        "negative controls went undetected: {undetected:?}; report {}",
        path.display()
    );
    assert!(
        violations.is_empty(),
        "{} invariant violation(s); report {}",
        violations.len(),
        path.display()
    );
    assert!(
        ambient_unchanged,
        "state outside the bench root changed; report {}",
        path.display()
    );
}

// ---------------------------------------------------------------------------------------------
// Isolation and payloads
// ---------------------------------------------------------------------------------------------

/// Which runtime's hook bridge a session imitates.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Runtime {
    Claude,
    Opencode,
}

impl Runtime {
    const fn fixtures(self) -> &'static str {
        match self {
            Self::Claude => "claude-hooks",
            Self::Opencode => "opencode-hooks",
        }
    }

    /// The payload field naming the working directory.
    const fn directory_field(self) -> &'static str {
        match self {
            Self::Claude => "cwd",
            Self::Opencode => "directory",
        }
    }

    /// The payload field holding a tool's text output.
    const fn output_field(self) -> &'static str {
        match self {
            Self::Claude => "stdout",
            Self::Opencode => "output",
        }
    }
}

/// One scripted session.
#[derive(Debug, Clone)]
struct SessionSpec {
    key: String,
    runtime: Runtime,
    secrets: bool,
}

impl SessionSpec {
    fn new(key: &str, runtime: Runtime, secrets: bool) -> Self {
        Self {
            key: key.to_owned(),
            runtime,
            secrets,
        }
    }
}

/// One temporary root holding the bench repository, the user directory, and every home.
struct Bench {
    root: TempDir,
    repository: PathBuf,
    user: PathBuf,
    fixtures: BTreeMap<String, Value>,
}

impl Bench {
    fn new() -> Self {
        let root = tempfile::Builder::new()
            .prefix("agent-jit-bench-")
            .tempdir()
            .unwrap();
        let repository = root.path().join("repo");
        let user = root.path().join("user");
        fs::create_dir(&repository).unwrap();
        fs::create_dir(&user).unwrap();
        seed_repository(&repository, &user);
        Self {
            fixtures: load_fixtures(),
            root,
            repository,
            user,
        }
    }

    /// A home that does not exist yet, so the first command against it creates it.
    fn home(&self, name: &str) -> PathBuf {
        let home = self.root.path().join(name);
        assert!(!home.exists(), "home {name} already exists");
        home
    }

    fn fixture(&self, runtime: Runtime, name: &str) -> &Value {
        let key = format!("{}/{name}", runtime.fixtures());
        self.fixtures
            .get(&key)
            .unwrap_or_else(|| panic!("fixture {key} is not loaded"))
    }

    /// A command for the binary under test that inherits nothing from this process.
    fn command(&self, home: &Path) -> Command {
        let mut command = Command::new(BINARY);
        command
            .env_clear()
            .env("AGENT_JIT_HOME", home)
            .env("HOME", &self.user)
            .env("PATH", SAFE_PATH)
            .env("LC_ALL", "C")
            .env("LANG", "C")
            .env("TZ", "UTC")
            .current_dir(&self.user);
        command
    }
}

fn load_fixtures() -> BTreeMap<String, Value> {
    let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("../agent-jit-engine/tests/fixtures");
    let mut fixtures = BTreeMap::new();
    for runtime in [Runtime::Claude, Runtime::Opencode] {
        let names = SCRIPT
            .map(HookEventKind::as_str)
            .into_iter()
            .chain(["with-secrets", "wrong-types"]);
        for name in names {
            let key = format!("{}/{name}", runtime.fixtures());
            let path = root.join(format!("{key}.json"));
            let raw = fs::read(&path).unwrap_or_else(|error| panic!("{}: {error}", path.display()));
            fixtures.insert(key, serde_json::from_slice(&raw).unwrap());
        }
    }
    fixtures
}

/// Creates the repository every bench session claims to work in.
fn seed_repository(repository: &Path, user: &Path) {
    let git = |args: &[&str]| {
        let output = Command::new("/usr/bin/git")
            .args(args)
            .current_dir(repository)
            .env_clear()
            .env("PATH", SAFE_PATH)
            .env("HOME", user)
            .env("LC_ALL", "C")
            .env("LANG", "C")
            .env("TZ", "UTC")
            .env("GIT_CONFIG_NOSYSTEM", "1")
            .env("GIT_CONFIG_GLOBAL", "/dev/null")
            .env("GIT_AUTHOR_NAME", "bench")
            .env("GIT_AUTHOR_EMAIL", "bench@example.invalid")
            .env("GIT_AUTHOR_DATE", "2026-01-01T00:00:00Z")
            .env("GIT_COMMITTER_NAME", "bench")
            .env("GIT_COMMITTER_EMAIL", "bench@example.invalid")
            .env("GIT_COMMITTER_DATE", "2026-01-01T00:00:00Z")
            .stdin(Stdio::null())
            .output()
            .unwrap();
        assert!(
            output.status.success(),
            "git {args:?}: {}",
            String::from_utf8_lossy(&output.stderr)
        );
    };
    git(&["init", "--initial-branch=main", "--quiet"]);
    fs::write(repository.join("README.md"), "recorder bench repository\n").unwrap();
    git(&["add", "README.md"]);
    git(&["commit", "--quiet", "--no-gpg-sign", "-m", "bench seed"]);
}

/// Builds the payload for one scripted step, marked so its segment can be found again.
///
/// Secret-bearing sessions take their tool events from the `with-secrets` fixture and carry its
/// command and output in the prompt, so every redacted field meets the same secrets.
fn payload(bench: &Bench, spec: &SessionSpec, step: usize, marker: &str) -> Value {
    let kind = SCRIPT[step];
    let secrets = bench.fixture(spec.runtime, "with-secrets");
    let mut document = if spec.secrets && kind == HookEventKind::PostToolUse {
        secrets.clone()
    } else {
        bench.fixture(spec.runtime, kind.as_str()).clone()
    };
    match step {
        1 => {
            let text = if spec.secrets {
                format!(
                    "{} {}",
                    secrets["tool_input"]["command"].as_str().unwrap(),
                    secrets["tool_response"][spec.runtime.output_field()]
                        .as_str()
                        .unwrap()
                )
            } else {
                document["prompt"].as_str().unwrap().to_owned()
            };
            document["prompt"] = json!(format!("[bench {}] {text}", spec.key));
        }
        2 if spec.secrets => document["tool_input"] = secrets["tool_input"].clone(),
        4 => document["tool_input"]["command"] = json!("cargo clippy --workspace --all-targets"),
        _ => {}
    }
    locate(bench, spec, &mut document, marker);
    document
}

/// Points a payload at its bench session and the bench repository, and marks it.
///
/// The marker is an unknown top-level field, so the adapter keeps it as an extension and the
/// segment checksum covers it: every event lands in a distinct file.
fn locate(bench: &Bench, spec: &SessionSpec, document: &mut Value, marker: &str) {
    document["session_id"] = json!(spec.key);
    document[spec.runtime.directory_field()] = json!(bench.repository.to_string_lossy());
    if spec.runtime == Runtime::Claude {
        let transcript = bench
            .user
            .join("transcripts")
            .join(format!("{}.jsonl", spec.key));
        document["transcript_path"] = json!(transcript.to_string_lossy());
    }
    document["bench_event"] = json!(marker);
}

// ---------------------------------------------------------------------------------------------
// Running children and keeping the ledger
// ---------------------------------------------------------------------------------------------

/// How one child process ended, without the bytes it printed.
#[derive(Debug, Clone)]
struct Outcome {
    micros: u64,
    exit_code: Option<i32>,
    signal: Option<i32>,
    stdout_bytes: usize,
    stderr_bytes: usize,
    /// First stderr line, shortened, with the canary prefix defused.
    excerpt: String,
    output_has_canary: bool,
}

impl Outcome {
    /// Exited zero and printed nothing.
    fn clean(&self) -> bool {
        self.exit_code == Some(0) && self.stdout_bytes == 0 && self.stderr_bytes == 0
    }

    fn describe(&self) -> String {
        format!(
            "exit {:?}, signal {:?}, stdout {} byte(s), stderr {} byte(s): {}",
            self.exit_code, self.signal, self.stdout_bytes, self.stderr_bytes, self.excerpt
        )
    }
}

/// One ingest the harness sent.
#[derive(Debug, Clone)]
struct Sent {
    session: String,
    marker: String,
    kind: HookEventKind,
    kill_planned: bool,
    outcome: Outcome,
}

/// Runs a child, feeding `input` on stdin and killing it after `kill_after` when set.
fn run_child(
    mut command: Command,
    input: Option<&[u8]>,
    kill_after: Option<Duration>,
) -> (Outcome, Vec<u8>) {
    command
        .stdin(if input.is_some() {
            Stdio::piped()
        } else {
            Stdio::null()
        })
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    let started = Instant::now();
    let mut child = command.spawn().unwrap();
    if let Some(bytes) = input {
        // A child killed early closes the pipe; that failed write is part of the experiment.
        let _ = child.stdin.take().unwrap().write_all(bytes);
    }
    if let Some(delay) = kill_after {
        thread::sleep(delay);
        let _ = child.kill();
    }
    let output = child.wait_with_output().unwrap();
    let micros = micros_since(started);

    let stderr = String::from_utf8_lossy(&output.stderr);
    let excerpt = stderr
        .lines()
        .next()
        .unwrap_or_default()
        .replace("CANARY-", "CANARY_")
        .chars()
        .take(160)
        .collect();
    let outcome = Outcome {
        micros,
        exit_code: output.status.code(),
        signal: output.status.signal(),
        stdout_bytes: output.stdout.len(),
        stderr_bytes: output.stderr.len(),
        excerpt,
        output_has_canary: !canary_offsets(&output.stdout).is_empty()
            || !canary_offsets(&output.stderr).is_empty(),
    };
    (outcome, output.stdout)
}

/// Sends one payload to `hook ingest`.
fn send(
    bench: &Bench,
    home: &Path,
    runtime: Runtime,
    kind: HookEventKind,
    document: &Value,
    kill_after: Option<Duration>,
) -> Outcome {
    let mut command = bench.command(home);
    command.args([
        "hook",
        "ingest",
        "--event",
        kind.as_str(),
        "--claude-version",
        RUNTIME_VERSION,
    ]);
    if runtime == Runtime::Opencode {
        command.env("AGENT_JIT_HOOK_ADAPTER", "opencode");
    }
    run_child(command, Some(document.to_string().as_bytes()), kill_after).0
}

/// Decides, from an ingest's global index, whether and when to kill it.
type KillPlan<'a> = &'a (dyn Fn(usize) -> Option<Duration> + Sync);

const fn never_kill(_index: usize) -> Option<Duration> {
    None
}

/// Sends every scripted event of every session, one session per worker at a time.
fn ingest_sessions(
    bench: &Bench,
    home: &Path,
    specs: &[SessionSpec],
    workers: usize,
    kill_plan: KillPlan<'_>,
) -> Vec<Sent> {
    let next = AtomicUsize::new(0);
    let ledger = Mutex::new(Vec::with_capacity(specs.len() * SCRIPT.len()));
    thread::scope(|scope| {
        for _ in 0..workers {
            scope.spawn(|| {
                loop {
                    let index = next.fetch_add(1, Ordering::Relaxed);
                    let Some(spec) = specs.get(index) else {
                        break;
                    };
                    for (step, kind) in SCRIPT.into_iter().enumerate() {
                        let marker = format!("{}-e{step}", spec.key);
                        let document = payload(bench, spec, step, &marker);
                        let kill_after = kill_plan(index * SCRIPT.len() + step);
                        let outcome = send(bench, home, spec.runtime, kind, &document, kill_after);
                        ledger.lock().unwrap().push(Sent {
                            session: spec.key.clone(),
                            marker,
                            kind,
                            kill_planned: kill_after.is_some(),
                            outcome,
                        });
                    }
                }
            });
        }
    });
    let mut sent = ledger.into_inner().unwrap();
    sent.sort_by(|left, right| left.marker.cmp(&right.marker));
    sent
}

// ---------------------------------------------------------------------------------------------
// Detectors
// ---------------------------------------------------------------------------------------------

/// One broken invariant.
#[derive(Debug, Clone)]
struct Violation {
    invariant: &'static str,
    kind: &'static str,
    subject: String,
}

impl Violation {
    fn to_json(&self, phase: &str) -> Value {
        json!({
            "phase": phase,
            "invariant": self.invariant,
            "kind": self.kind,
            "subject": self.subject,
        })
    }
}

fn violation(invariant: &'static str, kind: &'static str, subject: String) -> Violation {
    Violation {
        invariant,
        kind,
        subject,
    }
}

fn has(violations: &[Violation], kind: &str, needle: &str) -> bool {
    violations
        .iter()
        .any(|found| found.kind == kind && found.subject.contains(needle))
}

/// What a session directory scan found.
#[derive(Debug, Default)]
struct SpoolScan {
    sessions: usize,
    files: u64,
    bytes: u64,
    largest: u64,
    temporaries: Vec<String>,
    quarantined: u64,
    torn: Vec<String>,
    segments: Vec<Scanned>,
}

/// A named segment that passed its own verification.
#[derive(Debug)]
struct Scanned {
    path: PathBuf,
    session: String,
    marker: Option<String>,
    checksum: String,
    length: u64,
    session_end: bool,
}

/// Reads every segment file directly.
///
/// `store recover` moves failed segments into quarantine as it reads, so the harness never calls
/// it to look: it parses and verifies each file itself and leaves the directory untouched.
fn scan_spool(home: &Path) -> SpoolScan {
    let mut scan = SpoolScan::default();
    for directory in sorted_entries(&home.join(SEGMENTS)) {
        let session = name_of(&directory);
        if session == "quarantine" {
            scan.quarantined += u64::try_from(sorted_entries(&directory).len()).unwrap();
            continue;
        }
        if session.starts_with('.') || !directory.is_dir() {
            continue;
        }
        scan.sessions += 1;
        for path in sorted_entries(&directory) {
            let file_name = name_of(&path);
            let bytes = fs::symlink_metadata(&path).map_or(0, |metadata| metadata.len());
            scan.files += 1;
            scan.bytes += bytes;
            scan.largest = scan.largest.max(bytes);
            if file_name.starts_with(".tmp-") {
                scan.temporaries.push(relative(home, &path));
                continue;
            }
            if file_name.starts_with('.') {
                continue;
            }
            let verdict = fs::read(&path)
                .ok()
                .and_then(|raw| serde_json::from_slice::<Segment>(&raw).ok())
                .ok_or("segment_unreadable")
                .and_then(|segment| {
                    segment.verify()?;
                    if segment.session_key == session {
                        Ok(segment)
                    } else {
                        Err("segment_session_mismatch")
                    }
                });
            match verdict {
                Ok(segment) => scan.segments.push(Scanned {
                    marker: segment.payload.extensions.get("bench_event").cloned(),
                    checksum: segment.checksum.to_string(),
                    length: segment.length,
                    session_end: segment.payload.kind == HookEventKind::SessionEnd,
                    session: session.clone(),
                    path,
                }),
                Err(reason) => scan
                    .torn
                    .push(format!("{} ({reason})", relative(home, &path))),
            }
        }
    }
    scan
}

/// Byte offsets of every canary prefix in `bytes`.
fn canary_offsets(bytes: &[u8]) -> Vec<usize> {
    bytes
        .windows(CANARY.len())
        .enumerate()
        .filter_map(|(offset, window)| (window == CANARY).then_some(offset))
        .collect()
}

/// Every file under `root` holding a canary, as `label/relative path` plus offset. Never bytes.
fn scan_secrets(root: &Path, label: &str) -> Vec<String> {
    let mut hits = Vec::new();
    let mut pending = vec![root.to_path_buf()];
    while let Some(path) = pending.pop() {
        let Ok(metadata) = fs::symlink_metadata(&path) else {
            continue;
        };
        if metadata.is_dir() {
            pending.extend(sorted_entries(&path));
        } else if metadata.is_file() {
            let offsets = canary_offsets(&fs::read(&path).unwrap_or_default());
            if let Some(first) = offsets.first() {
                hits.push(format!(
                    "{label}/{} at offset {first} ({} occurrence(s))",
                    relative(root, &path),
                    offsets.len()
                ));
            }
        }
    }
    hits.sort();
    hits
}

/// S over a home plus the user directory and the repository the sessions named.
fn secret_violations(bench: &Bench, home: &Path, label: &str) -> Vec<Violation> {
    [
        (home, label),
        (bench.user.as_path(), "user"),
        (bench.repository.as_path(), "repo"),
    ]
    .into_iter()
    .flat_map(|(root, name)| scan_secrets(root, name))
    .map(|hit| violation("S", "home_canary", hit))
    .collect()
}

/// Recorder fault codes in the health spool, counted.
fn health_codes(home: &Path) -> BTreeMap<String, u64> {
    let mut codes = BTreeMap::new();
    for path in sorted_entries(&home.join("cache/spool")) {
        let name = name_of(&path);
        let jsonl = path
            .extension()
            .is_some_and(|extension| extension == "jsonl");
        if !(jsonl && name.starts_with("health")) {
            continue;
        }
        let text = fs::read_to_string(&path).unwrap_or_default();
        for line in text.lines().filter(|line| !line.trim().is_empty()) {
            let code = serde_json::from_str::<Value>(line)
                .ok()
                .and_then(|value| value["code"].as_str().map(str::to_owned))
                .unwrap_or_else(|| "unparseable".to_owned());
            *codes.entry(code).or_insert(0) += 1;
        }
    }
    codes
}

/// L1, L2, L4, and printed canaries, for everything the ledger sent into one home.
fn check_spool(sent: &[Sent], scan: &SpoolScan, health: &BTreeMap<String, u64>) -> Vec<Violation> {
    let mut violations = Vec::new();
    let mut located: BTreeMap<&str, Vec<&str>> = BTreeMap::new();
    for segment in &scan.segments {
        if let Some(marker) = &segment.marker {
            located
                .entry(marker.as_str())
                .or_default()
                .push(segment.session.as_str());
        } else {
            violations.push(violation(
                "L4",
                "phantom",
                format!("{}/{}", segment.session, segment.checksum),
            ));
        }
    }

    let ledger: BTreeSet<&str> = sent.iter().map(|record| record.marker.as_str()).collect();
    for record in sent {
        let sessions = located
            .get(record.marker.as_str())
            .map_or(&[][..], Vec::as_slice);
        if record.outcome.clean() {
            match sessions {
                [] => violations.push(violation("L1", "missing", record.marker.clone())),
                [session] if *session != record.session => violations.push(violation(
                    "L1",
                    "misplaced",
                    format!("{} in {session}", record.marker),
                )),
                [_] => {}
                _ => violations.push(violation(
                    "L1",
                    "duplicated",
                    format!("{} in {} segments", record.marker, sessions.len()),
                )),
            }
        } else if !(record.kill_planned && record.outcome.signal == Some(SIGKILL)) {
            violations.push(violation(
                "L2",
                "fault",
                format!("{}: {}", record.marker, record.outcome.describe()),
            ));
        }
        if record.outcome.output_has_canary {
            violations.push(violation("S", "output_canary", record.marker.clone()));
        }
    }

    for marker in located.keys() {
        if !ledger.contains(marker) {
            violations.push(violation("L4", "phantom", (*marker).to_owned()));
        }
    }
    for torn in &scan.torn {
        violations.push(violation("L4", "torn", torn.clone()));
    }
    if !health.is_empty() {
        violations.push(violation("L2", "health", format!("{health:?}")));
    }
    violations
}

/// A session the spool shows as complete, with the checksums the store must end up holding.
#[derive(Debug, Clone)]
struct Expected {
    session: String,
    digests: Vec<String>,
}

fn complete_sessions(scan: &SpoolScan) -> Vec<Expected> {
    let mut sessions: BTreeMap<&str, (bool, Vec<String>)> = BTreeMap::new();
    for segment in &scan.segments {
        let (complete, digests) = sessions.entry(segment.session.as_str()).or_default();
        *complete |= segment.session_end;
        digests.push(segment.checksum.clone());
    }
    sessions
        .into_iter()
        .filter(|(_, (complete, _))| *complete)
        .map(|(session, (_, digests))| Expected {
            session: session.to_owned(),
            digests,
        })
        .collect()
}

/// One `store` subcommand run.
struct StoreRun {
    outcome: Outcome,
    report: Option<Value>,
}

fn run_store(
    bench: &Bench,
    home: &Path,
    subcommand: &str,
    kill_after: Option<Duration>,
) -> StoreRun {
    let mut command = bench.command(home);
    command.args(["store", subcommand, "--json"]);
    let (outcome, stdout) = run_child(command, None, kill_after);
    StoreRun {
        report: serde_json::from_slice(&stdout).ok(),
        outcome,
    }
}

fn output_canary(run: &StoreRun, label: &str) -> Option<Violation> {
    run.outcome
        .output_has_canary
        .then(|| violation("S", "output_canary", label.to_owned()))
}

fn check_migrate(run: &StoreRun) -> Vec<Violation> {
    let mut violations: Vec<Violation> = output_canary(run, "store migrate").into_iter().collect();
    if run.outcome.exit_code != Some(0) || run.report.is_none() {
        violations.push(violation("I", "migrate_failed", run.outcome.describe()));
    }
    violations
}

/// L5 and L4 for a recover that ran to completion. `tolerated` names sessions whose skip is
/// reported rather than treated as loss.
fn check_recover(run: &StoreRun, tolerated: &BTreeSet<String>) -> Vec<Violation> {
    let mut violations: Vec<Violation> = output_canary(run, "store recover").into_iter().collect();
    let (Some(report), Some(0)) = (&run.report, run.outcome.exit_code) else {
        violations.push(violation("L5", "recover_failed", run.outcome.describe()));
        return violations;
    };
    for skipped in report["skipped"].as_array().into_iter().flatten() {
        let session = skipped["session"].as_str().unwrap_or_default();
        if !tolerated.contains(session) {
            violations.push(violation(
                "L5",
                "skipped",
                format!("{session}: {}", skipped["code"]),
            ));
        }
    }
    let quarantined = report["quarantined_segments"].as_u64().unwrap_or(0);
    if quarantined > 0 {
        violations.push(violation(
            "L4",
            "quarantined",
            format!("{quarantined} segment(s)"),
        ));
    }
    violations
}

/// Opens the bench store for reading. A WAL database needs its shared-memory file, so the
/// connection opens read-write and then refuses every write.
fn read_database(home: &Path) -> Connection {
    let connection = write_database(home);
    connection.execute_batch("PRAGMA query_only = ON").unwrap();
    connection
}

/// Opens the bench store for a planted fault. Never used on the operator's store.
fn write_database(home: &Path) -> Connection {
    Connection::open_with_flags(
        home.join(DATABASE),
        OpenFlags::SQLITE_OPEN_READ_WRITE | OpenFlags::SQLITE_OPEN_NO_MUTEX,
    )
    .unwrap()
}

fn execute(home: &Path, sql: &str) {
    write_database(home).execute_batch(sql).unwrap();
}

fn count(connection: &Connection, sql: &str) -> u64 {
    let value: i64 = connection.query_row(sql, [], |row| row.get(0)).unwrap();
    u64::try_from(value).unwrap()
}

fn strings(connection: &Connection, sql: &str) -> BTreeSet<String> {
    let mut statement = connection.prepare(sql).unwrap();
    statement
        .query_map([], |row| row.get::<_, String>(0))
        .unwrap()
        .map(Result::unwrap)
        .collect()
}

/// Rows of `(text, integer)`, keyed by the text.
fn pairs(connection: &Connection, sql: &str) -> BTreeMap<String, u64> {
    let mut statement = connection.prepare(sql).unwrap();
    statement
        .query_map([], |row| {
            Ok((row.get::<_, String>(0)?, row.get::<_, i64>(1)?))
        })
        .unwrap()
        .map(|row| {
            let (key, value) = row.unwrap();
            (key, u64::try_from(value).unwrap())
        })
        .collect()
}

const SESSIONS_WITHOUT_TRAJECTORY: &str = "SELECT count(*) FROM sessions s \
     WHERE NOT EXISTS (SELECT 1 FROM trajectories t WHERE t.session_id = s.session_id)";
const TRAJECTORIES_WITHOUT_METRICS: &str = "SELECT count(*) FROM trajectories t \
     WHERE NOT EXISTS (SELECT 1 FROM trace_metrics m WHERE m.trajectory_id = t.trajectory_id)";
const FINALIZED_DIGESTS: &str = "SELECT DISTINCT e.payload_digest FROM events e \
     JOIN trajectories t ON t.session_id = e.session_id WHERE e.payload_digest IS NOT NULL";

/// Stored events grouped by the spool session their checksum came from, then by stored session.
struct StoredEvents<'a> {
    by_spool_session: BTreeMap<&'a str, BTreeMap<String, Vec<String>>>,
    unknown: Vec<String>,
}

fn stored_events<'a>(
    connection: &Connection,
    owners: &BTreeMap<&str, &'a str>,
) -> StoredEvents<'a> {
    let mut stored = StoredEvents {
        by_spool_session: BTreeMap::new(),
        unknown: Vec::new(),
    };
    let mut statement = connection
        .prepare("SELECT session_id, payload_digest FROM events ORDER BY session_id, sequence")
        .unwrap();
    let rows = statement
        .query_map([], |row| {
            Ok((row.get::<_, String>(0)?, row.get::<_, Option<String>>(1)?))
        })
        .unwrap();
    for row in rows {
        let (session_id, digest) = row.unwrap();
        let digest = digest.unwrap_or_default();
        let owner = owners.get(digest.as_str()).copied();
        if let Some(owner) = owner {
            stored
                .by_spool_session
                .entry(owner)
                .or_default()
                .entry(session_id)
                .or_default()
                .push(digest);
        } else {
            stored.unknown.push(format!("{session_id}/{digest}"));
        }
    }
    stored
}

/// L3 for each expected session. Returns how many stored sessions have no metrics row.
fn compare_sessions(
    expected: &[Expected],
    stored: &StoredEvents<'_>,
    trajectories: &BTreeSet<String>,
    metrics: &BTreeMap<String, u64>,
    violations: &mut Vec<Violation>,
) -> u64 {
    let mut missing_metrics = 0;
    for session in expected {
        let by_id = stored.by_spool_session.get(session.session.as_str());
        let mut copies: BTreeMap<&str, usize> = BTreeMap::new();
        for digest in by_id.into_iter().flat_map(BTreeMap::values).flatten() {
            *copies.entry(digest.as_str()).or_default() += 1;
        }
        for digest in &session.digests {
            match copies.get(digest.as_str()).copied().unwrap_or(0) {
                0 => violations.push(violation(
                    "L3",
                    "missing_in_store",
                    format!("{}/{digest}", session.session),
                )),
                1 => {}
                many => violations.push(violation(
                    "L3",
                    "duplicated_in_store",
                    format!("{}/{digest} stored {many} times", session.session),
                )),
            }
        }

        let ids: Vec<&String> = by_id.into_iter().flat_map(BTreeMap::keys).collect();
        if ids.len() > 1 {
            violations.push(violation(
                "L3",
                "split_session",
                format!("{} across {} stored sessions", session.session, ids.len()),
            ));
        }
        if !ids.iter().any(|id| trajectories.contains(*id)) {
            violations.push(violation("L3", "no_trajectory", session.session.clone()));
        }
        let expected_count = u64::try_from(session.digests.len()).unwrap();
        for id in ids {
            match metrics.get(id) {
                None => missing_metrics += 1,
                Some(&stored) if stored != expected_count => violations.push(violation(
                    "L3",
                    "event_count_mismatch",
                    format!(
                        "{}: metrics {stored}, spool {expected_count}",
                        session.session
                    ),
                )),
                Some(_) => {}
            }
        }
    }
    missing_metrics
}

/// What the store held after recovery, and what it failed to hold.
struct StoreCheck {
    violations: Vec<Violation>,
    facts: Value,
}

/// Which expected session each checksum belongs to.
fn digest_owners(expected: &[Expected]) -> BTreeMap<&str, &str> {
    expected
        .iter()
        .flat_map(|session| {
            session
                .digests
                .iter()
                .map(move |digest| (digest.as_str(), session.session.as_str()))
        })
        .collect()
}

/// L3 and the L4 metrics counters, read straight from the database.
fn check_store(home: &Path, expected: &[Expected]) -> StoreCheck {
    let connection = read_database(home);
    let owners = digest_owners(expected);
    let stored = stored_events(&connection, &owners);
    let trajectories = strings(&connection, "SELECT session_id FROM trajectories");
    let metrics = pairs(
        &connection,
        "SELECT session_id, event_count FROM trace_metrics",
    );

    let mut violations: Vec<Violation> = stored
        .unknown
        .iter()
        .map(|event| violation("L3", "unknown_event", event.clone()))
        .collect();
    let missing_metrics =
        compare_sessions(expected, &stored, &trajectories, &metrics, &mut violations);

    let mut owners_by_id: BTreeMap<&str, BTreeSet<&str>> = BTreeMap::new();
    for (owner, ids) in &stored.by_spool_session {
        for id in ids.keys() {
            owners_by_id.entry(id.as_str()).or_default().insert(*owner);
        }
    }
    for (id, spool_sessions) in owners_by_id {
        if spool_sessions.len() > 1 {
            violations.push(violation(
                "L3",
                "mixed_session",
                format!("{id} holds {spool_sessions:?}"),
            ));
        }
    }

    let quarantined = count(
        &connection,
        "SELECT coalesce(sum(quarantined_segments), 0) FROM trace_metrics",
    );
    if quarantined > 0 {
        violations.push(violation(
            "L4",
            "metrics_quarantined",
            format!("{quarantined} segment(s)"),
        ));
    }
    let duplicates = count(
        &connection,
        "SELECT coalesce(sum(duplicates_collapsed), 0) FROM trace_metrics",
    );
    if duplicates > 0 {
        violations.push(violation(
            "L4",
            "metrics_duplicates",
            format!("{duplicates} event(s)"),
        ));
    }

    let facts = json!({
        "expected_sessions": expected.len(),
        "sessions": count(&connection, "SELECT count(*) FROM sessions"),
        "events": count(&connection, "SELECT count(*) FROM events"),
        "trajectories": count(&connection, "SELECT count(*) FROM trajectories"),
        "trace_metrics": count(&connection, "SELECT count(*) FROM trace_metrics"),
        "missing_metrics": missing_metrics,
        "sessions_without_trajectory": count(&connection, SESSIONS_WITHOUT_TRAJECTORY),
        "trajectories_without_metrics": count(&connection, TRAJECTORIES_WITHOUT_METRICS),
        "health_events": pairs(
            &connection,
            "SELECT code, count(*) FROM health_events GROUP BY code ORDER BY code",
        ),
    });
    StoreCheck { violations, facts }
}

/// I: `store check` reports a healthy store, and says so without failing.
fn check_integrity(bench: &Bench, home: &Path) -> (Vec<Violation>, Value) {
    let run = run_store(bench, home, "check", None);
    let mut violations: Vec<Violation> = output_canary(&run, "store check").into_iter().collect();
    if let (Some(report), Some(0)) = (&run.report, run.outcome.exit_code) {
        if report["healthy"].as_bool() != Some(true) {
            let integrity: String = report["integrity"].to_string().chars().take(200).collect();
            violations.push(violation(
                "I",
                "unhealthy",
                format!(
                    "integrity {integrity}, foreign key violations {}, schema {}",
                    report["foreign_key_violations"], report["schema_version"]
                ),
            ));
        }
        (violations, report.clone())
    } else {
        violations.push(violation("I", "check_failed", run.outcome.describe()));
        (violations, Value::Null)
    }
}

// ---------------------------------------------------------------------------------------------
// Negative controls
// ---------------------------------------------------------------------------------------------

/// One check inside a negative control.
struct ControlCheck {
    control: &'static str,
    check: &'static str,
    detected: bool,
}

const fn control(control: &'static str, check: &'static str, detected: bool) -> ControlCheck {
    ControlCheck {
        control,
        check,
        detected,
    }
}

/// What the controls detected, plus anything wrong where nothing was planted.
struct Controls {
    checks: Vec<ControlCheck>,
    baseline: Vec<Violation>,
    store: Value,
}

impl Controls {
    /// Controls with at least one check that went undetected.
    fn undetected(&self) -> Vec<&'static str> {
        let mut detected: BTreeMap<&'static str, bool> = BTreeMap::new();
        for check in &self.checks {
            *detected.entry(check.control).or_insert(true) &= check.detected;
        }
        detected
            .into_iter()
            .filter(|(_, detected)| !detected)
            .map(|(control, _)| control)
            .collect()
    }

    fn to_json(&self) -> Value {
        let checks: Vec<Value> = self
            .checks
            .iter()
            .map(|check| {
                json!({
                    "control": check.control,
                    "check": check.check,
                    "detected": check.detected,
                })
            })
            .collect();
        json!({
            "checks": checks,
            "undetected": self.undetected(),
            "baseline_violations": self.baseline.len(),
            "store_after_recover": self.store,
        })
    }
}

/// Plants each known fault in a home of its own and records whether the detectors report it.
fn run_controls(bench: &Bench) -> Controls {
    let home = bench.home("controls");
    let specs = [
        SessionSpec::new("ctl-a", Runtime::Claude, true),
        SessionSpec::new("ctl-b", Runtime::Claude, false),
    ];
    let mut sent = ingest_sessions(bench, &home, &specs, 1, &never_kill);

    // Before anything is planted, the detectors must find nothing.
    let mut baseline = spool_violations(bench, &home, &sent, "controls");

    plant_spool_faults(bench, &home, &mut sent);
    let found = spool_violations(bench, &home, &sent, "controls");
    let mut checks = vec![
        control(
            "C1",
            "canary planted beside the spool",
            has(&found, "home_canary", "bench-planted.txt"),
        ),
        control(
            "C2",
            "acknowledged segment deleted",
            has(&found, "missing", "ctl-b-e2"),
        ),
        control(
            "C2",
            "segment the ledger never sent",
            has(&found, "phantom", "ctl-a-phantom"),
        ),
        control(
            "C4",
            "torn file under a segment name",
            has(&found, "torn", "-torn.json"),
        ),
        control(
            "C5",
            "malformed payload reported on stderr",
            has(&found, "fault", "ctl-wrong-types"),
        ),
        control(
            "C5",
            "malformed payload counted in the health spool",
            has(&found, "health", ""),
        ),
    ];

    let expected = complete_sessions(&scan_spool(&home));
    baseline.extend(check_migrate(&run_store(bench, &home, "migrate", None)));
    let recovered = check_recover(&run_store(bench, &home, "recover", None), &BTreeSet::new());
    checks.push(control(
        "C4",
        "recover quarantines the torn file",
        has(&recovered, "quarantined", ""),
    ));
    baseline.extend(
        recovered
            .into_iter()
            .filter(|found| found.kind != "quarantined"),
    );
    let store = check_store(&home, &expected);
    checks.push(control(
        "C4",
        "trace metrics count the quarantined file",
        has(&store.violations, "metrics_quarantined", ""),
    ));
    baseline.extend(
        store
            .violations
            .into_iter()
            .filter(|found| found.kind != "metrics_quarantined"),
    );
    baseline.extend(check_integrity(bench, &home).0);

    checks.extend(store_controls(bench, &home, &expected, &mut baseline));
    Controls {
        checks,
        baseline,
        store: store.facts,
    }
}

/// Spool invariants plus a secret scan, for one home.
fn spool_violations(bench: &Bench, home: &Path, sent: &[Sent], label: &str) -> Vec<Violation> {
    let mut violations = check_spool(sent, &scan_spool(home), &health_codes(home));
    violations.extend(secret_violations(bench, home, label));
    violations
}

/// Plants the spool-side faults: a canary, a lost segment, a phantom, a torn file, and a fault.
fn plant_spool_faults(bench: &Bench, home: &Path, sent: &mut Vec<Sent>) {
    // C1: a canary beside the spool, where a leaking logger would put it.
    fs::write(
        home.join("cache/logs/bench-planted.txt"),
        "CANARY-planted-control\n",
    )
    .unwrap();

    // C2: an acknowledged segment disappears.
    let scan = scan_spool(home);
    let lost = scan
        .segments
        .iter()
        .find(|segment| segment.marker.as_deref() == Some("ctl-b-e2"))
        .unwrap();
    fs::remove_file(&lost.path).unwrap();

    // C2: a segment the ledger never sent.
    let phantom = SessionSpec::new("ctl-a", Runtime::Claude, false);
    let document = payload(bench, &phantom, 2, "ctl-a-phantom");
    assert!(send(bench, home, phantom.runtime, SCRIPT[2], &document, None).clean());

    // C4: a fragment under a segment name.
    fs::write(
        home.join(SEGMENTS)
            .join("ctl-a/00000000000000000001-torn.json"),
        r#"{"segment_version":1,"pay"#,
    )
    .unwrap();

    // C5: a malformed payload, sent and recorded like any other ingest.
    let malformed = bench.fixture(Runtime::Claude, "wrong-types").clone();
    let outcome = send(
        bench,
        home,
        Runtime::Claude,
        HookEventKind::UserPromptSubmit,
        &malformed,
        None,
    );
    sent.push(Sent {
        session: "ctl-wrong-types".to_owned(),
        marker: "ctl-wrong-types".to_owned(),
        kind: HookEventKind::UserPromptSubmit,
        kill_planned: false,
        outcome,
    });
}

/// Plants faults in the recovered store, one at a time.
fn store_controls(
    bench: &Bench,
    home: &Path,
    expected: &[Expected],
    baseline: &mut Vec<Violation>,
) -> Vec<ControlCheck> {
    let mut checks = Vec::new();

    // C3: a stored event disappears.
    execute(
        home,
        "DELETE FROM events WHERE rowid = (SELECT min(rowid) FROM events)",
    );
    checks.push(control(
        "C3",
        "stored event deleted",
        has(
            &check_store(home, expected).violations,
            "missing_in_store",
            "",
        ),
    ));

    // C1: a canary reaches the database.
    execute(
        home,
        "UPDATE trajectories SET intent = intent || ' CANARY-intent-control' \
         WHERE rowid = (SELECT min(rowid) FROM trajectories)",
    );
    checks.push(control(
        "C1",
        "canary written into the store",
        scan_secrets(home, "controls")
            .iter()
            .any(|hit| hit.starts_with("controls/data/")),
    ));

    // C6: a row that names a repository the store does not have.
    execute(
        home,
        "PRAGMA foreign_keys = OFF; \
         INSERT INTO sessions (session_id, repo_id, worktree_root, head_commit, runtime, \
         runtime_version, model_id, started_at_unix_ms, ended_at_unix_ms, record_json, digest, \
         written_at) VALUES ('bench-orphan-session', 'bench-missing-repository', '/', 'bench', \
         'claude', '0', 'bench', 0, NULL, '{}', 'bench', 0);",
    );
    checks.push(control(
        "C6",
        "session row naming a missing repository",
        has(&check_integrity(bench, home).0, "unhealthy", ""),
    ));
    execute(
        home,
        "DELETE FROM sessions WHERE session_id = 'bench-orphan-session'",
    );
    baseline.extend(check_integrity(bench, home).0);

    // C6: bytes overwritten inside the checkpointed database.
    corrupt_events_root_page(home);
    let corrupted = check_integrity(bench, home).0;
    checks.push(control(
        "C6",
        "database page overwritten",
        has(&corrupted, "unhealthy", "") || has(&corrupted, "check_failed", ""),
    ));
    checks
}

/// Overwrites the start of the events table's root page with bytes no page header may hold.
fn corrupt_events_root_page(home: &Path) {
    let (page_size, root_page) = {
        let connection = write_database(home);
        let _: (i64, i64, i64) = connection
            .query_row("PRAGMA wal_checkpoint(TRUNCATE)", [], |row| {
                Ok((row.get(0)?, row.get(1)?, row.get(2)?))
            })
            .unwrap();
        let page_size = count(&connection, "PRAGMA page_size");
        let root_page = count(
            &connection,
            "SELECT rootpage FROM sqlite_schema WHERE type = 'table' AND name = 'events'",
        );
        (page_size, root_page)
    };
    let file = OpenOptions::new()
        .write(true)
        .open(home.join(DATABASE))
        .unwrap();
    file.write_all_at(&[0xFF; 16], (root_page - 1) * page_size)
        .unwrap();
    file.sync_all().unwrap();
}

// ---------------------------------------------------------------------------------------------
// Phases
// ---------------------------------------------------------------------------------------------

/// One measured phase: its report section and what it found wrong.
struct PhaseResult {
    name: &'static str,
    report: Value,
    violations: Vec<Violation>,
}

/// Claude sessions, one in five carrying the fixture secrets.
fn claude_sessions(prefix: &str, count: usize) -> Vec<SessionSpec> {
    (0..count)
        .map(|index| {
            let key = format!("{prefix}-{index:04}");
            SessionSpec::new(&key, Runtime::Claude, index % 5 == 0)
        })
        .collect()
}

/// Both runtimes, two in five sessions carrying the fixture secrets.
fn mixed_sessions(prefix: &str, count: usize) -> Vec<SessionSpec> {
    (0..count)
        .map(|index| {
            let key = format!("{prefix}-{index:04}");
            match index % 5 {
                0 => SessionSpec::new(&key, Runtime::Claude, true),
                1 => SessionSpec::new(&key, Runtime::Opencode, false),
                2 => SessionSpec::new(&key, Runtime::Opencode, true),
                _ => SessionSpec::new(&key, Runtime::Claude, false),
            }
        })
        .collect()
}

/// Nearest-rank percentiles, extremes, and the mean, in microseconds.
fn latency(samples: &[u64]) -> Value {
    let mut sorted = samples.to_vec();
    sorted.sort_unstable();
    let (Some(&min), Some(&max)) = (sorted.first(), sorted.last()) else {
        return json!({ "n": 0 });
    };
    let rank = |percent: usize| sorted[(percent * sorted.len()).div_ceil(100) - 1];
    let total: u64 = sorted.iter().sum();
    json!({
        "n": sorted.len(),
        "min_us": min,
        "p50_us": rank(50),
        "p95_us": rank(95),
        "p99_us": rank(99),
        "max_us": max,
        "mean_us": total / u64::try_from(sorted.len()).unwrap(),
    })
}

/// Latency of every clean ingest, overall and per event kind.
fn latency_by_kind(sent: &[Sent]) -> Value {
    let mut samples: BTreeMap<&str, Vec<u64>> = BTreeMap::new();
    for record in sent.iter().filter(|record| record.outcome.clean()) {
        samples
            .entry("all")
            .or_default()
            .push(record.outcome.micros);
        samples
            .entry(record.kind.as_str())
            .or_default()
            .push(record.outcome.micros);
    }
    samples
        .into_iter()
        .map(|(kind, micros)| (kind.to_owned(), latency(&micros)))
        .collect::<serde_json::Map<_, _>>()
        .into()
}

fn spool_summary(scan: &SpoolScan) -> Value {
    json!({
        "sessions": scan.sessions,
        "files": scan.files,
        "bytes": scan.bytes,
        "largest_file_bytes": scan.largest,
        "bytes_per_file": scan.bytes.checked_div(scan.files),
        "temporaries": scan.temporaries.len(),
        "quarantined": scan.quarantined,
        "torn": scan.torn.len(),
        "verified_segments": scan.segments.len(),
    })
}

/// Sizes of the database and its sidecars.
fn store_files(home: &Path) -> Value {
    let size = |suffix: &str| {
        fs::metadata(home.join(format!("{DATABASE}{suffix}"))).map_or(0, |metadata| metadata.len())
    };
    json!({
        "database_bytes": size(""),
        "wal_bytes": size("-wal"),
        "shm_bytes": size("-shm"),
    })
}

/// Store size after recovery, as left and after a WAL checkpoint on the bench store.
fn store_growth(home: &Path) -> Value {
    let as_left = store_files(home);
    let connection = write_database(home);
    let trajectories = count(&connection, "SELECT count(*) FROM trajectories");
    let events = count(&connection, "SELECT count(*) FROM events");
    let (busy, wal_frames, checkpointed): (i64, i64, i64) = connection
        .query_row("PRAGMA wal_checkpoint(TRUNCATE)", [], |row| {
            Ok((row.get(0)?, row.get(1)?, row.get(2)?))
        })
        .unwrap();
    let checkpointed_files = store_files(home);
    let database = checkpointed_files["database_bytes"].as_u64().unwrap_or(0);
    json!({
        "as_left_by_recover": as_left,
        "checkpoint": { "busy": busy, "wal_frames": wal_frames, "checkpointed": checkpointed },
        "after_checkpoint": checkpointed_files,
        "trajectories": trajectories,
        "events": events,
        "database_bytes_per_trajectory": database.checked_div(trajectories),
        "database_bytes_per_event": database.checked_div(events),
    })
}

/// A recover report without per-session detail, except the sessions it skipped.
fn recover_summary(run: &StoreRun) -> Value {
    let Some(report) = &run.report else {
        return json!({ "unparseable": run.outcome.describe() });
    };
    let length = |field: &str| report[field].as_array().map_or(0, Vec::len);
    json!({
        "recovered": length("recovered"),
        "pending": length("pending"),
        "skipped": report["skipped"],
        "quarantined_segments": report["quarantined_segments"],
    })
}

/// The sessions a recover report lists under `field`.
fn listed_sessions(run: &StoreRun, field: &str) -> Vec<String> {
    run.report
        .as_ref()
        .and_then(|report| report[field].as_array())
        .into_iter()
        .flatten()
        .filter_map(|entry| entry["session"].as_str().map(str::to_owned))
        .collect()
}

/// Migrates and recovers a home, checks S, L3-L5, and I, and measures the store.
fn recover_and_check(
    bench: &Bench,
    home: &Path,
    label: &str,
    expected: &[Expected],
    violations: &mut Vec<Violation>,
) -> Value {
    let migrate = run_store(bench, home, "migrate", None);
    violations.extend(check_migrate(&migrate));
    let recover = run_store(bench, home, "recover", None);
    violations.extend(check_recover(&recover, &BTreeSet::new()));
    let growth = store_growth(home);
    let store = check_store(home, expected);
    violations.extend(store.violations);
    let (integrity, check) = check_integrity(bench, home);
    violations.extend(integrity);
    violations.extend(secret_violations(bench, home, label));
    for session in expected {
        if home.join(SEGMENTS).join(&session.session).exists() {
            violations.push(violation("L5", "not_drained", session.session.clone()));
        }
    }
    json!({
        "migrate_us": migrate.outcome.micros,
        "recover_us": recover.outcome.micros,
        "recover": recover_summary(&recover),
        "store": store.facts,
        "growth": growth,
        "check": check,
        "spool_after_recover": spool_summary(&scan_spool(home)),
    })
}

/// A few sessions into a throwaway home, so first-run costs stay out of the measured phases.
fn warm_up(bench: &Bench) -> Value {
    let home = bench.home("warm-up");
    let specs = claude_sessions("warm", WARMUP_SESSIONS);
    let sent = ingest_sessions(bench, &home, &specs, 1, &never_kill);
    fs::remove_dir_all(&home).unwrap();
    json!({
        "events": sent.len(),
        "clean": sent.iter().filter(|record| record.outcome.clean()).count(),
    })
}

fn phase_sequential(bench: &Bench) -> PhaseResult {
    let specs = claude_sessions("seq", SEQUENTIAL_SESSIONS);
    phase_throughput(bench, "sequential", &specs, 1)
}

fn phase_burst(bench: &Bench) -> PhaseResult {
    let specs = mixed_sessions("burst", BURST_SESSIONS);
    phase_throughput(bench, "burst", &specs, BURST_WORKERS)
}

/// Complete sessions ingested on `workers` threads, then recovered.
fn phase_throughput(
    bench: &Bench,
    name: &'static str,
    specs: &[SessionSpec],
    workers: usize,
) -> PhaseResult {
    let home = bench.home(name);
    let started = Instant::now();
    let sent = ingest_sessions(bench, &home, specs, workers, &never_kill);
    let wall_us = micros_since(started);

    let scan = scan_spool(&home);
    let mut violations = check_spool(&sent, &scan, &health_codes(&home));
    violations.extend(secret_violations(bench, &home, name));
    let expected = complete_sessions(&scan);
    let recovery = recover_and_check(bench, &home, name, &expected, &mut violations);

    let events = u64::try_from(sent.len()).unwrap();
    let per_second = (events * 1_000_000).checked_div(wall_us);
    let report = json!({
        "headline": format!(
            "{events} events on {workers} worker(s) in {} ms, {} events/s, recover {} ms",
            wall_us / 1_000,
            per_second.unwrap_or(0),
            recovery["recover_us"].as_u64().unwrap_or(0) / 1_000,
        ),
        "sessions": specs.len(),
        "workers": workers,
        "events": events,
        "wall_us": wall_us,
        "events_per_second": per_second,
        "latency": latency_by_kind(&sent),
        "spool_before_recover": spool_summary(&scan),
        "complete_sessions": expected.len(),
        "recovery": recovery,
    });
    PhaseResult {
        name,
        report,
        violations,
    }
}

/// Kills every third ingest, after a delay spread over 60 ms: long enough for some kills to land
/// after the segment is written, and for some ingests to exit before their kill.
fn crash_kill(index: usize) -> Option<Duration> {
    (index % 3 == 2)
        .then(|| Duration::from_micros(u64::try_from((index * 7_919) % 60_000).unwrap()))
}

/// Ingests with kills, then recovery of whatever the spool holds.
///
/// A killed event may or may not have a segment; only the ingests that were not killed are held
/// to L1 and L2. Everything the spool holds must still verify, recover, and be stored.
fn phase_crash_ingest(bench: &Bench) -> PhaseResult {
    let name = "crash_ingest";
    let home = bench.home(name);
    let specs = mixed_sessions("crash", CRASH_INGEST_SESSIONS);
    let sent = ingest_sessions(bench, &home, &specs, CRASH_INGEST_WORKERS, &crash_kill);

    let scan = scan_spool(&home);
    let mut violations = check_spool(&sent, &scan, &health_codes(&home));
    violations.extend(secret_violations(bench, &home, name));
    let expected = complete_sessions(&scan);

    let written: BTreeSet<&str> = scan
        .segments
        .iter()
        .filter_map(|segment| segment.marker.as_deref())
        .collect();
    let planned = sent.iter().filter(|record| record.kill_planned).count();
    let killed: Vec<&Sent> = sent
        .iter()
        .filter(|record| record.kill_planned && record.outcome.signal == Some(SIGKILL))
        .collect();
    let killed_after_write = killed
        .iter()
        .filter(|record| written.contains(record.marker.as_str()))
        .count();
    let unkilled: Vec<Sent> = sent
        .iter()
        .filter(|record| !record.kill_planned)
        .cloned()
        .collect();

    let recovery = recover_and_check(bench, &home, name, &expected, &mut violations);
    let report = json!({
        "headline": format!(
            "{} of {planned} planned kills landed, {killed_after_write} after the segment was \
             written; {} of {} sessions complete",
            killed.len(),
            expected.len(),
            specs.len(),
        ),
        "sessions": specs.len(),
        "workers": CRASH_INGEST_WORKERS,
        "events": sent.len(),
        "kills": {
            "planned": planned,
            "landed": killed.len(),
            "process_exited_first": planned - killed.len(),
            "landed_after_segment_written": killed_after_write,
            "landed_before_segment_written": killed.len() - killed_after_write,
        },
        "latency_without_kill": latency_by_kind(&unkilled),
        "spool_before_recover": spool_summary(&scan),
        "temporaries_before_recover": scan.temporaries,
        "complete_sessions": expected.len(),
        "incomplete_sessions": scan.sessions - expected.len(),
        "recovery": recovery,
        "temporaries_after_recover": scan_spool(&home).temporaries,
    });
    PhaseResult {
        name,
        report,
        violations,
    }
}

/// A recover attempt's kill delay.
fn recover_kill(attempt: usize) -> Duration {
    Duration::from_millis(u64::try_from(5 + (attempt * 37) % 300).unwrap())
}

/// Spool sessions with an event in a stored trajectory, matched by checksum.
fn finalized_sessions(home: &Path, expected: &[Expected]) -> BTreeSet<String> {
    let owners = digest_owners(expected);
    strings(&read_database(home), FINALIZED_DIGESTS)
        .iter()
        .filter_map(|digest| owners.get(digest.as_str()))
        .map(|session| (*session).to_owned())
        .collect()
}

/// What a killed recover can leave half done, read without writing.
fn torn_state(home: &Path, finalized: &BTreeSet<String>) -> Value {
    let connection = read_database(home);
    let still_spooled = finalized
        .iter()
        .filter(|session| home.join(SEGMENTS).join(session).exists())
        .count();
    json!({
        "trajectories": count(&connection, "SELECT count(*) FROM trajectories"),
        "sessions_without_trajectory": count(&connection, SESSIONS_WITHOUT_TRAJECTORY),
        "trajectories_without_metrics": count(&connection, TRAJECTORIES_WITHOUT_METRICS),
        "finalized_still_spooled": still_spooled,
    })
}

/// `store recover` killed once per attempt, with I and the torn state checked after each.
fn killed_recovers(
    bench: &Bench,
    home: &Path,
    expected: &[Expected],
    violations: &mut Vec<Violation>,
) -> Vec<Value> {
    let mut attempts = Vec::with_capacity(CRASH_RECOVER_ATTEMPTS);
    for attempt in 0..CRASH_RECOVER_ATTEMPTS {
        let delay = recover_kill(attempt);
        let run = run_store(bench, home, "recover", Some(delay));
        let killed = run.outcome.signal == Some(SIGKILL);
        let finalized = finalized_sessions(home, expected);
        // A killed attempt reports nothing to judge; one that finished before its kill is judged
        // like any other recover.
        let mut found: Vec<Violation> = if killed {
            output_canary(&run, "store recover").into_iter().collect()
        } else {
            check_recover(&run, &finalized)
        };
        found.extend(check_integrity(bench, home).0);
        violations.extend(found.into_iter().map(|mut found| {
            found.subject = format!("attempt {attempt}: {}", found.subject);
            found
        }));
        attempts.push(json!({
            "attempt": attempt,
            "kill_after_ms": u64::try_from(delay.as_millis()).unwrap(),
            "killed": killed,
            "micros": run.outcome.micros,
            "recover": if killed { Value::Null } else { recover_summary(&run) },
            "torn_state": torn_state(home, &finalized),
        }));
    }
    attempts
}

/// Complete sessions recovered by a `store recover` that is killed over and over.
///
/// Ingest never opens the database, so killing `store recover` is the only way to interrupt a
/// store write. The store must stay healthy after every killed attempt, and one uninterrupted
/// recover must then hold every session exactly once. Spool left behind by a session that is
/// already stored is reported, not treated as loss.
fn phase_crash_recover(bench: &Bench) -> PhaseResult {
    let name = "crash_recover";
    let home = bench.home(name);
    let specs = mixed_sessions("recover", CRASH_RECOVER_SESSIONS);
    let sent = ingest_sessions(bench, &home, &specs, BURST_WORKERS, &never_kill);
    let scan = scan_spool(&home);
    let mut violations = check_spool(&sent, &scan, &health_codes(&home));
    violations.extend(secret_violations(bench, &home, name));
    let expected = complete_sessions(&scan);

    let migrate = run_store(bench, &home, "migrate", None);
    violations.extend(check_migrate(&migrate));
    let attempts = killed_recovers(bench, &home, &expected, &mut violations);
    let killed = attempts
        .iter()
        .filter(|attempt| attempt["killed"] == true)
        .count();
    let stored_before = finalized_sessions(&home, &expected).len();

    let recover = run_store(bench, &home, "recover", None);
    violations.extend(check_recover(
        &recover,
        &finalized_sessions(&home, &expected),
    ));
    let store = check_store(&home, &expected);
    violations.extend(store.violations);
    let (integrity, check) = check_integrity(bench, &home);
    violations.extend(integrity);
    violations.extend(secret_violations(bench, &home, name));
    let stale: Vec<&str> = expected
        .iter()
        .map(|session| session.session.as_str())
        .filter(|session| home.join(SEGMENTS).join(session).exists())
        .collect();

    let report = json!({
        "headline": format!(
            "{killed} of {CRASH_RECOVER_ATTEMPTS} attempts killed, {stored_before} of {} sessions \
             stored before the uninterrupted recover, {} stale spool session(s)",
            expected.len(),
            stale.len(),
        ),
        "sessions": specs.len(),
        "workers": BURST_WORKERS,
        "events": sent.len(),
        "complete_sessions": expected.len(),
        "migrate_us": migrate.outcome.micros,
        "killed_attempts": killed,
        "attempts": attempts,
        "stored_before_final_recover": stored_before,
        "final_recover_us": recover.outcome.micros,
        "final_recover": recover_summary(&recover),
        "pending_after_final_recover": listed_sessions(&recover, "pending"),
        "store": store.facts,
        "check": check,
        "stale_spool_sessions": stale,
        "spool_after_recover": spool_summary(&scan_spool(&home)),
    });
    PhaseResult {
        name,
        report,
        violations,
    }
}

/// Benign text of exactly `bytes` bytes.
fn filler(bytes: usize) -> String {
    "bench filler text ".chars().cycle().take(bytes).collect()
}

/// A `post-tool-use` payload whose command and output each hold `OVERSIZE_FIELD_BYTES` of filler.
fn oversize_payload(bench: &Bench, spec: &SessionSpec, marker: &str) -> Value {
    let mut document = bench
        .fixture(spec.runtime, HookEventKind::PostToolUse.as_str())
        .clone();
    let text = filler(OVERSIZE_FIELD_BYTES);
    document["tool_input"]["command"] = json!(text);
    document["tool_response"][spec.runtime.output_field()] = json!(text);
    locate(bench, spec, &mut document, marker);
    document
}

/// Sends one session in order: start, prompt, the large tool events, stop, end.
fn ingest_oversize(bench: &Bench, home: &Path, spec: &SessionSpec) -> Vec<Sent> {
    let mut sent = Vec::with_capacity(OVERSIZE_EVENTS + 4);
    let mut deliver = |marker: String, kind: HookEventKind, document: &Value| {
        let outcome = send(bench, home, spec.runtime, kind, document, None);
        sent.push(Sent {
            session: spec.key.clone(),
            marker,
            kind,
            kill_planned: false,
            outcome,
        });
    };
    for step in [0, 1] {
        let marker = format!("{}-e{step}", spec.key);
        deliver(
            marker.clone(),
            SCRIPT[step],
            &payload(bench, spec, step, &marker),
        );
    }
    for index in 0..OVERSIZE_EVENTS {
        let marker = format!("{}-o{index:02}", spec.key);
        let document = oversize_payload(bench, spec, &marker);
        deliver(marker, HookEventKind::PostToolUse, &document);
    }
    for step in [6, 7] {
        let marker = format!("{}-e{step}", spec.key);
        deliver(
            marker.clone(),
            SCRIPT[step],
            &payload(bench, spec, step, &marker),
        );
    }
    sent
}

/// The code a recover report gives for skipping `session`, or `None` when it did not skip it.
fn skip_code(run: &StoreRun, session: &str) -> Option<String> {
    run.report.as_ref()?["skipped"]
        .as_array()?
        .iter()
        .find(|entry| entry["session"] == session)
        .and_then(|entry| entry["code"].as_str().map(str::to_owned))
}

/// Report-only probe of one session over the trajectory ceiling.
///
/// Each large field is cut to the field cap, and together the session retains more than the
/// ceiling. What recover does with it is recorded, not asserted. The probe is still held to S,
/// L1, L2, L4, and I, and the store may hold none of the session unless recover stored all of it.
fn phase_oversize(bench: &Bench) -> PhaseResult {
    let name = "oversize";
    let home = bench.home(name);
    let spec = SessionSpec::new("big-0000", Runtime::Claude, false);
    let sent = ingest_oversize(bench, &home, &spec);
    let scan = scan_spool(&home);
    let mut violations = check_spool(&sent, &scan, &health_codes(&home));
    violations.extend(secret_violations(bench, &home, name));
    let retained: u64 = scan.segments.iter().map(|segment| segment.length).sum();

    let migrate = run_store(bench, &home, "migrate", None);
    violations.extend(check_migrate(&migrate));
    let tolerated = BTreeSet::from([spec.key.clone()]);
    let recovers = [
        run_store(bench, &home, "recover", None),
        run_store(bench, &home, "recover", None),
    ];
    for run in &recovers {
        violations.extend(check_recover(run, &tolerated));
    }
    let stored = recovers
        .iter()
        .any(|run| listed_sessions(run, "recovered").contains(&spec.key));
    let expected = if stored {
        complete_sessions(&scan)
    } else {
        Vec::new()
    };
    let store = check_store(&home, &expected);
    violations.extend(store.violations);
    let (integrity, check) = check_integrity(bench, &home);
    violations.extend(integrity);
    violations.extend(secret_violations(bench, &home, name));
    let after = scan_spool(&home);
    let spool_kept = after.segments.len() == scan.segments.len();
    let skips: Vec<Option<String>> = recovers
        .iter()
        .map(|run| skip_code(run, &spec.key))
        .collect();

    let report = json!({
        "headline": format!(
            "{retained} bytes retained against a {MAX_TRAJECTORY_BYTES}-byte ceiling; skip codes \
             [{}], stored {stored}, spool kept {spool_kept}",
            skips
                .iter()
                .map(|code| code.as_deref().unwrap_or("none"))
                .collect::<Vec<_>>()
                .join(", "),
        ),
        "events": sent.len(),
        "field_bytes_sent": OVERSIZE_FIELD_BYTES,
        "field_cap_bytes": MAX_FIELD_BYTES,
        "retained_bytes": retained,
        "ceiling_bytes": MAX_TRAJECTORY_BYTES,
        "latency": latency_by_kind(&sent),
        "spool_before_recover": spool_summary(&scan),
        "migrate_us": migrate.outcome.micros,
        "recovers": recovers
            .iter()
            .map(|run| json!({ "micros": run.outcome.micros, "summary": recover_summary(run) }))
            .collect::<Vec<_>>(),
        "skip_codes": skips,
        "stored": stored,
        "spool_kept": spool_kept,
        "spool_after_recover": spool_summary(&after),
        "spool_health_after_recover": health_codes(&home),
        "store": store.facts,
        "check": check,
    });
    PhaseResult {
        name,
        report,
        violations,
    }
}

/// Fresh homes: one `session-start` creates each home's directories, then `store migrate`
/// creates its database.
fn phase_cold(bench: &Bench) -> PhaseResult {
    let name = "cold";
    let mut violations = Vec::new();
    let mut sent = Vec::with_capacity(COLD_HOMES);
    let mut migrate_us = Vec::with_capacity(COLD_HOMES);
    for index in 0..COLD_HOMES {
        let label = format!("cold-{index:02}");
        let home = bench.home(&label);
        let spec = SessionSpec::new(&label, Runtime::Claude, false);
        let marker = format!("{label}-e0");
        let document = payload(bench, &spec, 0, &marker);
        let outcome = send(bench, &home, spec.runtime, SCRIPT[0], &document, None);
        let record = Sent {
            session: label.clone(),
            marker,
            kind: SCRIPT[0],
            kill_planned: false,
            outcome,
        };
        violations.extend(check_spool(
            std::slice::from_ref(&record),
            &scan_spool(&home),
            &health_codes(&home),
        ));
        sent.push(record);

        let migrate = run_store(bench, &home, "migrate", None);
        violations.extend(check_migrate(&migrate));
        migrate_us.push(migrate.outcome.micros);
        violations.extend(secret_violations(bench, &home, &label));
    }

    let ingest = latency_by_kind(&sent);
    let migrate = latency(&migrate_us);
    let report = json!({
        "headline": format!(
            "{COLD_HOMES} fresh homes: first ingest p50 {} us, store migrate p50 {} us",
            ingest["all"]["p50_us"],
            migrate["p50_us"],
        ),
        "homes": COLD_HOMES,
        "cold_home": ingest["all"],
        "store_migrate": migrate,
    });
    PhaseResult {
        name,
        report,
        violations,
    }
}

// ---------------------------------------------------------------------------------------------
// Report
// ---------------------------------------------------------------------------------------------

/// A listing of the ambient `AGENT_JIT_HOME` and whether the default home exists.
#[derive(Debug, PartialEq, Eq)]
struct Ambient {
    home_set: bool,
    entries: Vec<(String, u64, Option<SystemTime>)>,
    application_support: bool,
}

impl Ambient {
    fn observe() -> Self {
        let home = std::env::var_os("AGENT_JIT_HOME")
            .filter(|value| !value.is_empty())
            .map(PathBuf::from);
        let mut entries = Vec::new();
        if let Some(root) = &home {
            let mut pending = vec![root.clone()];
            while let Some(path) = pending.pop() {
                let Ok(metadata) = fs::symlink_metadata(&path) else {
                    continue;
                };
                entries.push((
                    relative(root, &path),
                    metadata.len(),
                    metadata.modified().ok(),
                ));
                if metadata.is_dir() {
                    pending.extend(sorted_entries(&path));
                }
            }
        }
        entries.sort();
        let application_support = std::env::var_os("HOME").is_some_and(|user| {
            Path::new(&user)
                .join("Library/Application Support/agent-jit")
                .exists()
        });
        Self {
            home_set: home.is_some(),
            entries,
            application_support,
        }
    }
}

fn parameters() -> Value {
    json!({
        "runtime_version": RUNTIME_VERSION,
        "script": SCRIPT.map(HookEventKind::as_str),
        "session_mix": "index % 5: 0 claude with secrets, 1 opencode, 2 opencode with secrets, \
                        otherwise claude",
        "warm_up_sessions": WARMUP_SESSIONS,
        "cold_homes": COLD_HOMES,
        "sequential": { "sessions": SEQUENTIAL_SESSIONS, "workers": 1 },
        "burst": { "sessions": BURST_SESSIONS, "workers": BURST_WORKERS },
        "crash_ingest": {
            "sessions": CRASH_INGEST_SESSIONS,
            "workers": CRASH_INGEST_WORKERS,
            "kill": "ingests with index % 3 == 2, after (index * 7919) % 60000 us",
        },
        "crash_recover": {
            "sessions": CRASH_RECOVER_SESSIONS,
            "attempts": CRASH_RECOVER_ATTEMPTS,
            "kill": "after 5 + (attempt * 37) % 300 ms",
        },
        "oversize": { "events": OVERSIZE_EVENTS, "field_bytes": OVERSIZE_FIELD_BYTES },
    })
}

fn machine() -> Value {
    let sysctl = captured(
        "/usr/sbin/sysctl",
        &["-n", "machdep.cpu.brand_string", "hw.ncpu", "hw.memsize"],
        None,
    );
    let mut lines = sysctl.lines().map(str::trim);
    json!({
        "cpu": lines.next(),
        "logical_cpus": lines.next().and_then(|value| value.parse::<u64>().ok()),
        "memory_bytes": lines.next().and_then(|value| value.parse::<u64>().ok()),
        "os": captured("/usr/bin/uname", &["-srm"], None).trim(),
    })
}

fn source_state() -> Value {
    let workspace = Path::new(env!("CARGO_MANIFEST_DIR")).join("../..");
    let head = captured(
        "/usr/bin/git",
        &["--no-optional-locks", "rev-parse", "HEAD"],
        Some(&workspace),
    );
    let status = captured(
        "/usr/bin/git",
        &[
            "--no-optional-locks",
            "status",
            "--porcelain=v1",
            "--untracked-files=no",
        ],
        Some(&workspace),
    );
    json!({
        "head": head.trim(),
        "dirty_tracked_paths": status
            .lines()
            .map(|line| line.get(3..).unwrap_or(line))
            .collect::<Vec<_>>(),
    })
}

fn rustc_version() -> String {
    Command::new("rustc")
        .arg("-V")
        .current_dir(env!("CARGO_MANIFEST_DIR"))
        .stdin(Stdio::null())
        .output()
        .map_or_else(
            |_| String::new(),
            |output| String::from_utf8_lossy(&output.stdout).trim().to_owned(),
        )
}

/// Runs a system tool with a pinned environment and returns its stdout.
fn captured(program: &str, args: &[&str], directory: Option<&Path>) -> String {
    let mut command = Command::new(program);
    command
        .args(args)
        .env_clear()
        .env("PATH", SAFE_PATH)
        .env("LC_ALL", "C")
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .env("GIT_CONFIG_GLOBAL", "/dev/null")
        .stdin(Stdio::null());
    if let Some(directory) = directory {
        command.current_dir(directory);
    }
    command.output().map_or_else(
        |_| String::new(),
        |output| String::from_utf8_lossy(&output.stdout).into_owned(),
    )
}

/// Writes the report before anything is asserted, so a failing run still leaves its evidence.
fn write_report(report: &Value) -> PathBuf {
    let path = std::env::var_os("AGENT_JIT_BENCH_REPORT")
        .filter(|value| !value.is_empty())
        .map_or_else(
            || {
                Path::new(env!("CARGO_TARGET_TMPDIR"))
                    .join("recorder-bench")
                    .join(format!("report-{}.json", unix_ms()))
            },
            PathBuf::from,
        );
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent).unwrap();
    }
    let mut file = OpenOptions::new()
        .create(true)
        .truncate(true)
        .write(true)
        .mode(0o600)
        .open(&path)
        .unwrap();
    file.write_all(serde_json::to_string_pretty(report).unwrap().as_bytes())
        .unwrap();
    file.write_all(b"\n").unwrap();
    path
}

fn print_summary(path: &Path, report: &Value) {
    progress(&format!("report {}", path.display()));
    progress(&format!(
        "{} violation(s), passed {}",
        report["violations"].as_array().map_or(0, Vec::len),
        report["passed"]
    ));
}

/// One line on stderr, so a long run shows where it is.
fn progress(line: &str) {
    writeln!(io::stderr().lock(), "recorder bench: {line}").unwrap();
}

// ---------------------------------------------------------------------------------------------
// Small helpers
// ---------------------------------------------------------------------------------------------

fn sorted_entries(directory: &Path) -> Vec<PathBuf> {
    let Ok(entries) = fs::read_dir(directory) else {
        return Vec::new();
    };
    let mut paths: Vec<PathBuf> = entries.map(|entry| entry.unwrap().path()).collect();
    paths.sort();
    paths
}

fn name_of(path: &Path) -> String {
    path.file_name()
        .map_or_else(String::new, |name| name.to_string_lossy().into_owned())
}

fn relative(base: &Path, path: &Path) -> String {
    path.strip_prefix(base)
        .unwrap_or(path)
        .to_string_lossy()
        .into_owned()
}

fn micros_since(started: Instant) -> u64 {
    u64::try_from(started.elapsed().as_micros()).unwrap_or(u64::MAX)
}

fn unix_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |elapsed| {
            u64::try_from(elapsed.as_millis()).unwrap_or(u64::MAX)
        })
}
