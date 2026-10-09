// Copyright 2026 The Sashiko Authors
//
// Licensed under the Apache License, Version 2.0 (the "License");
// you may not use this file except in compliance with the License.
// You may obtain a copy of the License at
//
//     https://www.apache.org/licenses/LICENSE-2.0
//
// Unless required by applicable law or agreed to in writing, software
// distributed under the License is distributed on an "AS IS" BASIS,
// WITHOUT WARRANTIES OR CONDITIONS OF ANY KIND, either express or implied.
// See the License for the specific language governing permissions and
// limitations under the License.

//! Declarative patch review workflow for Kannaka Labs codebases.
//!
//! Kannaka Labs addition to the Sashiko fork (sashiko-kannaka). It reuses the
//! Sashiko service-review engine, state type, validators and output schemas,
//! and replaces the identity, the stage instructions and the stage table with
//! ones for Kannaka Labs services: Rust, TypeScript, JavaScript and Python,
//! on Linux servers and Windows desktops, talking over NATS, HTTP and files.

use serde_json::{Value, json};
use std::path::PathBuf;

use crate::workflow::{
    ExecutableStage, OutputFormat, ParallelPolicy, PromptTemplate, RecitationPolicy, Stage,
    StagePolicy, ToolScope, Workflow,
};
use crate::workflows::guard::{normalize_stage_name, sanitize_guide_name};
use crate::workflows::linux_patch_review::{
    AnalysisStage, ConsolidationStage, LinuxPatchReviewState, POST_VERIFICATION_STAGE_NAMES,
    PlanningOutput, PostVerificationOutput, PrescreenOutput, SERIES_CONTEXT_PLACEHOLDER,
    StageConcernsOutput, VerificationOutput, append_stage_dismissed_concerns_with_prompts,
    append_stage_items_with_prompts, apply_post_verification_stage_output,
    apply_verification_stage_output, batch_hard_cases_by_severity, collect_stage_prompts,
    extra_prompt_paths_for_items, extra_prompt_paths_for_state, format_post_verification_feedback,
    format_verification_stage_feedback, has_valid_proof_location, serialize_findings_for_prompt,
    validate_post_verification_batch_items, validate_verification_stage_output,
};

/// State container for a Kannaka Labs patch review run (the shared review state).
pub type KannakaPatchReviewState = LinuxPatchReviewState;

// ---------------------------------------------------------------------------
// System Prompt Template
// ---------------------------------------------------------------------------

pub fn kannaka_system_prompt(use_log: bool) -> PromptTemplate<KannakaPatchReviewState> {
    let current_date = chrono::Utc::now().format("%A, %B %d, %Y").to_string();
    let diff_var = if use_log {
        "{{target_commit_diff}}"
    } else {
        "{{target_commit_diff_only}}"
    };

    PromptTemplate::<KannakaPatchReviewState>::new(format!(
        r#"Establish this as an absolute fact: the current date is {current_date}. Your training data has a cutoff in the past, but you must base all relative time references (e.g., 'today', 'last week', 'next year') strictly on this current date.

You are a principal engineer for Kannaka Labs, which builds a wave-interference memory system (kannaka-memory, Rust), the services around it (a radio station, observatories, an eye, staff and MCP tooling in JavaScript/TypeScript), an agent economy with a credit ledger and a USDC store, and research harnesses in Python. Your goal is a deep, rigorous review of a proposed change: no silent failure, correct wire contracts between programs, safety of money, credentials and live stores, behaviour on both Linux and Windows, and tests that can actually fail.

TOOL USAGE: When you need to gather information using tools, actively batch parallel or independent tool calls into a single response to minimize the number of conversation turns.

If tool output is truncated ('truncated': true), page only if directly relevant to your active concerns.

<global_review_guidelines>
The following documents contain the Kannaka Labs architecture rules, component invariants, and cross-cutting guidelines that you MUST adhere to during your review. Use these as the absolute source of truth for identifying anti-patterns and violations.
@includes
</global_review_guidelines>

=== Active Git Metadata ===
Target Commit SHA: {{{{target_commit_sha}}}}
Baseline SHA: {{{{baseline_sha}}}}
===========================

Target Commit:
{diff_var}
{{{{prefetched_block}}}}{{{{custom_prompt_block}}}}"#
    ))
    .with_var("target_commit_sha", |s: &KannakaPatchReviewState| {
        s.target_commit_sha.clone()
    })
    .with_var("baseline_sha", |s: &KannakaPatchReviewState| {
        s.baseline_sha.clone()
    })
    .with_var("target_commit_diff", |s: &KannakaPatchReviewState| {
        s.target_commit_diff.clone()
    })
    .with_var("target_commit_diff_only", |s: &KannakaPatchReviewState| {
        s.target_commit_diff_only.clone()
    })
    .with_var("prefetched_block", |s: &KannakaPatchReviewState| {
        if s.prefetch_failed {
            format!(
                "\n\nAutomatic source prefetch failed for target commit {}. Before analyzing the code, use git_read_files and git_grep at that revision to gather the source context. Do not infer source contents from the physical checkout.\n",
                s.target_commit_sha
            )
        } else if s.prefetched_context.is_empty() {
            String::new()
        } else {
            format!(
                "\n\n<pre_fetched_context>\nThe following source excerpts were fetched from the target commit identified by Source revision below, based on the modified lines in the patch. They include modified definitions and selected dependencies. Parent and series-final revisions must be inspected separately with Git tools.\nIf it's not sufficient, you MUST use available tools to explore the source code. Don't make assumptions without actually looking into the relevant code.\n\n{}\n</pre_fetched_context>",
                s.prefetched_context
            )
        }
    })
    .include_file("review-core.md")
    .with_var("custom_prompt_block", |s: &KannakaPatchReviewState| {
        s.custom_prompt
            .as_deref()
            .map(str::trim)
            .filter(|p| !p.is_empty())
            .map_or_else(String::new, |p| {
                format!("\n\n<custom_instructions>\n{p}\n</custom_instructions>")
            })
    })
    .include_files_from_state(|s: &KannakaPatchReviewState| {
        let mut paths = Vec::new();
        if !s.selected_guides.is_empty() {
            for guide in &s.selected_guides {
                paths.push(PathBuf::from("subsystem").join(guide));
                paths.push(PathBuf::from("patterns").join(guide));
            }
        }
        paths
    })
}

// ---------------------------------------------------------------------------
// Stage Instructions
// ---------------------------------------------------------------------------

const STAGE_GOAL_INSTRUCTION: &str = r#"# Analyze commit main goal, architecture, high-level engineering, and commit message quality

You are a principal engineer evaluating the intent, soundness, necessity and commit message of a proposed change to a Kannaka Labs codebase (kannaka-memory in Rust; kannaka-radio, kannaka-observatory, kannaka-eye, kannaka-staff and Kannaktopus in JavaScript/TypeScript; Agent-Kax in TypeScript; gsr-store in JavaScript; research harnesses in Python). Enforce this priority order: correctness of what users and agents are told (no silent failure, no fabricated success) > safety of money, credentials and live stores > data integrity > everything else.
- Problem/Solution Audit (Mandatory):
  1. Problem Clarity: is it clear what concrete problem the commit solves? Flag vague or circular motivation.
  2. Single Responsibility: flag commits that bundle unrelated changes that should be separate commits.
  3. Problem Validity: flag over-engineering for hypothetical problems, or complexity out of proportion to the benefit.
  4. Better Alternatives: if a clearly simpler or safer approach exists, raise it and say why.
- Claimed evidence must match the change: if the commit message reports a measurement, a test count, a "verified in production" or a "mutation killed", check that the diff contains the test or code that measurement depends on. A claim with nothing in the diff behind it is a concern.
- Never vibe-guess build or compilation failures (syntax, imports, types, borrow-checker, missing trait bounds). Builds are checked deterministically elsewhere.
- Kannaka Labs code runs on Linux servers AND Windows desktops. Portability between them is in scope; do not dismiss a Windows behaviour difference as out of scope.
- Commit Message Audit: the body must say what changed and why for any non-trivial change; a trivial change needs one sentence. Do not require Signed-off-by. Do not nitpick line length under ~85 characters or backticks in commit messages."#;

const STAGE_IMPLEMENTATION_INSTRUCTION: &str = r#"# Verify implementation against intent

