# Kannaktopus: MCP Server, OpenClaw Adapter, Scripts and Hooks

Kannaktopus is a Claude Code plugin (plugin name `octo`) plus two Node adapters
that expose its bash workflow engine and the `kannaka` binary to other clients.
The MCP server speaks JSON-RPC over stdio and starts an HTTP "observatory" only
when `HTTP_PORT` is set. It runs on Windows desktops (Git Bash, MCP hosts) and
Linux servers (the Python daemons under systemd).

| Path | Role |
|---|---|
| `mcp-server/src/index.ts` | MCP server `octo-claw`: `octopus_*`, `kannaka_*`, `hrm_*`, `swarm_*` tools and `createHttpServer` |
| `mcp-server/dist/index.js` | What runs: `.mcp.json` launches `node --require ./mcp-server/check-node-version.js ./mcp-server/dist/index.js` |
| `openclaw/src/index.ts`, `openclaw/dist/` | OpenClaw adapter; mirrors the launcher and env allowlist |
| `scripts/orchestrate.sh`, `scripts/lib/*.sh` | Workflow engine both adapters shell out to |
| `scripts/kannaka-bridge.sh` | Shell wrapper around `kannaka` for hooks and `save_session_checkpoint` |
| `hooks/*.sh`, `.claude-plugin/hooks.json` | Hooks; only scripts named in `hooks.json` run |
| `scripts/kannaktopus_listener.py`, `queensync_presence.py`, `kannaktopus_floor.py` | NATS / floor daemons |

The failure that matters is the quiet one: a tool or route that reports success
with empty, stale or fabricated data, a hook that never fires, or a `kannaka`
write the CLI rejected while the caller printed "absorbed".

## 1. The compiled `dist/` is what ships

- CI (`.github/workflows/test.yml` running `make test-smoke`, `test-unit`,
  `test-integration`) never runs `tsc`. The tests covering the server
  (`test-adapter-flags.sh`, `test-credential-isolation.sh`,
  `test-openclaw-compat.sh`, `test-gemini-provider.sh`) `grep`
  `src/index.ts`. **A diff to `src/index.ts` without a regenerated
  `dist/index.js` (plus `.d.ts` / `.map`) passes CI and changes nothing a user
  runs.** An edit to `dist/` alone is lost at the next build. `openclaw/dist/`
  must stay committed (`tests/validate-openclaw.sh`).
- Because the tests are text matches, renaming `postFlags`, the
  `...postFlags, prompt` spread, the `process.env.OPENAI_API_KEY &&`
  conditional-spread form, or the literals `"grapple"`, `"squeeze"`,
  `"code-review"` fails a test even when behaviour is unchanged.

## 2. stdout belongs to the MCP transport

`main()` connects a `StdioServerTransport`; every diagnostic uses `console.error`.
A `console.log` or stdout write anywhere in the server corrupts the JSON-RPC
stream.

## 3. Calling `kannaka`: `runKannaka`

- `resolveKannakaBinary()`: `KANNAKA_BIN` wins; when it is the default
  `"kannaka"` on win32, `%USERPROFILE%\.local\bin\kannaka.exe` is used if it
  exists, else the bare name goes to `execFile` for PATH + PATHEXT.
- Env: full `process.env` plus `KANNAKA_QUIET=1` and
  `KANNAKA_DATA_DIR=resolveKannakaDataDir()`; `windowsHide: true`; timeout
  20 s (`swarm_send` `wait*1000+5000`, `swarm_tail` `seconds*1000+1500`). No
  `maxBuffer` is passed, so Node's 1 MiB default applies.
- Contract `{ stdout, stderr, isError }`: any zero exit is success (stderr holds
  HRM init chatter). On a throw, captured stdout counts as success **only**
  when `execErr.killed`; any other non-zero exit is `isError: true`. Do not
  widen that to "any stdout means success".
- So a timeout or `maxBuffer` overrun yields truncated stdout with
  `isError: false`. JSON consumers must handle a parse failure; `!isError` does
  not mean parseable.
