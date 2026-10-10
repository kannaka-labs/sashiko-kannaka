# Sashiko Kannaka

Kannaka Labs' code reviewer: a fork of [Sashiko](https://github.com/sashiko-dev/sashiko), the agentic patch
reviewer the Linux kernel runs on its mailing lists, equipped for the Kannaka Labs codebases.

Upstream Sashiko reviews kernel C with kernel prompts. This fork adds a third project, **`kannaka`**, that reviews
Kannaka Labs services: Rust, TypeScript, JavaScript and Python programs that run on Linux servers and Windows
desktops, talk over NATS, HTTP and each other's command-line output, and handle real money, public accounts and
live memory stores. Everything upstream does still works unchanged (`--project linux`, `--project sashiko`).

Upstream's own README is kept at [docs/UPSTREAM-README.md](docs/UPSTREAM-README.md).

## What it reviews for

The `kannaka` project ranks what a bad change costs here, in this order:

1. **Being told something false.** A failure that looks like success: an empty result that was really an error,
   a 200 with an error inside, a test that cannot fail, a measurement scored from a malformed input.
2. **Money, credentials and public surfaces.** Value moved without a verified authorization, a secret in a log or
   a prompt, a post or mail sent twice or without consent.
3. **Live state.** A memory store written by two processes, a read-looking command that writes, a retry loop
   that treats a permanent refusal as transient.
4. **Contracts between programs.** One program changing what it prints or publishes while another still parses
   the old shape.

Windows behaviour is in scope. Upstream's own service prompts tell the reviewer never to report portability
issues; the Kannaka stages do the opposite, and a test keeps it that way.

## How a review runs

The same graph as upstream: pre-screen, planning, parallel analysis stages, verification, per-finding
post-verification with repository tools, report and summary. The Kannaka analysis stages are:

| Stage | When | Looks for |
|---|---|---|
| `goal` | always | intent, single responsibility, claims in the commit message that the diff cannot back |
| `implementation` | always | incomplete changes at boundaries, including readers in other repositories |
| `execution-flow` | always | silent failure, crashes on runtime input, exit codes, timeouts |
| `concurrency` | planner | single-writer stores, locks, child processes, retry and reconnect loops |
| `wire-contracts` | planner | NATS subjects and ACLs, HTTP status and shape, JSON and binary output contracts |
| `platform` | planner | Windows and Linux differences: socket handles, paths, `~`, CRLF, BOM, signals, shims |
| `security` | planner | secrets, money, live stores, untrusted input, prompt injection |
| `interfaces-compat` | planner | CLI flags and `--help`, configuration, public surfaces |
| `tests` | planner | tests that cannot fail, hand-written fixtures, shared test state |

Verification keeps upstream's proof bar: a finding needs concrete code evidence, a dismissal needs it too, and
anything contested goes to tool-assisted post-verification.

## The prompt set

[`prompts/kannaka/`](prompts/kannaka/README.md), compiled into the binary:

- `review-core.md`, `severity.md`, `false-positive-guide.md`, `prompt-injection.md`, `github-summary-template.md`
- nine cross-cutting patterns: silent failure, error handling, concurrency, retries and refusals, wire contracts,
  Windows and Linux, secrets and money, public surfaces, tests that can fail
- ten component guides, each written from the repository's default branch and anchored to real symbols:

| Guide | Covers |
|---|---|
| `km-nats.md`, `km-store.md`, `km-cli.md`, `km-ci-release.md` | kannaka-memory: NATS transport and refusals, the HRM store, the `kannaka` CLI contract, CI and release |
| `radio.md` | kannaka-radio server |
| `node-http-services.md` | kannaka-observatory, kannaka-eye, kannaka-staff |
| `kannaktopus.md` | Kannaktopus MCP server, scripts and hooks |
| `kax-ledger.md` | Agent-Kax credit ledger, offers and escrow |
| `gsr-store.md` | the USDC record store and its gasless relayer |
| `kshb-harness.md` | the KSHB research harness |

The pre-screen stage picks guides from [`subsystem/subsystem.md`](prompts/kannaka/subsystem/subsystem.md) by path
and symbol. When code changes, the guide that describes it should change in the same pull request.

Writing these guides found real defects, all fixed since: a rate limit keyed on a header the caller controls and
a claimable anonymous purchase in the store; `--help` running `swarm join` and re-encoding a store in
kannaka-memory; an events stream that stored nothing; tagged memories that never stored in Kannaktopus; unguarded
admin routes on the radio; instruments that scored non-measurements in KSHB.

