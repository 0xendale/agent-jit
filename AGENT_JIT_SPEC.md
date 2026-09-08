# Agent JIT — Living Spec

**Status:** v0.2 / concept validation  
**Owner:** TBD  
**Last updated:** 2026-08-24

## 1. Problem / Why

Coding agents repeatedly execute the same repository-specific procedures: locate affected tests, map files to packages, inspect diffs, filter logs, or validate generated changes. Today each repetition is re-planned by an LLM, causing avoidable token use, tool calls, latency, variance, and occasional reasoning errors.

Existing memory and skills preserve knowledge or procedures, but still require the model to interpret and execute them. Agent JIT targets stable repository habits that can be replaced by deterministic execution.

## 2. Product Thesis

> Turn repository-specific habits learned by an agent into reusable executable capabilities.

Agent JIT does not replace agent reasoning. It moves known, repetitive work out of the reasoning loop so the model can focus on novel decisions.

```text
Memory: "I learned something."
Skill:  "I learned how I should do something."
JIT:    "I no longer need to think about doing it."
```

## 3. What It Does

Agent JIT:

1. Records agent trajectories, intent, outcome, and repository state.
2. Groups runs that solved the same repository-specific intent, even when commands differ.
3. Infers explicit inputs, outputs, dependencies, and invalidation rules.
4. Compiles the capability into a constrained declarative workflow.
5. Replays historical examples and compares semantic results.
6. Presents capability-level evidence for one-click enablement.
7. Makes the capability discoverable only when relevant to a future task.
8. Measures coverage, savings, correctness, and fallback rate.

## 4. Target Users

Primary:

- Developers using coding agents daily in the same repositories.
- Teams running many agent tasks over large monorepos.
- Agent-harness maintainers with repeated, measurable workflows.

Not initially targeted:

- Casual users with too little repetition to compile.
- Non-coding general assistants.
- Workflows whose output depends mainly on subjective judgment.

## 5. Core Workflow

```text
agent runs with intent + outcome
   ↓
capture normalized trajectory + repo fingerprint
   ↓
human groups equivalent solved intents (MVP)
   ↓
infer parameters + capability contract
   ↓
compile declarative workflow IR
   ↓
replay against recorded runs in sandbox
   ↓
user enables capability from evidence summary
   ↓
register repository capability
   ↓
invoke → validate → fall back on mismatch
```

Candidate lifecycle:

`observed → proposed → validating → approved → active → degraded/retired`

## 6. Architecture

### Components

| Component | Responsibility |
|---|---|
| Trajectory recorder | Ingest intent, agent messages, tool calls, results, timing, outcome, and repo state. |
| Candidate inspector | Let a human group runs representing the same solved intent and inspect projected value. |
| Parameterizer | Infer typed inputs/outputs, preconditions, dependencies, and variable values across grouped runs. |
| Workflow compiler | Lower the capability into a constrained declarative DAG built from fixed primitives. |
| Replay validator | Execute historical fixtures and compare semantic outputs and outcomes. |
| Capability runtime | Discover relevant capabilities, check preconditions, execute the IR, or fall back safely. |

### Initial Technical Shape

- Local-first CLI; no background service required for MVP.
- SQLite metadata/trace store.
- Repository-scoped registry under `.agent-jit/` or user cache keyed by repo identity.
- Declarative workflow IR expressed as versioned YAML/JSON DAGs.
- Fixed, allowlisted primitives such as `workspace.packages_for_diff`, `repo.generated_artifacts`, `checks.for_package`, and `checks.run`.
- JSON Schema contracts for capability inputs, outputs, and primitive boundaries.
- Git commit, lockfiles, config files, and relevant source paths used as dependency fingerprints.
- Instrumented agent runtimes: Claude Code first, OpenCode second (ADR-0009). Both integrate through documented hook/plugin payloads only — never internal state files — with no model modification required.
- One discovery surface, such as `jit.query(intent)`, instead of exposing every compiled capability globally.

### Safety Model

- No autonomous grouping or activation in MVP.
- Sandboxed replay before approval.
- Fixed primitive allowlist; the compiler cannot emit arbitrary shell or Python.
- Read-only primitives by default.
- Timeout, output cap, and deterministic environment controls.
- Runtime precondition checks and immediate fallback to the original agent path.
- Full provenance: source traces, inferred contract, workflow IR, validation results, and capability version.

