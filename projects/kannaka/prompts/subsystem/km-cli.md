# kannaka-memory: CLI Contract (`src/bin/kannaka.rs`, `src/bin/handlers/`)

`kannaka` is a single binary whose stdout is consumed by other programs
(services that shell out to it and parse JSON, cron jobs, the TUI) and whose
side effects land in a live memory store. Dispatch is a large
`match args[command_start]` in `main()` (`src/bin/kannaka.rs`) plus extracted
handler groups under `src/bin/handlers/` (`swarm.rs`, `ask.rs`, `ops.rs`,
`substrate.rs`, `inbox.rs`, `services.rs`, `identity.rs`, ...), each pulled in
with `#[path = "handlers/<x>.rs"] mod handlers_<x>`. `src/cli.rs` holds the
clap tree (`build_cli`), `parse`, plugin exec and `print_envelope`.

## 1. Dispatch order in `main()` (what loads the store, what does not)

1. If any arg is `--help`, `-h` or `help`, or the first arg is not in
   `is_builtin_subcommand`, `cli::parse` runs clap. It returns `Handled`
   (help/version/`--list-plugins`/`completions`/`update`), `Plugin` (exec
   `kannaka-<verb>` from `PATH`, or a `KNOWN_ALIASES` binary), or `Builtin`
   (fall through with the original `args`).
2. `init` (wizard), `swarm tail`, `mail`, `swarm activate-gate`,
   `swarm beacon`: no HRM.
3. `KannakaConfig::load()`, the `apply_*_env_from_config` bridges (env wins;
   they only set unset vars, before any thread spawns), then
   `check_for_updates_background` (an HTTP thread when `updates.auto_check`
   and the last check is older than 24 h; it rewrites `config.toml`'s
   `last_checked`).
4. No-HRM commands: `classify`, `cross-modal-dream`, `radio`, `market`,
   `constellation`, `orchestrate`, `config`, `compute`, `identity`,
   `reputation`, `nostr`, `belief on|off|history|cores`, and
   `recall --remote|--collective` (`handle_networked_recall`).
5. `dream` probes the write lock and exits 0 if held.
6. `init_with_hrm` loads (or creates) the store. Everything after this point
   pays the full load and holds a live `HrmStore` whose `Drop` saves if dirty.

A new command that does not need the store belongs before step 6. A command
added after it becomes another process that can flush over the writer's
`.hrm`; long-running readers must set read-only as `handle_swarm_serve` and
`handle_attention_serve` do (`std::env::set_var("KANNAKA_READONLY", "1")`
and `set_readonly(true)`).

`is_builtin_subcommand` must stay in sync with `build_cli()`; the comment on it
says so. `completions` and `update` are intentionally absent so they always go
through clap.

## 2. The `--help` hazard: subcommands run

Most subcommands are clap `passthrough` / `passthrough_doc` entries (or
`swarm`, `events`, `substrate`, `attention`, `facets`, `dedupe`, `inbox`, ...)
with one `trailing_var_arg(true).allow_hyphen_values(true)` positional. Once
that positional has taken its first token, clap treats every later token,
`--help` included, as a value and returns `Dispatch::Builtin`; the legacy
handler then **runs the command**. `kannaka swarm join --help` joins the
swarm; the handlers do not look for `--help` (only `compute` and `nostr`
match it, in the sub-verb position). Only `recall`, `update` and
`completions` declare real clap arguments. The sibling binary
`kannaka-recompute-encoding` (`src/bin/recompute_encoding.rs`) treats every
argument except `--dry-run` as optional and re-encodes the store at
`KANNAKA_DATA_DIR` when run as `--help`.

For review: a test, script, CI step or doc that runs any kannaka binary with
`--help` against a real data dir is a mutation of that store. A diff that adds
`--help` handling to one handler does not make the others safe.

## 3. stdout / stderr contract