- Args are an argv array (no shell); user text is a positional with no `--`
  separator. The CLI's `remember` exits 2 on any unknown `--` token, so content
  starting with `--` is rejected. Only `swarm_send` validates its inputs
  (`ID_RE = /^[\w.:@-]+$/`; `--arg` keys with no leading `-`, `=` or
  whitespace). Keep those checks.

### CLI contract the tools must match

From the `kannaka` source: `remember` takes `--importance`, `--category`,
`--modality` and one comma-joined `--tags T1,T2`; there is no `--tag`. `recall`
prints a JSON array and has no `--json`. `neighbors <id-or-query> [--top-k N]
[--json]`. `observe --json` serializes `SystemReport`: metrics live under
`consciousness` (`phi`, `xi`, `mean_order`, `num_clusters`, `total_memories`,
`total_skip_links`), the list under `clusters.clusters[]`.

Current code that does **not** match (check before blaming a diff for it):
`kannaka_absorb` pushes `--tag <t>` per tag, so any call with `tags` exits 2 and
stores nothing, and it sends `modality` as `--category` (the bridge's
`kannaka_absorb` is correct). `/api/experiments/xi` reads `obs.xi`, `obs.phi`
etc. from the report's top level, where they do not exist, and returns `{}`
with 200; `generateConstellation` reads `observe.consciousness` correctly.

## 4. Cache fallbacks: `loadObserve`, `loadStatus`

Both return `{ ok: true, stdout, source: "live" | "cache" }` or
`{ ok: false, error, cacheError }`: live first, then `observe-cache.json` /
`status-cache.json` in `resolveKannakaDataDir()` when live is `isError` or empty.

- The fallback is shared on purpose: `kannaka_constellation`,
  `hrm_list_clusters`, `hrm_cluster_details` and the routes
  `/api/hrm/observe`, `/constellation`, `/clusters`, `/clusters/:id`,
  `/api/experiments/xi` use `loadObserve`; `/api/hrm/status` uses
  `loadStatus`. A new reader that calls `runKannaka(["observe", "--json"])`
  directly loses it. The `kannaka_status` and `kannaka_observe` tools
  currently do exactly that.
- `source` must reach the caller (`X-Kannaka-Source` header or a `source`
  field). Cached data must never be presented as live.
- It falls back on failure, not on bad output: truncated non-empty live stdout
  (§3) comes back as `source: "live"`, the caller's `JSON.parse` fails, and the
  cache is never read.
- A cache read failure is `ok: false`, never `{}` or a default report.

## 5. No fabricated topology

- `generateConstellation` takes clusters only from `clusters.clusters[]`;
  `skip_links` is always `[]` (`skip_links_info` explains; only the scalar
  count is reported); positions are flagged `layout.derived: true`; a cluster
  without `member_ids` contributes no points (`plotted_members`,
  `members_known`); zero clusters is an empty constellation; both disagreeing
  counters stay in `counters`. Reject edges synthesized from layout, members
  invented from `size`, or a loop sized by `num_clusters`.
- `traverseHrm` (shared by `hrm_traverse` and `/api/hrm/traverse`) records each
  failed hop in `errors`; `failed` only when every lookup failed (tool
  `isError`, route 502); partial walks return `partial: true`. Edges hang off
  the expanded node `q`, never `neighbors[0]`; the seed is always a node.
- `hrm_cluster_details` and `/api/hrm/clusters/:id` index the array position,
  not `cluster_id`.

## 6. Workflow launch: `runOrchestrate` / `executeOrchestrate`

- Args `[...flags, command, ...postFlags, prompt]`: global flags (`-q`,
  `--autonomy`) before the command, `grapple`'s `-r` / `--mode` after it. No
  `-d`.
- cwd: `resolveWorkflowCwd()` tries `editorContext.workspaceRoot`,
  `OCTOPUS_PROJECT_DIR`, `CLAUDE_PROJECT_DIR`, then `PLUGIN_ROOT`. A supplied
  value that is not an existing absolute directory is an **error**, never a
  silent fall back to the plugin checkout (orchestrate.sh derives
  `PROJECT_ROOT` from its cwd). `octopus_set_editor_context` rejects a `..`
  segment and a non-existent `workspace_root`. OpenClaw differs by design:
  `resolveWorkspaceDir` fails registration only for a bad
  `pluginConfig.workspaceDir`, and warns past a bad `OCTOPUS_PROJECT_DIR` or
  `agents.defaults.workspace` to `process.cwd()`.