## 7. MVP Scope

Build a manual JIT compiler for two instrumented coding-agent runtimes — Claude Code first, OpenCode second (pulled forward from Phase 2 by ADR-0009) — and one repository. The MVP proves value before investing in automatic intent mining or synthesis.

Included:

- Capture intent, shell/tool trajectories, outcome, duration, exit status, and bounded output.
- Let a human group runs representing the same solved workflow.
- Infer candidate parameters and show projected coverage and savings.
- Compile grouped runs into a declarative DAG using fixed primitives.
- Replay candidates against at least five historical executions.
- Present an enable card with intent, observations, projected savings, and replay score; keep IR/provenance expandable.
- Discover approved capabilities through one contextual query interface.
- Compare JIT execution with baseline on correctness, coverage, latency, tokens, and tool calls.
- Use `validate_change(diff)` as a benchmark candidate, not a committed product wedge; compare it with other repository-specific habits before selection.

MVP acceptance bar:

- At least 20% of total observed agent execution cost is plausibly compilable (“JIT coverage”).
- One real capability compiled and reused at least 20 times.
- Zero silent incorrect outputs in benchmark runs.
- At least 50% fewer agent tool calls for that workflow.
- At least 30% lower median latency and estimated input tokens.

## 8. Non-Goals

- Automatic intent clustering or general-purpose autonomous program synthesis.
- Replacing agent planning, skills, memory, or orchestration.
- Model/tool routing optimization.
- Compiling one-off or judgment-heavy tasks.
- Cross-repository tool sharing before contracts and security are proven.
- Arbitrary generated scripts or primitives that mutate source code in MVP.
- Guaranteeing deterministic behavior for external network services.

## 9. Example Workflows

### A. Validate Current Change

Repeated solved intent: “validate this change according to how this repository works.” Different runs may use different commands while producing the same capability.

```text
inspect diff
→ map files to packages
→ detect generated artifacts requiring regeneration
→ resolve feature flags and required checks
→ run cheapest checks first
→ summarize actionable failures
```

Compiled tool:

```text
validate_change({ "base_ref": "HEAD~1" })
→ { "checks": [...], "failures": [...], "evidence": [...], "fingerprint": "..." }
```

### B. Reproduce Known Failure

```text
resolve issue or failure ID
→ recover repository-specific fixture and environment
→ run the established reproduction path
→ verify the expected failure signature
```

Compiled capability: `reproduce_known_failure(id)`.

### C. Failure-Only Test Output

```text
run test suite
→ identify failed targets
→ extract relevant failure blocks
→ map stack frames to source
```

Compiled capability: `test_failures(target, since_fingerprint)`.

## 10. Benchmark / Success Metrics

### Correctness Gates

- Semantic output match against historical runs: 100% for activation.
- Silent mismatch rate: 0%; any mismatch must trigger fallback.
- False eligibility rate: <1% of invocations.
- Deterministic replay rate: ≥99% for supported workflows.

### Product Metrics

- JIT coverage = cost of compilable repeated work / total agent execution cost.
- Realized total saving = JIT coverage × saving within compiled paths.
- Median tool calls avoided per invocation.
- Median input/output tokens avoided.
- p50 and p95 latency reduction.
- End-to-end task success delta versus baseline.
- Capability reuse rate after enablement.
- Candidate approval and rejection rates.
- Fallback, degradation, and retirement rates.
- Time-to-break-even: compile/validation cost divided by per-use savings.

### Initial Experiment

First analyze 50–100 real trajectories to estimate repeated-work cost, deterministic share, JIT coverage, and theoretical savings. Only then run 30–50 repository tasks in paired conditions:

1. Baseline agent with no JIT tools.
2. Same agent and tasks with one enabled JIT capability.

Pin model, prompts, repository commit, environment, and task set. Compare correctness first, then tokens, tool calls, latency, and variance.

`validate_change(diff)` is only a convenient benchmark if it demonstrates behavior beyond existing CI, build-system, or affected-test automation. Phase 0 must also identify and rank alternative capabilities by user pain, repository specificity, existing-tool overlap, frequency, and compilability.

## 11. Risks and Mitigations

