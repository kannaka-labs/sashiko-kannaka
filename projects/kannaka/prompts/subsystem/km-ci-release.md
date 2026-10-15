# kannaka-memory: CI Gates, Lint Policy, Tests and Releases

This guide covers `.github/workflows/`, `Cargo.toml` features, the test
environment, `CHANGELOG.md`, and the `v*` tag release path. Use it to judge
whether a change is actually exercised by CI, whether a diff follows the
repository's lint and formatting policy, and whether a release-shaped change
is complete. It says nothing about whether Rust compiles; CI checks that.

## 1. What `ci.yml` gates

Trigger: `push` and `pull_request` on `master`. One job, `ubuntu-latest`,
stable toolchain:

1. Checks out `kannaka-memory`, then `consciousness-core` and
   `kannaka-attention` at `v<version>` **read from `Cargo.lock`** (the step
   "Resolve sibling versions from Cargo.lock"). Both are `path` dependencies
   (`../consciousness-core`, `../kannaka-attention`); the lockfile is what
   pins them. A missing sibling tag fails the job.
2. `cargo check --all-targets` (default features).
3. `cargo test --lib --bins --test nats_contract_conformance --test
   attention_gravity_e2e --test autoresearch_cron`.
4. `cargo test --features bridge --lib hive_bridge::` and
   `cargo build --features bridge --bins`.
5. `cargo clippy --lib --bin kannaka`.

Consequences for review:

- **Integration tests under `tests/` run only if named.** Not run in CI:
  `acp_dispatch`, `audio_integration`, `codebook_health`, `dolt_integration`,
  `inc1_corroboration`, `json_envelope`, `qubo_corpus`, `qubo_solver`,
  `sga_consistency`. A diff whose only test is a new file under `tests/` is not
  gated unless it also adds `--test <name>` to the Test step. A diff that adds
  such a test and claims CI coverage is overstating it.
- **Only Linux is tested.** Windows and macOS are built only by `release.yml`,
  after the tag. Platform-specific behaviour (socket timeouts on `try_clone`d
  handles, `#[cfg(unix)]` `flock` in `try_acquire_write_lock`, path handling)
  is unverified by CI; a fix for one of them needs a test that would fail on
  that platform or an explicit statement of where it was run.
- **Default features only**, except `bridge` (step 4). Default is
  `["hrm", "nats", "glyph", "video", "nostr", "mail"]`; `collective` is never
  compiled in CI. Bin-level unit tests behind `#[cfg(all(test, feature =
  "nats"))]` run because `nats` is default.
- **The bridge step compiles bins with `--features bridge`** because
  `kannaka-nostr-bridge` and `kannaka-hive-bridge` have
  `required-features = ["bridge"]`; `--bins` in step 3 skips them.

## 2. Clippy and formatting policy

- Clippy is scoped to `--lib --bin kannaka` (the CI comment: other targets
  carry pre-existing `clippy::correctness` errors). It is invoked **without
  `-- -D warnings`**, despite the workflow's header comment saying `-D
  warnings` applies on the clippy step. Only error-level (deny-by-default)
  lints fail the build. A toolchain update that promotes a lint to deny can
  turn every PR red; the repository's response is a targeted
  `#[allow(clippy::<lint>)]` with a reason comment at the site (see the two
  `clippy::approx_constant` allows in `src/glyph_bridge.rs` and
  `src/store.rs`), not a crate-wide allow.
- **Formatting is not checked and must not be applied repo-wide.** The Fmt
  check is disabled in `ci.yml` because the codebase predates rustfmt and a
  full sweep "would be a 10K-line cosmetic diff". There is no `rustfmt.toml`.
  A diff that runs `cargo fmt` over the tree (or over whole files it otherwise
  touches lightly) buries the real change in reformatting and should be
  flagged as unrelated churn. Formatting the lines a change actually edits is
  fine.
- Style-only clippy findings (warn level) are not review findings here.

## 3. Tests and the process environment

Recall ranking reads environment variables **per call** in
`src/medium/hemisphere.rs`: `recall_energy_exp()` (`KANNAKA_RECALL_ENERGY_EXP`)
and `recall_temporal_exp()` (`KANNAKA_RECALL_TEMPORAL_EXP`, plus
`KANNAKA_RECALL_TEMPORAL_HALFLIFE_DAYS` and `KANNAKA_RECALL_TEMPORAL_FLOOR`).
Both default to `0.0`, and CI runs with neither set. Every recall-ranking
test in the library therefore runs under whatever the invoking shell exports;
a shell prepared for the eval harnesses (`evals/*/run_probes.py` set
`KANNAKA_RECALL_ENERGY_EXP=0.0` and `KANNAKA_RECALL_TEMPORAL_EXP=1.0`)
changes ranking under the tests, and a local pass or failure no longer
predicts CI. Run tests with both unset:

```
env -u KANNAKA_RECALL_TEMPORAL_EXP -u KANNAKA_RECALL_ENERGY_EXP cargo test --lib --bins ...
```

The same leak applies to other process-global knobs read at use time:
`KANNAKA_READONLY` (read by `HrmStore::new`/`load`; set, every persistence
test silently stops persisting), `KANNAKA_DATA_DIR`, `KANNAKA_RECALL_OBSERVE`,
`KANNAKA_RECALL_BEAM`, `KANNAKA_DREAM_ENTROPY`, `NATS_USER`/`NATS_PASSWORD`.
`KANNAKA_RECALL_XI_BOOST` is cached in a `OnceLock`, so it cannot be toggled
inside one test binary at all.