- Env is an allowlist, never `process.env` whole: `PATH`, `windowsChildEnv()`
  (win32: `SystemRoot`, `SystemDrive`, `PATHEXT`, `USERPROFILE`, `TEMP`,
  `TMP`), `HOME`, `TMPDIR`, `SHELL`, `USER`, provider keys only when set,
  `CLAUDE_OCTOPUS_*` / `OCTOPUS_*` minus `BLOCKED_ENV_VARS`, and the IDE
  context. `BLOCKED_ENV_VARS` must be identical in both adapters; a new key
  needs the conditional spread in both files.
- Errors are scrubbed with `/[A-Za-z_]+KEY=[^\s]+/g`, which covers `*_KEY=`
  only; `GH_TOKEN=`, `GITHUB_TOKEN=`, `ANTHROPIC_AUTH_TOKEN=` pass through.
- `selection` is capped at `MAX_SELECTION_LENGTH` (it travels as an env var).
- The launcher (`BASH_EXECUTABLE_NAMES`, `findBashOnPath`,
  `resolveScriptLaunch`, `windowsChildEnv`) is duplicated in
  `openclaw/src/index.ts`; a fix to one copy needs the other.

## 7. HTTP observatory: `createHttpServer`

- Routes match `new URL(req.url, ...).pathname` exactly; parameters come from
  `url.searchParams`, so query strings never break a match.
  `/api/hrm/clusters` is tested before the `/api/hrm/clusters/` prefix arm.
- Per request: CORS, `OPTIONS` 204, bearer auth on `/api/*` when
  `OCTO_HTTP_TOKEN` is set (`isAuthorized`: length check then
  `timingSafeEqual`), then `rateLimitOk` (off unless `OCTO_HTTP_RATE_RPM` > 0;
  `rateKeyFor` trusts `X-Forwarded-For` only with `OCTO_HTTP_TRUST_PROXY=1`).
  Auth must stay before the limiter and before any spawn. Without a token
  `main()` binds loopback only; binding all interfaces without one exposes the
  HRM.
- `MAX_QUERY_LEN` (default 512) bounds `q` and `start`. Upper clamps exist
  (`top_k` 20 / 50 / 10, `depth` 4) but no lower bound where the zod schemas
  have `min(1)`. Traversal spawns one `kannaka` per lookup, sequentially: depth
  4 at top_k 10 can exceed a thousand spawns of up to 20 s for one request,
  which the limiter counts once.
- Status mapping: 400 bad params, 401, 429, 404 unknown cluster or missing
  experiment file, 500 failed `kannaka`, 502 unparseable JSON (status,
  observe, recall) or an all-failed traverse. `/api/hrm/neighbors` returns
  stdout without validating it.
- Each branch `return`s after `res.end`. The outer `catch` calls
  `res.writeHead(500)` without checking `res.headersSent`; keep new branches
  write-then-return.

## 8. Data dir and paths