| Risk | Mitigation |
|---|---|
| Equivalent commands obscure a repeated intent | Human grouping in MVP; preserve intent and outcome alongside tool traces. |
| Repeated commands represent different intents | Compile grouped solved intents, not raw subsequences. |
| Pattern looks stable but encodes hidden assumptions | Explicit preconditions, dependency fingerprints, replay fixtures, fallback. |
| Repository changes invalidate a tool | Fine-grained dependency tracking; mark stale before execution. |
| Compiled workflow creates a security boundary | Declarative IR, fixed primitives, sandbox, read-only default, capability-level enablement. |
| Savings are smaller than compile/maintenance cost | Require projected break-even threshold before proposing a candidate. |
| Agent fails to discover or trust the capability | Contextual `jit.query(intent)`, evidence output, and routing benchmark. |
| Overfitting to one agent’s noisy behavior | Require repeated successful traces and normalize equivalent trajectories. |
| Capabilities accumulate and create tool-schema bloat | Single discovery surface, contextual schema loading, usage-based retirement. |
| Captured traces leak secrets | Local storage, redaction before persistence, configurable retention. |

## 12. Open Questions

- Which runtime provides the cleanest first integration: Codex, Claude Code, or a generic proxy?
- What intent/outcome representation is sufficient to group equivalent workflows after MVP?
- Which candidate capability has the strongest user pull without duplicating existing CI or repository automation?
- What semantic comparator can validate outputs without reintroducing expensive LLM judgment?
- How granular should dependency fingerprints be to avoid both stale results and excessive invalidation?
- Should compiled capabilities be committed to the repository or remain per-user artifacts?
- How should capabilities expose evidence so agents can verify rather than blindly trust results?
- What minimum repetition and projected savings justify compilation?
- Can one contextual discovery interface route accurately without adding more context than it saves?
- Which operations can safely graduate from read-only to mutating?

## 13. Phased Roadmap

### Phase 0 — Validate the Thesis

- Collect 50–100 real coding-agent trajectories from one repository.
- Manually group repeated solved intents and measure their execution cost.
- Estimate repeated-work share, deterministic share, JIT coverage, and theoretical total savings.
- Select one repository-specific habit with high cost, stable outcomes, and low judgment.
- Stop if JIT coverage is below 20% or no capability can plausibly break even within 20 uses.

### Phase 1 — Manual JIT Compiler

- Implement the trajectory recorder, candidate inspector, parameterizer, workflow IR, replay validator, and capability runtime.
- Keep solved-intent grouping fully manual throughout MVP; do not treat automatic clustering as required follow-up work.
- Benchmark the highest-ranked Phase 0 capability; use `validate_change(diff)` only if it wins the candidate comparison.

### Phase 2 — Assisted Compilation

- Add candidate ranking, dependency fingerprints, automatic stale detection, and periodic replay.
- Support 3–5 read-only primitive families.
- Integrate a second repository. The second agent runtime (OpenCode) was pulled forward into the current build by ADR-0009.

Automatic intent clustering remains optional research. Add it only after manual grouping proves product value and a separate benchmark shows high grouping precision without hiding ambiguous cases.

### Phase 3 — Adaptive Runtime

- Propose solved-intent candidates automatically with projected coverage and break-even.
- Route eligible calls, monitor mismatches, and retire degraded tools.
- Add team-shared registries with signed provenance and policy controls.

### Phase 4 — Broader Compilation

- Evaluate safe mutating primitives, cross-repository capabilities, executable lowering, and richer contract synthesis.
- Proceed only if correctness and net savings remain positive after maintenance costs.

## 14. Immediate Build Order

1. Define normalized trajectory, intent, outcome, and capability schemas.
2. Build a recorder for one agent runtime.
3. Collect and manually classify 50–100 real trajectories.
4. Calculate repeated cost, deterministic share, coverage, and potential savings.
5. Select the strongest Phase 0 capability, then define its minimal workflow IR and fixed primitive set.
6. Add parameter inference, replay, semantic comparison, and capability enablement.
7. Expose contextual discovery and run paired benchmarks.

## 15. Decision Rule

Continue investment only if Phase 0 finds meaningful JIT coverage and the manual compiler preserves correctness while materially reducing total execution cost. Automatic detection cannot rescue weak coverage. If the best candidate cannot reach the acceptance bar, stop product development or narrow Agent JIT to a trajectory observability experiment.