- stdout carries the result, and for machine-facing commands only the result:
  `recall` (JSON array, or envelope), `recall --remote` (the `results` array),
  `recall --collective` (the reply JSON), `export-json` (bare array),
  `status` (pretty JSON object), `remember` (the new id, one line),
  `remember --batch` (one line per input line: an id or `error: line N: ...`),
  `recall --batch` (one JSON array or `{"error":...}` per input line),
  `import-json` (summary object), `ask` (the answer text).
- Diagnostics go to stderr with a bracketed tag: `[nats]`, `[hrm]`,
  `[encoder]`, `[ncs]`, `[lock]`, `[events]`, `[config]`, the "Using HRM
  backend" and "Loading existing HRM file" lines (suppressed by
  `KANNAKA_QUIET`), and the update notice from the background thread.
- `usage()` prints to stderr and exits 1; clap's explicit help goes to stdout.
- Wire-sourced text (peer replies, server error text) is printed through
  `kannaka_memory::sanitize_display`. A new path that prints peer-supplied
  bytes raw is a terminal-injection bug.

A diff that adds a `println!` of a progress or warning line to a command whose
stdout is JSON breaks every parser of that command.

## 4. Exit codes

| Code | Meaning in this tree |
|---|---|
| 0 | success; also `dream` skipped because another process holds the write lock |
| 1 | operational failure: HRM init failed, store error, missing required positional, NATS connect failed, `ask` transport error, `remember --batch` with any failed line, `update --check` when a newer release exists |
| 2 | bad invocation: `flag_value` missing value, `parse_flag_value` unparsable, unknown flag (`remember`, `recall`, `export-recall-scenarios`), bad RFC 3339 (`--at`, `--effective`, ...), unreadable `--batch` file, encoder refusal (`build_encoding_pipeline`); also "no reply" (`recall --remote` timeout, `ask --remote broadcast` with zero replies) |
| 3 | a reply arrived but was not usable: `networked_recall_output` error, `ask` peer failure, replies with no answer, hop ceiling refusal |

`remember` and `recall` reject unknown `--flags` with exit 2 rather than
folding them into the memory text or query. Other handlers vary (`dream`
ignores unknown args; `events restore` warns via `warn_unknown_flag`). A diff
that loosens the strict ones reintroduces typo-into-content.

## 5. Recall output

- Local `recall <query>` prints a compact JSON array of
  `{id, content, similarity, strength, age_hours, layer, times_seen}`;
  `--envelope` wraps it with `print_envelope("recall", ...)` as
  `{schema_version, command, data, errors}`. There is **no `--json` flag**;
  `--json` is an unknown flag and exits 2. The usage strings say this
  explicitly.
- After printing, it calls `sys.flush_reactivation()`; recall is not
  read-only (observation, reactivation sidecar).
- `--top-k`/`--limit`, `--at`, `--nats-url` (accepted, unused locally),
  `--batch FILE`.

## 6. Networked recall (`handle_networked_recall`)