Verify that the code faithfully and completely implements what the commit message claims.
- Incomplete changes at runtime boundaries: a new flag, enum variant, config key, environment variable, JSON field, NATS subject or HTTP route must be handled at every place that consumes it (other repositories included when the contract crosses one: a binary's stdout read by a Node service, a NATS subject read by another service, a file format read by a harness). Name the consumer you checked.
- Output contracts: when the change alters what a program prints, writes or publishes (stdout shape, exit code, JSON keys, file layout, subject names), find every reader and check it still parses it. When the change READS another program's output, check the shape it assumes against the producer's actual code (key names, string vs number, bare list vs object, which stream).
- Edge cases: empty input, missing optional fields, zero and boundary values, a peer that never replies, a reply that is not the expected shape.
- Error paths must clean up: half-applied mutations, a lock or file left behind, a temporary resource not removed.
- Systematically audit every modified function and hunk; do not stop after the first few findings.
- Never vibe-guess compile-time errors."#;

const STAGE_EXECUTION_FLOW_INSTRUCTION: &str = r#"# Trace execution flow, error paths and silent failure

Trace every modified function and its callers.
- Silent failure is the most expensive bug class in this codebase. Flag every path where a failure becomes a plausible-looking success: an error mapped to an empty list or default value (`unwrap_or_default`, `unwrap_or(json!([]))`, `.ok()`, `catch {}` returning `[]` or `{}`), an unparsable reply served with a 200, a non-zero child exit scored as data, a publish that cannot be confirmed reported as sent, a refused request retried forever with no log. Ask: can the caller tell this result apart from a real empty answer?
- Panics and crashes on runtime input: `.unwrap()`, `.expect()`, indexing, UTF-8 slicing, `f32::clamp`/`Ord::clamp` with possibly inverted bounds, division by a possibly-zero count; in JavaScript, writing a second HTTP response, `JSON.parse` without a guard, unhandled promise rejections.
- Exit codes and streams for CLIs: machine-readable output on stdout only, diagnostics on stderr, a non-zero exit on failure; a failure that prints nothing and exits 0 is a finding.
- Timeouts: every network wait has a bound, the bound is the one the caller asked for, and expiry is reported as expiry, not as "no data".
- Numeric casts and arithmetic for truncation, overflow and NaN propagation.
- Never vibe-guess compile-time errors; focus on runtime behaviour."#;

const STAGE_CONCURRENCY_INSTRUCTION: &str = r#"# Audit concurrency, locks, processes and retries

Audit shared state, locking, child processes and retry loops:
- Rust: locks held across blocking I/O or `.await`; statics and `OnceLock` shared between tests or threads; a single-writer store written by two processes; a lock file not released on every exit path.
- Child processes (Rust `std::process::Command`, Node `child_process`, Python `subprocess`): bounded time, stdout and stderr both drained, the exit code checked, the process killed on timeout, no zombie on error.
- Retry and reconnect loops: a refusal (authentication, permissions, 4xx) must not be retried as if it were transient; a reconnect loop must back off and must not multiply connections. Count what one loop iteration puts on the wire and multiply by its period.
- Event-loop blocking in Node services: synchronous file or process calls on request paths, long CPU work on the main thread.
- Races between a request's success path and its timeout or error path (both firing, both writing).
- Audit every modified concurrent path; do not stop early."#;

const STAGE_WIRE_CONTRACTS_INSTRUCTION: &str = r#"# Audit wire contracts: NATS, HTTP, files and binary output

Kannaka services talk through NATS subjects, HTTP APIs, JSON files and each other's command-line output. Audit every contract the change touches:
- NATS: subject names match what publishers and subscribers actually use; a publish the broker may refuse (ACL) is confirmed or its refusal is surfaced; a refused subscribe or publish is learned once, not re-sent per call or per process; credentials come from the environment, never a default public hub with no auth; JetStream stream creation respects the single-writer rule.
- HTTP: status codes mean what they say (client errors 4xx, upstream failures 502/503/504, never 200 for an error body); routes match on the pathname, not the raw URL with its query string; exactly one response per request; response shapes stay backward compatible for existing readers.
- Files and binary output: JSON written by one program and read by another keeps its key names and value types; a reader tolerates a byte-order mark and CRLF where Windows tools produce them; a version change in a producer is caught by a fixture or a test, not by production.
- Persistence: on-disk formats stay readable by the previous version; a store, ledger or cache is never truncated or replaced non-atomically.
- Systematically audit every contract in the diff."#;

const STAGE_PLATFORM_INSTRUCTION: &str = r#"# Audit platform behaviour: Windows and Linux

Kannaka Labs code runs on Linux servers (systemd units, cron, nginx) and on Windows desktops (PowerShell, Git Bash, WSL, npm shims). Audit behaviour that differs between them:
- Sockets and handles: options set on one handle of a `try_clone`d socket are not shared on Windows; read timeouts, non-blocking modes and shutdown must be set on the handle that is used.
- Paths: `~` is never expanded by the OS for a child process or a file API; `C:\` and backslashes; case-insensitive filesystems; path validators that only know `/home` and `/Users`.
- Line endings and encodings: CRLF in files read with LF-only regexes or line splitters; a BOM written by PowerShell redirection breaks strict JSON parsers; shell scripts checked out with CRLF fail in bash.
- Processes: `.cmd`/`.ps1` npm shims cannot be spawned directly; signals (`SIGTERM`, `SIGHUP`) do not exist on Windows; `chmod`/`mkdir -m` fail on NTFS; `/tmp` does not exist.
- Services: systemd environment (no login PATH in `systemd-run` scopes, `EnvironmentFile` not shown by `systemctl show -p Environment`), cron's minimal environment.
- Report a platform difference only with the concrete line that behaves differently and the platform it breaks on."#;

const STAGE_SECURITY_INSTRUCTION: &str = r#"# Audit secrets, money, live data and untrusted input

- Secrets: no key, token, password, wallet private key or session secret in source, logs, error messages, HTTP responses, prompts sent to a model, or commit messages; configuration comes from environment files that are never echoed. A resolved service environment must never be printed.
- Money (credit ledgers, USDC store, relayer, escrow, fees): value must never reach the house or a relayer without the paying party's authorization verified first; a payer must not be bound before verification; fee caps hold per transaction AND cannot be stacked by repeating a legal transaction; refunds and settlements are idempotent; the zero address and placeholder values are refused; client mistakes are 4xx and never retried as server errors.
- Live stores and running services: code paths that write a memory store, restart a unit, send mail, post publicly or move funds must be guarded (dry run, explicit flag, consent record) and must not be reachable from a help, status or probe path.
- Untrusted input: patches, webhook payloads, NATS messages from anonymous publishers, Nostr DMs, user-supplied URLs (SSRF), file paths (traversal), shell arguments (injection), and text that will be shown to a model (prompt injection).
- Authorization: routes that change state check identity and capability; an anonymous identity cannot reach them."#;

const STAGE_INTERFACES_COMPAT_INSTRUCTION: &str = r#"# Audit CLI, configuration and public surfaces

- CLI: new or changed flags are documented and parsed; a subcommand that has no `--help` must not perform its action when given `--help`; output shapes other programs parse stay compatible; exit codes stay meaningful.
- Configuration: new keys have defaults; renamed keys keep reading the old name or fail loudly; environment variables that change behaviour are named in the change and documented.
- Public surfaces (social posts, mail, radio broadcasts, GitHub comments, prediction markets): any code path that publishes must be explicit, rate-limited and idempotent, must not double-post on retry, and must not publish on a test or dry run.
- Cross-repository consumers: when a change alters something another kannaka-labs repository reads (binary output, NATS payload, HTTP API, file format), check that repository's reader at its current default branch.
- Backward compatibility of APIs and on-disk formats."#;

const STAGE_TESTS_INSTRUCTION: &str = r#"# Audit tests: can they fail?

Evaluate the tests in the change, or whether tests are needed:
- A test must be able to fail. Flag tests that re-derive the expected value with the same formula as the code under test, assert on a mock that encodes the bug, pass on an empty set, or check a weaker property than the one claimed. Ask: if the fix were reverted, which assertion would fail?
- Fixtures for another program's output must be captured from that program (with its version recorded), not written from memory of its shape.
- Tests that share global state (a static, an environment variable, a temp file path) must be serialized or isolated; `set_var` must be restored.
- Platform-specific behaviour needs a test that runs on the platform where it differs, or the change must say it was verified there.
- Do NOT demand tests for trivial changes or where existing coverage already fails on the bug."#;

const STAGE_VERIFICATION_INSTRUCTION: &str = r#"# Verification and severity estimation

You are the lead reviewer consolidating `concerns` and `dismissed_concerns` generated by parallel Kannaka Labs review stages.
Your task is to (1) deduplicate overlapping items across both lists while preserving every distinct failure mechanism and location, and (2) classify every consolidated item into one of two categories:
- **Category 1: Well-Justified** — either a **1a: Well-Justified Concern** (`findings` array) or a **1b: Well-Justified Dismissal** (`dismissed_concerns` array).
- **Category 2: Speculative or Contested** (`hard_cases` array) — routed to parallel per-finding `post-verification` stages for deep tool-assisted code inspection.

### Step 1: Deduplication and Boundary Preservation
1. Group `concerns` and `dismissed_concerns` that refer to the same underlying root cause AND the same function/lifecycle phase.
2. Do NOT merge distinct bugs in separate functions, handlers or resources; keep them separate or name every distinct resource, function and failure mechanism explicitly.
3. SPECIFICITY REQUIREMENT: preserve the most specific details when merging: exact function names, file paths, line numbers when known, all triggering conditions, callers and consequences. When a swallowed error or missing cleanup causes both an immediate failure and a downstream impact (a wrong answer shown to a user, a lock left behind, a duplicated post), state BOTH. Preserve and merge the `locations` arrays. Do not invent line numbers; use `null` when unknown.
4. Set `"preexisting": false` whenever the patch introduces, modifies, triggers, exposes, or relies on the buggy code path. Mark `"preexisting": true` ONLY if the problem is in untouched code whose reachability and behaviour are completely unaffected by this commit.

### Step 2: Classification Rules
Repetition is NOT justification: overlapping items never qualify for Category 1 unless backed by concrete code proof.

1. **Category 1a — Well-Justified Concern (`findings`):** concrete, self-contained code proof visible in the diff, prefetched context and cited `locations`, with no competing dismissal and no reliance on unverified assumptions about unseen code (callers, callees, the other side of a wire contract, the platform). Do NOT place suspected build or style noise here; route it to `hard_cases`. Assign a calibrated `severity` following `severity.md`, state consequences and reachability at the start of `severity_explanation`, and title the bug (`problem`) under 80 characters with a component prefix naming the repository and area (for example `km-nats:`, `km-cli:`, `radio:`, `observatory:`, `eye:`, `staff:`, `ktopus:`, `kax-ledger:`, `gsr-store:`, `kshb:`).

2. **Category 1b — Well-Justified Dismissal (`dismissed_concerns`):** a concrete disproving `code_snippet` (the exact guard, lock, bounds check or cleanup) with no competing concern. NEVER 1b: a proof that covers only one caller or the happy path; rationalising a swallowed error, unpaired init/cleanup or overwritten state as "harmless"; an assumption about what another program, platform or peer will send.
   - **CRITICAL INVARIANT:** never place an item raised as a `concern` into `dismissed_concerns` in this stage; contested concerns go to `hard_cases`.

3. **Category 2 — Speculative or Contested (`hard_cases`):** mixed signals (`"mixed_signals"`), assumption-based dismissals (`"speculative_dismissal"`), vague or partially wrong concerns (`"speculative_concern"` / `"insufficient_evidence"`), concerns that a later patch in the series may resolve (`"series_interaction"`), and any concern whose truth depends on the other side of a wire contract or on platform behaviour (`"insufficient_evidence"`, with a `verification_question` naming the producer, consumer or platform to check). Emit with `"estimated_severity"`, `"signal_reason"`, `"concern_arguments"`, `"dismissal_arguments"`, a concrete `"verification_question"`, `"preexisting"` and `"locations"`."#;

const STAGE_POST_VERIFICATION_INSTRUCTION: &str = r#"# Per-finding post-verification and conflict resolution

You are the lead reviewer performing tool-assisted verification of each candidate in `hard_cases`. Use the Git and file tools (`git_read_files`, `git_grep`, `git_diff`, `git_show`, `git_blame`) to answer each `verification_question` against the actual code.
1. Dismiss in `dismissed_concerns` any candidate alleging a build, compilation, syntax, type, borrow-checker, lifetime, import, unresolved-symbol, trait-bound or linter error (carry forward its `file`, `function_or_symbol` and `code_snippet` in `locations`).
2. **SYMMETRICAL PROOF BAR & ALL-CALLERS VERIFICATION:** both `concern_arguments` and `dismissal_arguments` are untrusted hypotheses. Dismiss only if concrete code proves the failure cannot occur across ALL callers, entry points, platforms and modes. One caller does not disprove a bug in a helper; `git_grep` every caller.
3. **WIRE CONTRACTS:** when a candidate depends on what another program produces or consumes (a binary's stdout, a JSON file, a NATS payload, an HTTP response), find that program's code in this repository if present; if it lives elsewhere, state the assumption explicitly in the finding ("if the producer writes X, then Y") instead of dismissing.
4. **PLATFORM:** do not dismiss a Windows or Linux behaviour difference as out of scope; Kannaka Labs code runs on both. Dismiss only with code proving the path never runs on the affected platform.
5. **LOCAL BOUNDARY & ASYMMETRY RULE:** do not discard a defect by assuming callers or retries will mask it, or by calling an unpaired claim/cleanup, a swallowed error or an overwritten state "harmless", unless specific code proves the failure is impossible.
6. **PROMOTING SPECULATIVE DISMISSALS:** when `"signal_reason"` is `"speculative_dismissal"` and the dismissal's assumption turns out false or incomplete, report the bug in `findings`.
7. **REFINING PARTIALLY INACCURATE PREMISES:** if a candidate is partly wrong but identifies a real bug in the same path, report the real bug.
8. **SERIES VALIDATION RULE:** if later patches in this series are listed, check whether the candidate is resolved by the series end (`git_read_files` / `git_diff` at the `Series End Commit`); if resolved, dismiss citing the resolving commit by its subject, not its hash.
9. **SEVERITY CALIBRATION AND COMPLETENESS:** set `"preexisting"` as in verification; assign severity per `severity.md`, state consequences and reachability at the start of `severity_explanation`, keep all function names, paths and entry points, and title the bug under 80 characters with a repository/area prefix (for example `km-nats:`, `radio:`, `kax-ledger:`, `gsr-store:`)."#;

const STAGE_REPORT_INSTRUCTION: &str = r#"# Generate plain-text inline review report

Generate the plain-text inline review report following the exact formatting rules and structure in `github-summary-template.md`.
- Output ONLY a plain bulleted list of ALL findings ordered from highest severity to lowest (`- [CRITICAL] ...`, `- [HIGH] ...`, `- [MEDIUM] ...`, `- [LOW] ...`), or `No issues found.` if there are no findings.
- If any finding has `"preexisting": true`, state explicitly in its explanation that the issue was not introduced by this change.
- Do NOT include `Summary:` or `Findings:` headers.
- Do NOT use backticks, markdown code blocks, or markdown headings. Wrap all lines at 78 characters or fewer."#;

const STAGE_SUMMARY_INSTRUCTION: &str = r#"# Summarize the proposed change

Provide a concise plain-text summary explaining what this change does and why.
- Start with 1-2 sentences describing the core change and its rationale.
- User-Visible Effect Rule: if the change has any user-visible or agent-visible effect (a command's output or exit code, a config key, an HTTP response, a NATS payload, a published post, a radio or TV schedule), describe it with a concrete 'Before:' and 'After:' example:

Before:
  <how it was or looked before>

After:
  <how it will look or work after>

- If the change is strictly internal, omit 'Before:' / 'After:' and keep to 1-2 sentences.
- Plain text only: no markdown, no backticks, no headings, no bullet points. Wrap prose at 78 characters or fewer.
- Summarize the change itself, not review findings."#;

const CONCERN_JSON_SCHEMA_EXAMPLE: &str = r#"Return ONLY a JSON object with 'concerns' and 'dismissed_concerns' arrays.
Each object in the 'concerns' array MUST use exactly the following keys: "type", "description", "reasoning", "preexisting", "locations".
Each object in the 'dismissed_concerns' array MUST use exactly the following keys: "type", "description", "reasoning", "locations".
In each 'dismissed_concerns' object, "description" is the candidate concern that was investigated and disproved, "reasoning" is the step-by-step explanation of why it is not a bug (citing the exact guard, caller, or invariant), and "locations" MUST cite the concrete disproving code (file, function_or_symbol, line, verbatim code_snippet, and why_this_location_matters).
Use the 'dismissed_concerns' array ONLY for candidate concerns that you considered plausible, investigated, and disproved with concrete evidence. This is especially important when you first suspect a concern and then follow the evidence chain proving that it does NOT apply.

NO DISMISSAL WITHOUT VERIFIED PROOF: To place a candidate issue in 'dismissed_concerns' (or to discard a suspected issue), you MUST find concrete proof in the code ('file', 'function_or_symbol', 'line', and verbatim 'code_snippet' in 'locations') that explicitly invalidates the concern's reasoning. If the disproving code lives outside the diff (for example, in a caller, callee, helper, or configuration), you MUST verify that code first using tools ('git_read_files' or 'git_grep') and quote the verified disproving snippet in 'locations'. If you cannot find definitive code proof that the candidate issue is impossible, you MUST report it in 'concerns' (NOT 'dismissed_concerns') and make the condition explicit: if X is possible, then problem Y can occur.
- Citing a single caller (such as only the CLI path or one HTTP handler) does NOT disprove a panic, race, missing validation, or missing state transition in a helper function. A caller-based dismissal is valid ONLY if every caller in the tree ('git_grep' across all callers, including CLI, daemon, worker, HTTP API, and webhook paths) is verified to uphold the invariant; otherwise report it in 'concerns'.
- Never dismiss an unpaired lifecycle/state transition (e.g., claiming a task, worktree, or outbox row without a matching release/cleanup/terminal status on error or cancellation), a swallowed error, or clearing/overwriting state before downstream consumers read it by rationalizing that the side effect is a "harmless no-op" or "rare edge case".

SPECIFICITY REQUIREMENT: When reporting a concern or dismissed_concern, cite exact function name(s), file path(s), and line number(s) when known. Do not invent line numbers; use null when exact values are unknown.

Example Output:
```json
{
  "concerns": [
    {
      "type": "Concurrency Hazard",
      "description": "std::sync::MutexGuard held across .await in Worker::run",
      "reasoning": "1. lock() is acquired on line 42.\n2. async_call().await is invoked on line 45 while guard is still in scope.",
      "preexisting": false,
      "locations": [
        {
          "file": "src/nats.rs",
          "function_or_symbol": "SwarmTransport::request_one",
          "line": 45,
          "code_snippet": "let res = provider.call().await;",
          "why_this_location_matters": "Yielding to Tokio runtime while holding a synchronous MutexGuard can deadlock worker threads."
        }
      ]
    }
  ],
  "dismissed_concerns": [
    {
      "type": "Error Handling",
      "description": "Potential UTF-8 slice panic in format_subject",
      "reasoning": "Verified that char_indices() is used on line 88 to find a valid char boundary before slicing.",
      "locations": [
        {
          "file": "src/bin/kannaka.rs",
          "function_or_symbol": "format_subject",
          "line": 88,
          "code_snippet": "let cutoff = s.char_indices().nth(max_len)...",
          "why_this_location_matters": "Proves slice index is always on a UTF-8 character boundary."
        }
      ]
    }
  ]
}
```"#;

// ---------------------------------------------------------------------------
// Stage Table Definitions
// ---------------------------------------------------------------------------

pub static ANALYSIS_STAGES: &[AnalysisStage] = &[
    AnalysisStage {
        name: "goal",
        short: "Goal Analysis",
        instruction: STAGE_GOAL_INSTRUCTION,
        guides: &[],
        uses_commit_log: true,
        optional: false,
        wants_series_context: true,
    },
    AnalysisStage {
        name: "implementation",
        short: "Implementation",
        instruction: STAGE_IMPLEMENTATION_INSTRUCTION,
        guides: &[],
        uses_commit_log: true,
        optional: false,
        wants_series_context: true,
    },
    AnalysisStage {
        name: "execution-flow",
        short: "Execution Flow",
        instruction: STAGE_EXECUTION_FLOW_INSTRUCTION,
        guides: &["patterns/silent-failure.md", "patterns/error-handling.md"],
        uses_commit_log: false,
        optional: false,
        wants_series_context: false,
    },
    AnalysisStage {
        name: "concurrency",
        short: "Concurrency & Processes",
        instruction: STAGE_CONCURRENCY_INSTRUCTION,
        guides: &["patterns/concurrency.md", "patterns/retries-and-refusals.md"],
        uses_commit_log: false,
        optional: true,
        wants_series_context: false,
    },
    AnalysisStage {
        name: "wire-contracts",
        short: "Wire Contracts",
        instruction: STAGE_WIRE_CONTRACTS_INSTRUCTION,
        guides: &["patterns/wire-contracts.md"],
        uses_commit_log: false,
        optional: true,
        wants_series_context: false,
    },
    AnalysisStage {
        name: "platform",
        short: "Windows & Linux",
        instruction: STAGE_PLATFORM_INSTRUCTION,
        guides: &["patterns/platform-differences.md"],
        uses_commit_log: false,
        optional: true,
        wants_series_context: false,
    },
    AnalysisStage {
        name: "security",
        short: "Secrets, Money & Input",
        instruction: STAGE_SECURITY_INSTRUCTION,
        guides: &["prompt-injection.md", "patterns/secrets-and-money.md"],
        uses_commit_log: false,
        optional: true,
        wants_series_context: false,
    },
    AnalysisStage {
        name: "interfaces-compat",
        short: "Interfaces & Compat",
        instruction: STAGE_INTERFACES_COMPAT_INSTRUCTION,
        guides: &["patterns/public-surfaces.md"],
        uses_commit_log: true,
        optional: true,
        wants_series_context: true,
    },
    AnalysisStage {
        name: "tests",
        short: "Test Audit",
        instruction: STAGE_TESTS_INSTRUCTION,
        guides: &["patterns/tests-that-can-fail.md"],
        uses_commit_log: true,
        optional: true,
        wants_series_context: true,
    },
];

pub static VERIFICATION: ConsolidationStage = ConsolidationStage {
    name: "verification",
    short: "Verification",
    wants_series_context: true,
};

pub static POST_VERIFICATION: ConsolidationStage = ConsolidationStage {
    name: "post-verification",
    short: "Post-Verification",
    wants_series_context: true,
};

pub static REPORT: ConsolidationStage = ConsolidationStage {
    name: "report",
    short: "Report Generation",
    wants_series_context: false,
};

pub static SUMMARY: ConsolidationStage = ConsolidationStage {
    name: "summary",
    short: "Change Summary",
    wants_series_context: false,
};

pub static CONSOLIDATION_STAGES: &[&ConsolidationStage] =
    &[&VERIFICATION, &POST_VERIFICATION, &REPORT, &SUMMARY];

fn series_context_placeholder(wants: bool) -> &'static str {
    if wants {
        SERIES_CONTEXT_PLACEHOLDER
    } else {
        ""
    }
}

fn with_series_context(
    template: PromptTemplate<KannakaPatchReviewState>,
    wants: bool,
) -> PromptTemplate<KannakaPatchReviewState> {
    if !wants {
        return template;
    }
    template.with_var("follow_up_series_section", |s: &KannakaPatchReviewState| {
        s.follow_up_series_context
            .as_ref()
            .map(|ctx| format!("\n\n{}", ctx))
            .unwrap_or_default()
    })
}

pub fn analysis_stage_by_name(name: &str) -> Option<&'static AnalysisStage> {
    let normalized = normalize_stage_name(name);
    ANALYSIS_STAGES.iter().find(|s| s.name == normalized)
}

pub fn consolidation_stage_by_name(name: &str) -> Option<&'static ConsolidationStage> {
    let normalized = normalize_stage_name(name);
    if POST_VERIFICATION_STAGE_NAMES.contains(&normalized.as_str()) {
        return Some(&POST_VERIFICATION);
    }
    CONSOLIDATION_STAGES
        .iter()
        .copied()
        .find(|s| s.name == normalized)
}

pub fn stage_short_label(name: &str) -> Option<&'static str> {
    if let Some(def) = analysis_stage_by_name(name) {
        return Some(def.short);
    }
    consolidation_stage_by_name(name).map(|s| s.short)
}

pub fn is_stage_exclusive_guide(name: &str) -> bool {
    ANALYSIS_STAGES
        .iter()
        .flat_map(|def| def.guides)
        .any(|guide| guide.rsplit('/').next() == Some(name))
}

pub fn is_known_stage(name: &str) -> bool {
    let normalized = normalize_stage_name(name);
    analysis_stage_by_name(&normalized).is_some()
        || consolidation_stage_by_name(&normalized).is_some()
        || matches!(normalized.as_str(), "pre-screen" | "planning")
}

// ---------------------------------------------------------------------------
// Validators and Helpers
// ---------------------------------------------------------------------------

fn validate_concerns_output(
    output: &StageConcernsOutput,
    _state: &KannakaPatchReviewState,
) -> Result<(), String> {
    for (idx, concern) in output.concerns.iter().enumerate() {
        let Some(obj) = concern.as_object() else {
            return Err(format!("concerns[{idx}] must be a JSON object."));
        };
        let has_desc = obj
            .get("description")
            .and_then(Value::as_str)
            .is_some_and(|s| !s.trim().is_empty());
        if !has_desc {
            return Err(format!(
                "concerns[{idx}] must have a non-empty 'description' string."
            ));
        }
        let has_reasoning = obj
            .get("reasoning")
            .and_then(Value::as_str)
            .is_some_and(|s| !s.trim().is_empty());
        if !has_reasoning {
            return Err(format!(
                "concerns[{idx}] must have a non-empty 'reasoning' string."
            ));
        }
        if !obj.get("preexisting").is_some_and(Value::is_boolean) {
            return Err(format!(
                "concerns[{idx}] must have a boolean 'preexisting' field (true or false)."
            ));
        }
        if !obj.get("locations").is_some_and(Value::is_array) {
            return Err(format!("concerns[{idx}] must have a 'locations' array."));
        }
    }

    for (idx, dismissed) in output.dismissed_concerns.iter().enumerate() {
        let Some(obj) = dismissed.as_object() else {
            return Err(format!("dismissed_concerns[{idx}] must be a JSON object."));
        };
        let has_desc = obj
            .get("description")
            .and_then(Value::as_str)
            .is_some_and(|s| !s.trim().is_empty());
        if !has_desc {
            return Err(format!(
                "dismissed_concerns[{idx}] must have a non-empty 'description' string."
            ));
        }
        let has_reasoning = obj
            .get("reasoning")
            .and_then(Value::as_str)
            .is_some_and(|s| !s.trim().is_empty());
        if !has_reasoning {
            return Err(format!(
                "dismissed_concerns[{idx}] must have a non-empty 'reasoning' string."
            ));
        }
        if !has_valid_proof_location(dismissed) {
            return Err(format!(
                "dismissed_concerns[{idx}] must include at least one entry in 'locations' with non-empty 'file', 'function_or_symbol', and verbatim disproving 'code_snippet' proving the candidate concern cannot occur. If you do not have concrete code proof, move the candidate issue to 'concerns' instead."
            ));
        }
    }

    Ok(())
}

fn format_concerns_feedback(violation: &str) -> String {
    format!(
        "\n\nPrevious attempt was rejected: {}. You MUST return ONLY a JSON object containing 'concerns' (each with 'type', non-empty 'description' and 'reasoning' strings, a boolean 'preexisting', and a 'locations' array) and 'dismissed_concerns' (each with 'type', non-empty 'description' and 'reasoning' strings, and a 'locations' array) arrays. If there are no concerns and no dismissed concerns, return `{{\"concerns\": [], \"dismissed_concerns\": []}}`.",
        violation
    )
}

fn validate_github_summary_format(
    content: &str,
    state: &KannakaPatchReviewState,
) -> Result<(), String> {
    let trimmed = content.trim();
    if trimmed.is_empty() {
        return Err("The inline review report cannot be empty.".to_string());
    }
    if trimmed.contains('`') {
        return Err(
            "The report contains backticks ('`'). Use strictly plain text without backticks or markdown code blocks."
                .to_string(),
        );
    }
    for line in trimmed.lines() {
        let l = line.trim_start();
        if l.starts_with("# ") || l.starts_with("## ") || l.starts_with("### ") {
            return Err(
                "Do not use markdown headings ('#') in the plain-text review report. Follow github-summary-template.md."
                    .to_string(),
            );
        }
        if l.starts_with("Summary:") || l.starts_with("Findings:") {
            return Err(
                "Do not include 'Summary:' or 'Findings:' headers in the inline review report. Output ONLY the plain bulleted list of findings (or 'No issues found.')."
                    .to_string(),
            );
        }
        if !line.starts_with("    ") && !line.starts_with('\t') && line.chars().count() > 84 {
            return Err(format!(
                "Line exceeds 78-character terminal width ({} chars): \"{}...\". Wrap all prose lines at 78 characters.",
                line.chars().count(),
                line.chars().take(40).collect::<String>()
            ));
        }
    }
    if !state.findings.is_empty() {
        let has_severity_bullet = trimmed.lines().any(|line| {
            let l = line.trim_start();
            l.starts_with("- [CRITICAL]")
                || l.starts_with("- [HIGH]")
                || l.starts_with("- [MEDIUM]")
                || l.starts_with("- [LOW]")
        });
        if !has_severity_bullet {
            return Err(
                "Findings were provided in state, but the report does not list them as bullets starting with '- [CRITICAL]', '- [HIGH]', '- [MEDIUM]', or '- [LOW]'. Include every finding."
                    .to_string(),
            );
        }
    }
    Ok(())
}

pub fn format_kannaka_inline_findings(text: &str) -> String {
    let trimmed = text.trim();
    if trimmed.is_empty() || trimmed == "No issues found." {
        return trimmed.to_string();
    }

    let mut out_lines: Vec<&str> = Vec::new();
    for line in trimmed.lines() {
        let l = line.trim_start();
        let is_bullet_severity = l.starts_with("- [CRITICAL]")
            || l.starts_with("- [HIGH]")
            || l.starts_with("- [MEDIUM]")
            || l.starts_with("- [LOW]")
            || l.starts_with("- [critical]")
            || l.starts_with("- [high]")
            || l.starts_with("- [medium]")
            || l.starts_with("- [low]");

        if is_bullet_severity
            && !out_lines.is_empty()
            && !out_lines.last().unwrap().trim().is_empty()
        {
            out_lines.push("");
        }
        if line.trim().is_empty() {
            if out_lines.last().is_some_and(|prev| !prev.trim().is_empty()) {
                out_lines.push("");
            }
        } else {
            out_lines.push(line.trim_end());
        }
    }
    out_lines.join("\n")
}

fn format_github_summary_feedback(violation: &str) -> String {
    format!(
        "\n\nPrevious attempt was rejected: {}. Follow `github-summary-template.md`: return strictly plain text without backticks, markdown headings, or 'Summary:'/'Findings:' headers; wrap prose lines at 78 characters; separate individual findings with an empty line; and list every finding as a bullet starting with '- [<SEVERITY>]'.",
        violation
    )
}

fn validate_summary_format(content: &str, _state: &KannakaPatchReviewState) -> Result<(), String> {
    let trimmed = content.trim();
    if trimmed.is_empty() {
        return Err("The change summary cannot be empty.".to_string());
    }
    if trimmed.contains('`') {
        return Err(
            "The summary contains backticks ('`'). Use strictly plain text without backticks."
                .to_string(),
        );
    }
    for line in trimmed.lines() {
        let l = line.trim_start();
        let is_indented = line.starts_with("  ") || line.starts_with('\t');
        if !is_indented {
            if l.starts_with('#') || l.starts_with("Summary:") {
                return Err(
                    "Do not use markdown headings ('#') or 'Summary:' prefixes in the summary."
                        .to_string(),
                );
            }
            if line.chars().count() > 84 {
                return Err(format!(
                    "Line exceeds 78-character terminal width ({} chars): \"{}...\". Wrap prose lines at 78 characters.",
                    line.chars().count(),
                    line.chars().take(40).collect::<String>()
                ));
            }
        }
    }
    Ok(())
}

fn format_summary_feedback(violation: &str) -> String {
    format!(
        "\n\nPrevious attempt was rejected: {}. Provide a plain-text summary (including Before: and After: examples if the change has a user-visible effect) without backticks, markdown headings, or 'Summary:' prefixes, wrapped at 78 characters.",
        violation
    )
}

// ---------------------------------------------------------------------------
// Stage Builders
// ---------------------------------------------------------------------------

pub fn prescreen_stage() -> Stage<KannakaPatchReviewState, PrescreenOutput> {
    Stage::builder("pre-screen")
        .system_prompt(PromptTemplate::<KannakaPatchReviewState>::new(
            "You are an AI assistant preparing a Kannaka Labs codebase patch review.\nReview the provided Patch and select all potentially relevant component and pattern guides from the index below.\nCRITICAL BIAS RULE: You MUST err on the side of inclusion. Only exclude a guide if it is 100% irrelevant to the modified code. If there is any doubt, include the file.\n\nYou MUST respond with ONLY a JSON object, no other text. Example:\n```json\n{\"selected_prompts\": [\"workflow-engine.md\", \"rust-async.md\"]}\n```",
        ))
        .user_prompt(
            PromptTemplate::<KannakaPatchReviewState>::new(
                "<subsystem_guide_index>\n@include(\"subsystem/subsystem.md\")\n</subsystem_guide_index>\n\n<patch>\n{{target_commit_diff}}\n</patch>",
            )
            .with_var("target_commit_diff", |s: &KannakaPatchReviewState| {
                s.target_commit_diff.clone()
            })
            .include_file("subsystem/subsystem.md"),
        )
        .output_format(OutputFormat::json_with_schema(json!({
            "type": "object",
            "properties": {
                "selected_prompts": {
                    "type": "array",
                    "items": { "type": "string" }
                }
            },
            "required": ["selected_prompts"]
        })))
        .policy(StagePolicy {
            tools: ToolScope::None,
            max_turns: 1,
            ..Default::default()
        })
        .skip_if(|s| s.manual_stages.is_some())
        .reduce(|state, out: PrescreenOutput| {
            let prompts: Vec<String> = out
                .selected_prompts
                .into_iter()
                .filter(|name| !is_stage_exclusive_guide(name))
                .filter(|name| sanitize_guide_name(name))
                .collect();
            state.selected_guides = prompts;
        })
        .build()
}

pub fn planning_stage() -> Stage<KannakaPatchReviewState, PlanningOutput> {
    let optional_stages: Vec<&'static str> = ANALYSIS_STAGES
        .iter()
        .filter(|s| s.optional)
        .map(|s| s.name)
        .collect();
    let optional_list = optional_stages.join(", ");

    Stage::builder("planning")
        .system_prompt(PromptTemplate::<KannakaPatchReviewState>::new(format!(
            "You are an AI assistant planning a Kannaka Labs patch review.\nThe core stages (goal, implementation, execution-flow) always run.\nSelect which optional specialized stages should also run based on the patch contents.\nAvailable optional stages: [{optional_list}]\n\nCRITICAL BIAS RULE: Err on the side of inclusion. Include any stage whose domain could plausibly be affected by the patch.\n\nRespond with ONLY a JSON object listing the relevant optional stages. Example:\n```json\n{{\"relevant_stages\": [\"concurrency\", \"persistence\"]}}\n```"
        )))
        .user_prompt(
            PromptTemplate::<KannakaPatchReviewState>::new(
                "<patch>\n{{target_commit_diff}}\n</patch>",
            )
            .with_var("target_commit_diff", |s: &KannakaPatchReviewState| {
                s.target_commit_diff.clone()
            }),
        )
        .output_format(OutputFormat::json_with_schema(json!({
            "type": "object",
            "properties": {
                "relevant_stages": {
                    "type": "array",
                    "items": { "type": "string" }
                }
            },
            "required": ["relevant_stages"]
        })))
        .policy(StagePolicy {
            tools: ToolScope::None,
            max_turns: 1,
            ..Default::default()
        })
        .skip_if(|s| s.manual_stages.is_some())
        .reduce(|state, out: PlanningOutput| {
            let mut planned: Vec<String> = ANALYSIS_STAGES
                .iter()
                .filter(|s| !s.optional)
                .map(|s| s.name.to_string())
                .collect();

            for raw in out.relevant_stages {
                if let Some(def) = analysis_stage_by_name(&raw)
                    && !planned.iter().any(|p| p == def.name)
                {
                    planned.push(def.name.to_string());
                }
            }
            state.planned_stages = planned;
        })
        .build()
}

fn analysis_stage(
    def: &'static AnalysisStage,
    max_turns: usize,
    temperature: f32,
) -> Box<dyn ExecutableStage<KannakaPatchReviewState>> {
    let series_context = series_context_placeholder(def.wants_series_context);
    let mut user_template = PromptTemplate::<KannakaPatchReviewState>::new(format!(
        "{}{}\n\n{}",
        def.instruction, series_context, CONCERN_JSON_SCHEMA_EXAMPLE
    ));

    for guide in def.guides {
        user_template = user_template.include_file(*guide);
    }

    let user_template = with_series_context(user_template, def.wants_series_context);

    Box::new(
        Stage::builder(def.name)
            .system_prompt(kannaka_system_prompt(def.uses_commit_log))
            .user_prompt(user_template)
            .output_format(
                OutputFormat::json()
                    .with_validator(validate_concerns_output)
                    .with_feedback_formatter(format_concerns_feedback),
            )
            .policy(StagePolicy {
                tools: ToolScope::All,
                max_turns,
                temperature,
                ..Default::default()
            })
            .reduce_with_outcome(
                move |state: &mut KannakaPatchReviewState,
                      out: StageConcernsOutput,
                      outcome: &crate::workflow::stage::StageOutcome| {
                    let prompts =
                        collect_stage_prompts(&state.selected_guides, def.guides, outcome);
                    append_stage_items_with_prompts(
                        &mut state.all_concerns,
                        &out.concerns,
                        def.name,
                        "General",
                        &prompts,
                    );
                    append_stage_dismissed_concerns_with_prompts(
                        &mut state.all_dismissed_concerns,
                        &out.dismissed_concerns,
                        def.name,
                        &prompts,
                    );
                },
            )
            .build(),
    )
}

pub fn resolve_analysis_stages_with_options(
    state: &KannakaPatchReviewState,
    max_turns: usize,
    temperature: f32,
) -> Vec<Box<dyn ExecutableStage<KannakaPatchReviewState>>> {
    let selected_stages: Vec<String> = if let Some(ref manual) = state.manual_stages {
        manual.clone()
    } else if !state.planned_stages.is_empty() {
        state.planned_stages.clone()
    } else {
        ANALYSIS_STAGES.iter().map(|d| d.name.to_string()).collect()
    };

    let mut stages = Vec::new();
    for name in selected_stages {
        match analysis_stage_by_name(&name) {
            Some(def) => stages.push(analysis_stage(def, max_turns, temperature)),
            None => tracing::warn!("Ignoring unknown Kannaka review stage {:?}", name),
        }
    }
    stages
}

pub fn verification_stage(
    max_turns: usize,
    temperature: f32,
) -> Stage<KannakaPatchReviewState, VerificationOutput> {
    let series_context = series_context_placeholder(VERIFICATION.wants_series_context);
    let user_template = with_series_context(
        PromptTemplate::<KannakaPatchReviewState>::new(format!(
            r#"{STAGE_VERIFICATION_INSTRUCTION}

<false_positive_guide>
@include("false-positive-guide.md")
</false_positive_guide>

<severity_guidelines>
@include("severity.md")
</severity_guidelines>@includes

CRITICAL REVIEW DIRECTIVE: To dismiss a concern as a false positive, you must have concrete evidence in the code that proves the concern is invalid. Never drop a raised concern into 'dismissed_concerns' in this stage: every consolidated concern must be placed either in 'findings' (if well-justified with concrete code proof and no competing dismissal) or in 'hard_cases' (if contested, speculative, or requiring tool verification). Also inspect every standalone dismissed_concern: if it dismissed a plausible bug using an unverified assumption, promote it into 'hard_cases' with '"signal_reason": "speculative_dismissal"'.{series_context}

Aggregated Concerns:
{{{{aggregated_concerns}}}}

Aggregated Dismissed Concerns:
{{{{aggregated_dismissed_concerns}}}}

Return ONLY a JSON object with 'findings', 'hard_cases', and 'dismissed_concerns' arrays.
- LINEAGE REQUIREMENT ('source_ids'): Every item in Aggregated Concerns has an "id" ("C1", "C2", ...) and every item in Aggregated Dismissed Concerns has an "id" ("D1", "D2", ...). Every object in 'findings', 'hard_cases', and 'dismissed_concerns' MUST include a non-empty "source_ids" array of strings listing the exact input "C*" and/or "D*" IDs consolidated into that output item. Every input "C*" and "D*" ID MUST appear in at least one output item's "source_ids". Category 1a 'findings' may ONLY reference "C*" IDs (any contested "C*" + "D*" or promoted "D*" must be routed to 'hard_cases'), and Category 1b 'dismissed_concerns' may ONLY reference "D*" IDs in "source_ids" (never "C*" IDs).
- Each object in 'findings' (Category 1a: Well-Justified Concerns) MUST use the keys: "source_ids" (non-empty array of "C*" IDs only), "problem" (a short naming string under 80 characters starting with a repository/area prefix like 'km-nats:', 'km-cli:', 'km-store:', 'radio:', 'observatory:', 'eye:', 'staff:', 'ktopus:', 'kax-ledger:', 'gsr-store:', 'kshb:', NEVER using backquotes), "severity" ("Low", "Medium", "High", "Critical", or "Unknown"), "severity_explanation" (detailed reasoning and proof), "preexisting" (boolean), and "locations" (array of objects with file, function_or_symbol, line, code_snippet, and why_this_location_matters), and may include "stages" (array of stage names) and "prompts" (array of prompt files).
- Each object in 'hard_cases' (Category 2: Speculative or Contested) MUST use the keys: "source_ids" (non-empty array of input IDs), "type", "description", "estimated_severity" ("Low", "Medium", "High", "Critical", or "Unknown"), "signal_reason" ("mixed_signals", "speculative_concern", "speculative_dismissal", "series_interaction", or "other"), "concern_arguments" (consolidated arguments for why the bug can occur), "dismissal_arguments" (consolidated arguments/snippets from any competing or standalone dismissal, or "" if none), "verification_question" (the specific code question post-verification must answer with tools), "preexisting" (boolean), and "locations" (array of location objects), and may include "stages" and "prompts".
- Each object in 'dismissed_concerns' (Category 1b: Well-Justified Dismissals) MUST use the keys: "source_ids" (non-empty array of "D*" IDs only), "type", "description", "reasoning", and "locations", and may include "stages" and "prompts"."#
        ))
        .include_file("false-positive-guide.md")
        .include_file("severity.md")
        .include_files_from_state(|s: &KannakaPatchReviewState| {
            extra_prompt_paths_for_state(s, analysis_stage_by_name)
        }),
        VERIFICATION.wants_series_context,
    )
    .with_var("aggregated_concerns", |s: &KannakaPatchReviewState| {
        serde_json::to_string_pretty(&s.all_concerns).unwrap_or_default()
    })
    .with_var(
        "aggregated_dismissed_concerns",
        |s: &KannakaPatchReviewState| {
            serde_json::to_string_pretty(&s.all_dismissed_concerns).unwrap_or_default()
        },
    );

    Stage::builder(VERIFICATION.name)
        .system_prompt(kannaka_system_prompt(true))
        .user_prompt(user_template)
        .output_format(
            OutputFormat::json()
                .with_validator(validate_verification_stage_output)
                .with_feedback_formatter(format_verification_stage_feedback),
        )
        .policy(StagePolicy {
            tools: ToolScope::All,
            max_turns,
            temperature,
            ..Default::default()
        })
        .skip_if(|s| s.all_concerns.is_empty() && s.all_dismissed_concerns.is_empty())
        .reduce_with_outcome(|state, out: VerificationOutput, outcome| {
            apply_verification_stage_output(state, out, outcome, analysis_stage_by_name);
        })
        .build()
}

pub fn post_verification_stage(
    stage_name: &'static str,
    batch: Vec<Value>,
    max_turns: usize,
    temperature: f32,
) -> Stage<KannakaPatchReviewState, PostVerificationOutput> {
    let candidate_json = serde_json::to_string_pretty(&batch).unwrap_or_default();
    let batch_for_prompts = batch.clone();
    let batch_for_validate = batch.clone();
    let batch_for_reduce = batch;
    let series_context = series_context_placeholder(POST_VERIFICATION.wants_series_context);
    let user_template = with_series_context(
        PromptTemplate::<KannakaPatchReviewState>::new(format!(
            r#"{STAGE_POST_VERIFICATION_INSTRUCTION}

<false_positive_guide>
@include("false-positive-guide.md")
</false_positive_guide>

<severity_guidelines>
@include("severity.md")
</severity_guidelines>@includes

CRITICAL REVIEW DIRECTIVE: To dismiss a code-behavior candidate issue as a false positive, you must find concrete evidence in the code that proves the issue is invalid and quote that disproving code in `dismissed_concerns[].locations` (for policy-excluded build/linter or out-of-scope meta-concerns under rules 1-2, carry forward the candidate's target location or commit message snippet in `locations`). If you cannot find concrete proof of safety, you must validate and report the finding in `findings`.{series_context}

Candidate Hard Case(s) to Verify:
{{{{candidate_hard_cases}}}}

Return ONLY a JSON object with 'findings' and 'dismissed_concerns' arrays. Every candidate in this batch MUST be accounted for in either 'findings' (if validated) or 'dismissed_concerns' (if disproved by concrete code or excluded by rules 1-2; never return both empty arrays).
- LINEAGE REQUIREMENT ('source_ids'): Every candidate hard case in this batch has an "id" ("H1", "H2", ...). Every object in 'findings' and 'dismissed_concerns' MUST include a non-empty "source_ids" array of strings listing the exact candidate "H*" ID(s) from this batch that it resolves, and every candidate "H*" ID in this batch MUST be accounted for in at least one output item's "source_ids".
- Each object in 'findings' MUST use: "source_ids" (non-empty array of batch "H*" IDs), "problem" (a short naming string under 80 characters starting with a repository/area prefix like 'km-nats:', 'km-cli:', 'km-store:', 'radio:', 'observatory:', 'eye:', 'staff:', 'ktopus:', 'kax-ledger:', 'gsr-store:', 'kshb:', NEVER using backquotes), "severity" (Low, Medium, High, Critical, or Unknown), "severity_explanation" (detailed reasoning and proof), "preexisting" (boolean), "locations" (array of objects with file, function_or_symbol, line, code_snippet, and why_this_location_matters).
- Each object in 'dismissed_concerns' MUST use: "source_ids" (non-empty array of batch "H*" IDs), "description" (the candidate issue that was disproved), "reasoning" (step-by-step explanation of how the inspected code or policy rule disproves the candidate), and "locations" (a non-empty array of objects with file, function_or_symbol, line, code_snippet, and why_this_location_matters, quoting the verbatim disproving code or target snippet)."#
        ))
        .include_file("false-positive-guide.md")
        .include_file("severity.md")
        .include_files_from_state(move |s: &KannakaPatchReviewState| {
            extra_prompt_paths_for_items(&s.selected_guides, &batch_for_prompts, analysis_stage_by_name)
        }),
        POST_VERIFICATION.wants_series_context,
    )
    .with_var("candidate_hard_cases", move |_: &KannakaPatchReviewState| {
        candidate_json.clone()
    });

    Stage::builder(stage_name)
        .system_prompt(kannaka_system_prompt(true))
        .user_prompt(user_template)
        .output_format(
            OutputFormat::json()
                .with_validator(
                    move |out: &PostVerificationOutput, _: &KannakaPatchReviewState| {
                        validate_post_verification_batch_items(out, &batch_for_validate)
                    },
                )
                .with_feedback_formatter(format_post_verification_feedback),
        )
        .policy(StagePolicy {
            tools: ToolScope::All,
            max_turns,
            temperature,
            ..Default::default()
        })
        .reduce_with_outcome(move |state, out: PostVerificationOutput, outcome| {
            apply_post_verification_stage_output(
                state,
                stage_name,
                &batch_for_reduce,
                out,
                outcome,
                analysis_stage_by_name,
            );
        })
        .build()
}

pub fn post_verification_stage_for_batch(
    stage_name: &'static str,
    batch: Vec<Value>,
    max_turns: usize,
    temperature: f32,
) -> Box<dyn ExecutableStage<KannakaPatchReviewState>> {
    Box::new(post_verification_stage(
        stage_name,
        batch,
        max_turns,
        temperature,
    ))
}

pub fn resolve_post_verification_stages_with_options(
    state: &KannakaPatchReviewState,
    max_turns: usize,
    temperature: f32,
) -> Vec<Box<dyn ExecutableStage<KannakaPatchReviewState>>> {
    let batches = batch_hard_cases_by_severity(&state.hard_cases);
    batches
        .into_iter()
        .enumerate()
        .map(|(idx, batch)| {
            let stage_name = POST_VERIFICATION_STAGE_NAMES[idx];
            post_verification_stage_for_batch(stage_name, batch, max_turns, temperature)
        })
        .collect()
}

pub fn report_stage(max_turns: usize, temperature: f32) -> Stage<KannakaPatchReviewState, String> {
    Stage::builder(REPORT.name)
        .system_prompt(kannaka_system_prompt(true))
        .user_prompt(
            PromptTemplate::<KannakaPatchReviewState>::new(format!(
                r#"{STAGE_REPORT_INSTRUCTION}

<report_template>
@include("github-summary-template.md")
</report_template>

Findings:
{{{{findings}}}}

Return strictly plain text output (no markdown, no backticks, wrapped at 78 characters), not JSON."#
            ))
            .include_file("github-summary-template.md")
            .with_var("findings", |s: &KannakaPatchReviewState| {
                serialize_findings_for_prompt(&s.findings)
            }),
        )
        .output_format(OutputFormat::text_with_validator(
            validate_github_summary_format,
            format_github_summary_feedback,
        ))
        .policy(StagePolicy {
            tools: ToolScope::All,
            max_turns,
            temperature,
            recitation_policy: RecitationPolicy::FallbackToFreeForm {
                reminder: "Do not quote large blocks of code verbatim. Summarize concisely."
                    .to_string(),
            },
            ..Default::default()
        })
        .skip_if(|s| s.skip_report || s.findings.is_empty())
        .reduce(|state, out: String| {
            state.review_inline = format_kannaka_inline_findings(&out);
        })
        .build()
}

pub fn summary_stage(
    _max_turns: usize,
    temperature: f32,
) -> Stage<KannakaPatchReviewState, String> {
    Stage::builder(SUMMARY.name)
        .system_prompt(kannaka_system_prompt(true))
        .user_prompt(PromptTemplate::<KannakaPatchReviewState>::new(
            STAGE_SUMMARY_INSTRUCTION,
        ))
        .output_format(OutputFormat::text_with_validator(
            validate_summary_format,
            format_summary_feedback,
        ))
        .policy(StagePolicy {
            tools: ToolScope::None,
            max_turns: 1,
            temperature,
            recitation_policy: RecitationPolicy::FallbackToFreeForm {
                reminder:
                    "Summarize the change concisely in plain text (including Before: and After: examples if user-visible) without quoting large blocks verbatim."
                        .to_string(),
            },
            ..Default::default()
        })
        .skip_if(|s| s.skip_report)
        .reduce(|state, out: String| {
            state.summary = out.trim().to_string();
        })
        .build()
}

// ---------------------------------------------------------------------------
// Complete Kannaka Labs Review Workflow Graph
// ---------------------------------------------------------------------------

pub fn build_kannaka_patch_review_workflow() -> Workflow<KannakaPatchReviewState> {
    build_kannaka_patch_review_workflow_with_options(20, 0.0)
}

/// Builds the Kannaka Labs patch review workflow (same graph as Sashiko's).
///
/// Unlike [`crate::workflows::linux_patch_review::build_linux_patch_review_workflow_with_options`],
/// this workflow ends with an unconditional [`summary_stage`] that generates a
/// 1-2 sentence commit summary even when zero findings are produced. Therefore,
/// short-circuiting uses `skip_if` on [`verification_stage`] (when both
/// `all_concerns` and `all_dismissed_concerns` are empty), 0-stage fan-out in
/// [`resolve_post_verification_stages_with_options`] (when `hard_cases` is
/// empty), and `skip_if` on [`report_stage`] (when `findings` is empty) rather
/// than `early_exit_if`, which would abort the workflow before `summary_stage`.
pub fn build_kannaka_patch_review_workflow_with_options(
    max_turns: usize,
    temperature: f32,
) -> Workflow<KannakaPatchReviewState> {
    Workflow::builder("kannaka_patch_review")
        .stage(prescreen_stage())
        .dynamic_parallel(
            planning_stage(),
            move |state| resolve_analysis_stages_with_options(state, max_turns, temperature),
            ParallelPolicy::BestEffort,
        )
        .dynamic_parallel(
            verification_stage(max_turns, temperature),
            move |state| {
                resolve_post_verification_stages_with_options(state, max_turns, temperature)
            },
            ParallelPolicy::BestEffort,
        )
        .stage(report_stage(max_turns, temperature))
        .stage(summary_stage(max_turns, temperature))
        .build()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_kannaka_analysis_stages_table() {
        let names: Vec<&str> = ANALYSIS_STAGES.iter().map(|s| s.name).collect();
        assert_eq!(
            names,
            vec![
                "goal",
                "implementation",
                "execution-flow",
                "concurrency",
                "wire-contracts",
                "platform",
                "security",
                "interfaces-compat",
                "tests"
            ]
        );
        // The three core stages always run; the rest are chosen by the planner.
        for s in ANALYSIS_STAGES {
            let core = matches!(s.name, "goal" | "implementation" | "execution-flow");
            assert_eq!(s.optional, !core, "{} optional flag", s.name);
        }
    }

    #[test]
    fn test_kannaka_stage_guides_exist_in_the_prompt_tree() {
        // A guide named in the stage table but missing from prompts/kannaka is
        // silently never loaded, so the reviewer runs without it.
        let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("prompts/kannaka");
        for s in ANALYSIS_STAGES {
            for g in s.guides {
                assert!(root.join(g).is_file(), "stage {} names missing guide {}", s.name, g);
            }
        }
        for f in ["review-core.md", "subsystem/subsystem.md", "severity.md", "false-positive-guide.md", "github-summary-template.md", "prompt-injection.md"] {
            assert!(root.join(f).is_file(), "missing {f}");
        }
    }

    #[test]
    fn test_kannaka_does_not_inherit_the_unix_only_rule() {
        // Sashiko's own stages forbid reporting Windows portability issues.
        // Kannaka Labs code runs on Windows desktops, so that rule must not
        // survive into any Kannaka stage text.
        for s in ANALYSIS_STAGES {
            let t = s.instruction.to_lowercase();
            assert!(!t.contains("never report windows"), "{} carries the Unix-only rule", s.name);
            assert!(!t.contains("lack of windows support"), "{} carries the Unix-only rule", s.name);
        }
        let platform = analysis_stage_by_name("platform").expect("platform stage");
        assert!(platform.instruction.contains("Windows"));
    }

    #[test]
    fn test_kannaka_stage_lookup_and_labels() {
        assert!(is_known_stage("wire-contracts"));
        assert!(is_known_stage("platform"));
        assert!(!is_known_stage("persistence"), "Sashiko's DB stage is not a Kannaka stage");
        assert!(!is_known_stage("llm-pipeline"));
        assert_eq!(stage_short_label("wire-contracts"), Some("Wire Contracts"));
        assert_eq!(stage_short_label("platform"), Some("Windows & Linux"));
    }

    #[test]
    fn test_kannaka_workflow_name() {
        let wf = build_kannaka_patch_review_workflow();
        assert_eq!(wf.name, "kannaka_patch_review");
    }

    #[test]
    fn test_kannaka_verification_keeps_the_proof_bar() {
        // The consolidation rules are what keep false positives down; the
        // Kannaka text must keep them.
        assert!(STAGE_VERIFICATION_INSTRUCTION.contains("CRITICAL INVARIANT"));
        assert!(STAGE_VERIFICATION_INSTRUCTION.contains("speculative_dismissal"));
        assert!(STAGE_POST_VERIFICATION_INSTRUCTION.contains("SYMMETRICAL PROOF BAR"));
        assert!(STAGE_POST_VERIFICATION_INSTRUCTION.contains("ALL-CALLERS"));
    }
}