Rules a test diff must follow:

- A test that sets a process-global env var or static must restore it on every
  exit (guards like `IdEnvGuard` in `src/config.rs`, `SuppressTemporalScoring`)
  and serialize against siblings that read it (`REFUSALS_TEST_SERIAL`,
  per-test `static SERIAL: Mutex<()>` in `src/nats.rs`). Tests run in parallel
  threads of one process.
- Test-only resets exist for process-wide NATS memory
  (`reset_stream_create_denied_for_test`, `reset_subscribe_refusals_for_test`,
  `reset_disk_seed_for_test`, `TEST_REFUSALS_FILE`). A test that sets one of
  those flags without resetting it changes the behaviour of every later test.
- Tests must never touch a real data dir or run a `kannaka` binary against
  one; use `tempfile` (dev-dependency) and an explicit path. `--help` on a
  kannaka subcommand runs it (see the CLI guide).
- NATS integration tests use `connect_or_skip()` and skip when no broker is
  reachable; they prove nothing in CI. Broker-behaviour tests that must gate
  use in-process fake brokers on loopback (`memory_event_confirm_tests::broker`,
  `loopback_pair`).
- The repository's own changelog standard for a fix is that the test fails
  without the fix (single-mutant checks are described in many entries). A test
  that would pass with the fix reverted is not evidence.

## 4. Releases

- Trigger: pushing a tag matching `v*` runs `release.yml` (and
  `notify-marketplace.yml`). The release workflow **does not run tests or
  clippy**; it trusts that the tagged commit passed `ci.yml` on `master`.
- Build matrix: `x86_64-pc-windows-msvc`, `x86_64-unknown-linux-musl`,
  `aarch64-unknown-linux-musl`, `x86_64-apple-darwin`, `aarch64-apple-darwin`;
  binaries `kannaka` and `kannaka-acp` only. Siblings are pinned from
  `Cargo.lock` exactly as in CI; a missing `v<version>` tag on a sibling fails
  the release rather than falling back to its default branch.
- macOS binaries are codesigned before checksumming, notarised after, and the
  sidecars re-verified (`shasum -a 256 -c`); every artifact ships with a
  `<artifact>.sha256` in `sha256sum` format, which `kannaka update` verifies.
  Renaming an artifact breaks installers and `self_update`; the comment on the
  musl targets says the names are kept for that reason.
- `publish-channels.yml` is called with `version: ${{ github.ref_name }}`.
- Nothing checks that the tag equals `Cargo.toml`'s `version`. `config::VERSION`
  is `env!("CARGO_PKG_VERSION")`, and `kannaka update` / the background update
  check compare it with the latest release's `tag_name` stripped of `v`
  (`version_is_newer`). A tag that does not match `Cargo.toml` produces a binary
  that misreports its version and is offered "updates" to itself.
- The release commit (`release: vX.Y.Z`, merged by PR, then an annotated tag on
  the merge) changes exactly `Cargo.toml` `version`, the package's own entry in
  `Cargo.lock`, and `CHANGELOG.md`.
- Sibling bumps arrive as PRs from `cc-release-cascade.yml` /
  `ka-release-cascade.yml` (`repository_dispatch`) that edit `Cargo.lock`;
  that PR's CI run is the compatibility check against the new sibling.

## 5. CHANGELOG expectations

`CHANGELOG.md` starts with `## [Unreleased]`; released sections are
`## [X.Y.Z] — YYYY-MM-DD`, newest first. Each entry is a `###` heading that
states the behaviour change as a sentence (with the issue/PR reference in
parentheses), followed by prose: what was wrong and how it showed, what the
code does now (naming the symbols, env vars and output lines), what it
deliberately does not do, and how it was proven (tests, mutants, measurements).
Doc/CI/tooling-only changes are grouped or marked "nothing in the binary
changes". A user-visible change (CLI output, exit code, env var, NATS subject
or payload, on-disk file) without an `[Unreleased]` entry is incomplete.

## Bug patterns to look for

1. New test placed under `tests/` without a `--test` entry in `ci.yml`, described as gated.
2. Edits to `ci.yml` that drop a named `--test`, the bridge step, or the lockfile pinning of siblings (unpinned siblings make CI test code no release ships).
3. A fix for Windows/macOS-only behaviour verified only by Linux CI.
4. A crate-wide `#![allow(...)]` or a clippy scope widening/narrowing slipped into an unrelated change; whole-file reformatting.
5. A test that mutates `KANNAKA_*` env or a process-global flag without restore and serialization.
6. A release PR whose `Cargo.toml` version, `Cargo.lock` entry, CHANGELOG heading and intended tag disagree.
7. Release-workflow edits that sign or modify a binary after its `.sha256` is computed.

## Not a bug here

- No `cargo fmt --check` and no `-D warnings`: deliberate (stated in `ci.yml`).
- Clippy findings in `src/bin/research.rs` or tests: outside the clippy scope by design.
- Integration tests that skip without a NATS broker.
- `release.yml` not re-running tests.