Routed before the HRM load when `--remote` or `--collective` is present.
Uses `SwarmTransport::connect_request_only` (no JetStream probes) and
`request_one` with `--timeout` (default 8 s). `--remote` asks
`KANNAKA.recall.<agent-id>` (`--agent-id` or `cfg.agent.id`); otherwise
`KANNAKA.substrate.recall`. Connect failure exits 1, no reply exits 2.
`networked_recall_output`: `--remote` requires an object with a `results`
array and prints only that array (an empty array is a real answer); a non-JSON
reply, a missing `results` (the responder's `error` string is quoted) or a
non-array `results` is an error, exit 3. `--collective` requires JSON and
prints it unchanged. Tests in `networked_recall_output_tests` pin each case; a
diff that maps a broken reply to `[]` with exit 0 must not pass review.

## 7. Networked commands

NATS: `swarm *` (join, serve, listen, sync, tail, peers, brief, ...), `events
*`, `substrate *`, `inbox *`, `attention serve`, `ask --remote`, `recall
--remote|--collective`, `compute` (also HTTP), and the publish side effects of
`remember` (memory-sync on `KANNAKA.memory.new`, the `.remember` event from
`save()`, `--substrate`) and `dream` (dream report, consciousness, dream
lifecycle). `remember --batch` never publishes. HTTP: `update`, the background
update check, `radio`, `market`, `constellation`, `identity`, `research`
(OpenAlex), `mail` (IMAP/JMAP), `events restore --from-url`, the `ollama`
encoder whenever text is encoded, and `embedder_reachable` when a new store is
created without an `.encoder` stamp.

Broker selection is `resolve_nats_url(args, start, &cfg.swarm.nats_url)`:
`--nats-url` > `KANNAKA_NATS_URL` (folded into config) > `config.toml` >
`DEFAULT_NATS_URL`. Every command that accepts `--nats-url` must consume it
with `flag_value` so the URL never leaks into memory text or a query.
`nats_url_tests` pin the precedence.

## 8. Environment variables that change behaviour

Paths and identity: `KANNAKA_DATA_DIR`, `KANNAKA_ALLOW_EXTERNAL_HRM`,
`KANNAKA_AGENT_ID`, `KANNAKA_NATS_URL`, `NATS_USER`, `NATS_PASSWORD`.
Persistence: `KANNAKA_READONLY`, `KANNAKA_QUIET`.
Encoder: `KANNAKA_ENCODER` (`hash`|`ollama`), `KANNAKA_ENCODER_MODEL`,
`KANNAKA_ENCODER_DIM`, `KANNAKA_ENCODER_URL`, `KANNAKA_ENCODER_FORCE`.
Recall ranking: `KANNAKA_RECALL_ENERGY_EXP`, `KANNAKA_RECALL_TEMPORAL_EXP`
(+ `_HALFLIFE_DAYS`, `_FLOOR`), `KANNAKA_RECALL_OBSERVE`,
`KANNAKA_RECALL_BEAM`, `KANNAKA_RECALL_PREFILTER`,
`KANNAKA_RECALL_INCLUDE_DREAMS`, `KANNAKA_RECALL_XI_BOOST` (read once per
process).
Dream: `KANNAKA_CONSOLIDATE`, `KANNAKA_TRIAGE`, `KANNAKA_GHOST_RETAIN_DAYS`,
`KANNAKA_DREAM_ENTROPY`, `KANNAKA_CHIRAL_PERTURBATION`, `DREAM_MODE`,
`KANNAKA_BELIEF_PHASE`.
Swarm: `KANNAKA_EVENTS_REMEMBER` (`off`|`ids`|`content`),
`KANNAKA_NATS_RETRY_REFUSED`, `KANNAKA_PRESENCE_LIVE_SECS`,
`KANNAKA_EXEMPLAR_COUPLING`, `KANNAKA_SERVE_PROMPT_ARM`, `KANNAKA_ASK_LOG`.
Snapshots: `KANNAKA_SNAPSHOT_RETAIN`, `KANNAKA_SNAPSHOT_INTERVAL_SECS`.

`dream` writes `KANNAKA_AGENT_ID` and `KANNAKA_NATS_URL` into its own
environment from config when unset; `swarm serve` / `attention serve` set
`KANNAKA_READONLY=1`. A new env var that changes stored data or published
output should be documented next to its reader and default to the old
behaviour.

## Bug patterns to look for

1. A machine-readable command gaining a stdout line that is not the result.
2. A no-HRM command placed after `init_with_hrm`, or a long-running reader that does not force read-only.
3. Running any kannaka binary with `--help` (or with no `--dry-run`) in tests, scripts or CI against a real data dir.
4. A typo-tolerant flag loop (`_ => i += 1` for `--x`) on a command whose positionals become stored content or a query.
5. A broken networked reply printed as an empty result with exit 0; or exit 0 when an `ask` peer reported a failure.
6. Reading `cfg.swarm.nats_url` directly instead of `resolve_nats_url`, or a value-taking flag not consumed with `flag_value`.
7. Wire text printed without `sanitize_display`.

## Not a bug here

- `status` and `dream` ignoring unknown flags.
- Exit 2 meaning both "bad usage" and "no reply"; callers distinguish by stderr.
- `remember --batch` writing `error:` lines to stdout: it keeps one output line per input line.
- `dream` exiting 0 when the lock is held.