- `resolveKannakaDataDir()` expands `~`, `~/`, `~\` in `KANNAKA_DATA_DIR`
  against `resolveHomeDir()` (`USERPROFILE` on win32, `HOME` elsewhere, then
  `os.homedir()`); unset is `<home>/.kannaka`. No OS expands `~` inside an env
  var, so every new reader needs this (`queensync_presence.py`
  `_observe_cache_path` uses `os.path.expanduser`).
- `kannaka-bridge.sh` sets `KANNAKA_DATA_DIR="${KANNAKA_DATA_DIR:-~/.kannaka}"`:
  bash leaves a `~` inside double quotes literal, and it is not exported, so
  it never reaches `kannaka`. Adding `export` would hand the binary a directory
  named `~`.
- `resolveKannakaMemoryRoot()` defaults to a sibling `../kannaka-memory`; a
  missing experiments file is a 404 with a hint, not an empty 200.
- `kannaka_available` in the bridge holds a hardcoded WSL path into one user's
  Windows profile, the form `scripts/validate-no-hardcoded-paths.sh` denylists
  (`/Users/<name>/`, `/mnt/c/Users/...`, `C:\Users\<name>`). Do not add more.

## 9. Hooks and the bridge

- `hooks/kannaka-session-start.sh` and `kannaka-session-end.sh` are not in
  `hooks.json` and never run. The live HRM paths are `session-start-memory.sh`
  §5, `session-end.sh` §6 and `save_session_checkpoint` in
  `scripts/lib/session.sh`.
- `kannaka-bridge.sh` is mode `100644` in git. `save_session_checkpoint` runs it
  as `bash "$hrm_bridge"`, gated on `-f`, and compares the printed
  `true`/`false` of `available` (which always exits 0);
  `tests/unit/test-checkpoint-hrm-bridge.sh` pins this with a non-executable
  stub. `session-start-memory.sh` and `session-end.sh` gate on
  `[[ -x "$KANNAKA_BRIDGE" ]]` and exec it, so on a fresh clone their HRM
  sections are silently skipped. Prefer `bash "$script"` behind `-f`.
- When sourced, the bridge's dispatcher returns early (`BASH_SOURCE[0] != $0`);
  without that guard its usage branch's `exit 1` kills the sourcing hook. Its
  `set -euo pipefail` applies to the sourcing script.
- `kannaka_exec` swallows every failure (`|| echo ""`, stderr to `/dev/null`
  unless `KANNAKA_DEBUG`): a broken call looks like "no memories". The timeout
  wrapper is whichever `timeout` / `gtimeout` is first on PATH; Windows'
  `System32\timeout.exe` is an unrelated program that Git Bash normally shadows
  with `/usr/bin/timeout`.
- Hooks exit 0 on any HRM failure and never block: HRM writes are backgrounded
  or `|| true`. Hook stdout becomes session context; `kannaka-session-start.sh`
  prints recalled text with `echo -e`, which interprets backslashes in it.
- There is no `.gitattributes`. Tests strip CR (`tr -d '\r'`) before sourcing
  shell files, because a CRLF checkout fails under bash with `$'\r'`.

## 10. Python daemons

- `kannaktopus_listener.py` `HANDLERS` (`ping`, `status`, `capabilities`, `run`,
  `version`, `wake`) are reachable from the anon-publishable
  `KANNAKA.ask.<arm>` / `KANNAKA.ask.broadcast` as well as the authenticated
  `KANNAKTOPUS.command.*`. `cmd_run` returns `implemented: False` on purpose.
  **A handler that runs a process, reads a caller-chosen path or writes state
  is remote code execution for anyone on the bus.** `preflight` fails closed
  without credentials unless `--anon-only`; subscriptions are guarded per
  subject, and zero subscribed is logged as an error.
- All three daemons register SIGINT/SIGTERM with `loop.add_signal_handler` and
  catch `NotImplementedError` for Windows. Keep the catch.
- NATS URL precedence `NATS_URL`, `KANNAKA_NATS_URL`, default is the same in
  `scripts/lib/nats-publish.sh`, the listener and the beacon; change all three.

## What is NOT a bug here

- `runKannaka` ignoring stderr on a zero exit; `skip_links: []` and
  layout-derived coordinates; `cmd_run` returning `implemented: False`.
- Loopback-only, unauthenticated HTTP when `OCTO_HTTP_TOKEN` is unset; wildcard
  CORS (access is gated by the token, not the origin).
- The duplicated launcher and env code in the two adapters (the bug is letting
  them diverge).
- `swarm_tail` reporting a quiet window as "no swarm messages".

## Checklist

1. `src/` changed: is the rebuilt `dist/` in the diff? Any stdout write added?
2. New `kannaka` call: flags the CLI accepts, truncated stdout handled, observe
   or status read through `loadObserve` / `loadStatus` with `source` exposed?
3. Workflow cwd still the caller's project (bad explicit dir = error)? Child env
   still an allowlist, `BLOCKED_ENV_VARS` unchanged, other adapter mirrored?
4. HTTP: auth before limiter and spawn, loopback without a token, every user
   number and string bounded, one response then `return`?
5. Hooks: wired in `hooks.json`, exit 0 on failure, bridge run via `bash`, no
   home paths, `~` expanded where a path comes from the environment?
6. Listener: nothing that executes or touches files on anon subjects?