## Running it

Sashiko targets Unix and does not compile on Windows. On a Windows desktop, run it in WSL.

**Build** (Rust 1.90 or newer; Ubuntu needs `pkg-config libssl-dev cmake build-essential`):

```bash
git clone https://github.com/kannaka-labs/sashiko-kannaka && cd sashiko-kannaka
git submodule deinit -f third_party/linux 2>/dev/null   # the kernel tree is only for --project linux
CARGO_BUILD_JOBS=4 cargo build --release                 # ~16 min cold; keep jobs low on a shared desktop
ln -sf "$PWD/target/release/sashiko" /usr/local/bin/sashiko-kannaka
```

**Configure** with [`docs/examples/Settings.kannaka.toml`](docs/examples/Settings.kannaka.toml): the Claude Code
CLI provider (no API key; it uses a Claude Code subscription), `model = "sonnet"`, and the fields `sashiko review`
requires.

**From WSL, reach the Windows Claude Code CLI** with [`scripts/kannaka/install-wsl-claude-bridge.sh`](scripts/kannaka/install-wsl-claude-bridge.sh).
It installs `/usr/local/bin/claude` as a bridge to the signed-in Windows `claude.exe`.

**Review a commit** from a clone inside WSL (not under `/mnt/c`, so no Windows project memory is loaded):

```bash
cd ~/repos/kannaka-memory
sashiko-kannaka review --settings ~/Settings.kannaka.toml --project kannaka HEAD
sashiko-kannaka review --settings ~/Settings.kannaka.toml --project kannaka --format json HEAD~3..HEAD > review.json
```

A small commit takes about 8 minutes and about 300k input tokens. Nothing is posted anywhere; local review
uses a scratch clone and leaves the working tree alone.

## Isolation of the model

The `claude-cli` provider runs the CLI with `--tools "" --strict-mcp-config --setting-sources ""`. Without
those flags Claude Code loads the operator's settings, plugins and MCP servers and gives the reviewing model the
operator's whole toolset (measured: about 19,000 tokens of context, including memory tools). With them the model
sees only the prompt Sashiko builds (about 3,000 tokens). The flags live in `claude_cli_args()` in
`src/ai/claude_cli.rs`, with a test.

## Status

- Merged and running locally. The fork has no CI yet: GitHub does not run inherited workflows on a new fork
  until they are enabled. Verification so far: release build clean; the 15 Kannaka and project tests pass; the
  library suite passes 988 of 989, the one failure being an upstream `git_ops` test that disagrees with the older
  git on Ubuntu 24.04; an end-to-end review of a kannaka-memory commit.
- **Not supported yet for `kannaka`:** the daemon's pre-existing-bug tracker. Its prompts are written for a
  single Linux or Sashiko tree, and the daemon refuses `linux_bug.enabled` with `--project kannaka`.
- **Evaluation** is pre-registered in kannaka-scientist (`docs/specs/2026-10-09-sashiko-trial-prereg.md`): stock
  Sashiko as a clean baseline on nine known bugs and four controls; the Kannaka arm on those nine is reported
  as contaminated, because the prompts were written by the person who fixed them; the real test is four weeks of
  new kannaka-labs pull requests reviewed both ways, nothing posted, scored as defects surface.

## Code map of the fork

| Change | Where |
|---|---|
| The `kannaka` project | `src/project.rs` (`ProjectId::Kannaka`, prompt dir `kannaka`) |
| The workflow and stages | `src/workflows/kannaka_patch_review.rs` |
| Routing | `src/workflows/mod.rs`, `src/workflows/review_map.rs`, `src/worker/prompts.rs`, `src/prompt_bundle.rs`, `src/reviewer.rs` |
| Bug tracker refusal | `src/main.rs` (`run_daemon`); tracker arms shared in `src/workflows/linux_bug.rs`, `src/worker/bug_worker.rs` |
| CLI isolation | `src/ai/claude_cli.rs` (`claude_cli_args`) |
| Prompts | `prompts/kannaka/` |

Kannaka Labs changes are marked in the files they touch. To pull upstream improvements:
`git fetch upstream && git merge upstream/main` (the fork only adds files and match arms, so conflicts are rare).

## License

Apache-2.0, as upstream. See [LICENSE](LICENSE). Sashiko is a Linux Foundation project by its authors; this
fork is not affiliated with or endorsed by them.
