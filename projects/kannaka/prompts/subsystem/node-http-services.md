# Node HTTP Services: Observatory, Eye, Staff

This guide covers three plain-`http` Node servers that share one shape: a
single `http.createServer` callback that dispatches with `if` chains, shells
out to the `kannaka` binary, proxies other constellation services, and falls
back to caches when either is slow or down.

| Repo | Entry point | Helpers |
|---|---|---|
| kannaka-observatory | `server.js` | `lib/hrm-client.js`, `lib/http-json.js`, `lib/http-transport.js`, `lib/data-dirs.js`, `lib/coalesce.js`, `lib/remote-allowlist.js`, `lib/mcp-tools.js` |
| kannaka-eye | `server.js` | `attention-bridge.js` |
| kannaka-staff | `src/index.js` | `src/staff/util.js`, `src/staff/<role>/index.js` |

The failures that matter here are not crashes alone. They are: a failure
served as a 200 that looks like data, a second response on one request
(`ERR_HTTP_HEADERS_SENT`, which takes the whole process down), a request that
hangs forever, N concurrent `kannaka` processes each loading the HRM into RAM,
and an endpoint that silently reads the wrong host or the wrong data dir.

## 1. Calling the `kannaka` binary

### Observatory: `runKannaka(kannakabin, dataDir, args, timeout)`

- Uses `spawn` with `env: { ...process.env, KANNAKA_DATA_DIR: dataDir, KANNAKA_QUIET: "1" }`.
  Every observatory call names its data dir explicitly; `KANNAKA_DATA_DIR`
  (default `os.homedir()/.kannaka`) and `SUBSTRATE_DATA_DIR` are different
  stores. A new call that omits `dataDir` reads whatever the environment says.
- **The manual `setTimeout` is the only timeout.** `timeout` must not be passed
  to `spawn()`: on Node 22+ spawn kills the child at the same moment, `close`
  clears the manual timer first, and the promise rejects as "No stdout"
  instead of as a timeout. Flag any diff that adds `timeout` to this `spawn`.
- The exit code is ignored. Settlement rules on `close`:
  - non-empty stdout that parses as JSON and is an envelope
    (`schema_version` is a string, `"data" in parsed`, `errors` is an array):
    rejects if `errors` is non-empty, else resolves `parsed.data`. All three
    conditions are required so a payload with its own `data` field is not
    unwrapped. Callers pass `--envelope` only for `status`.
  - parseable non-envelope JSON resolves as-is.
  - **unparseable stdout resolves `{ raw, stderr }`; it does not reject.**
    Every caller must treat `raw !== undefined` as a backend failure.
    `recallResults(data)` does this for recall and the route returns 502
    `recall_bad_output`. A new route that does `res.end(JSON.stringify(data))`
    straight from `runKannaka` serves a failure as a 200.
  - empty stdout rejects with "No stdout from kannaka".
- CLI flag contract pinned by `test/recall-args.test.js`: no `["recall", ...]`
  argument array anywhere in `server.js` or `lib/` may contain `--json`
  (`kannaka recall` prints a JSON array by default and rejects the flag).
  `observe --json`, `clusters --json` and `neighbors --json` are correct.
  The remote recall adds `--remote --agent-id RECALL_REMOTE_AGENT` with a
  15s guard; a non-array result falls back to the local CLI, and `[]` is a
  valid answer, not a failure.
- `/api/hrm/dream` (POST) spawns `dream --mode <deep|lite>` itself and parses
  **human log lines**, not JSON, from `stdout + "\n" + stderr`
  (`extractCount`, anchored at line start, commas stripped). The mode comes
  from a body capped at 4096 bytes and is whitelisted to `deep`/`lite`.
  `test/hrm-routing.test.js` asserts the resolved mode reaches the spawn.

### Concurrency guards (observatory)

- `createCachedClient(...).cachedKannaka(key, args, ttl)`: TTL cache plus a
  per-key `inFlight` promise. Concurrent cold callers share one process.
- `singleFlight(producer)` (`lib/coalesce.js`) wraps `getMemoriesJson`
  (`kannaka export-json`, the whole HRM) and `_fetchCorrespondenceSF`. It
  coalesces but does not cache.
- **A diff that replaces either with a direct `runKannaka` on a hot path, or
  clears `inFlight` before the promise settles, reopens the fork stampede that
  OOMs the host.** `test/hrm-client.test.js` and `test/coalesce.test.js` cover
  both.

### Eye: `classifyNative(inputBuffer)`

`execFile(KANNAKA_BIN, ["classify"], { timeout: 5000, maxBuffer: 1 MiB })`,
input written to stdin. It resolves `null` on any error or unparseable output
and never rejects; `null` means "use the JS fallback". Whether the binary can
actually classify is decided by `nativeClassifierAvailable()`, a memoized probe
that needs a non-empty `fold_sequence` back. File presence alone must never be
reported as "native": `/api/constellation` and the SVG use
`nativeOk ? "native" : (KANNAKA_BIN ? "unverified" : "off")`. An explicitly set
`KANNAKA_BIN` wins even when the file is missing (with a startup warning);
auto-detection checks `../kannaka-memory/target/{release,debug}`.

### Staff: `exec` with shell strings

`growth` `launchDream` runs `exec(\`${KANNAKA_BIN} dream --mode ${mode}\`)` and
the no-Growth branch of `handleAction("trigger-dream")` does the same with
`--mode lite`. Probes use `exec` for `systemctl is-active`, `pgrep -f` and
`df`. These go through a shell, so the binary path and every interpolated
value must be trusted: `mode` is whitelisted to `deep`/`lite` before
`requestDream`. A diff that interpolates a query parameter into one of these
strings is command injection; use `execFile` with an argument array.
`execActionLocal(cmd, args)` already does. The Growth `g.inFlight` guard
(and `requestDream` refusing while a dream runs) is the staff equivalent of
single-flight.

## 2. One response per request

`writeHead` after headers are sent throws, and **kannaka-eye has no
`uncaughtException` handler**, so a double reply kills the Eye.

- Eye `/api/radio` funnels every outcome through `reply(status, body)`, which
  no-ops when `replied || res.headersSent || res.writableEnded`. On timeout it
  replies 504 *before* `radioReq.destroy()`, because `destroy` also emits
  `error` (which would reply 503). Any new upstream call in Eye must use the
  same first-writer-wins guard.
- Observatory generic `/api/*` proxy (to `ORACLE_HOST:RADIO_PORT`) and
  `/api/hrm/remote`: on error or timeout after headers went out they
  `res.destroy()` instead of writing a status.
- `lib/http-json.js` `getJson` and the remote proxy call `req.destroy()` in
  the `timeout` handler. Node's `timeout` option only arms an idle timer; a
  diff that sets `timeout` without a `timeout` listener that destroys the
  request reintroduces an unbounded hang on a peer that drops packets.
- Staff `probeHttp`, `probeStreamHead` and `probeTcp` never reject; they
  resolve `{ ok: false, status: 0, error }`. `settle` runs on both `end` and
  `close`; the second resolve is a no-op.

## 3. Route matching: pathname versus raw `req.url`

- **Staff** parses once with `requestTarget(req.url)` and every GET route
  compares `pathname`. `tests/route-query.test.js` asserts `/api/state?x=1` is
  200 and `/api/state/extra` is 404. Exception: `POST /action/*` matches and
  HMAC-verifies the **raw** `req.url`, because `verifyStaffHmac` signs
  `${ts}\n${method}\n${reqUrl}` with the query string included and the
  dashboard's `signPath` signs the same bytes. Normalizing the URL before
  verification breaks every signed call.
- **Eye** routes on `url.parse(req.url, true).pathname`.
- **Observatory is mixed.** `reqPath` (the parsed pathname) is used only for
  `/api/hrm/status`, `/api/hrm/observe`, `/api/hrm/constellation` and
  `/api/profile`. Many routes still use `req.url === "..."` (for example
  `/api/constellation`, `/api/hrm/collective`, `/api/hrm/memories`,
  `/api/hrm/dream`, `/api/hrm/remember`). With a query string these do not
  404: they fall through to the catch-all `/api/*` proxy and are answered by
  the radio, or time out with 504. A new exact-match route should use
  `reqPath`. Prefix routes (`startsWith`) parse with `new URL(req.url, ...)`.
- Observatory tests are **source-level**: `server.js` calls `listen()` at
  require time, so `test/clusters-fallback.test.js`, `test/hrm-routing.test.js`
  and `test/api-honesty.test.js` locate handlers by literal text such as
  `if (req.url.startsWith("/api/hrm/clusters"))`. Rewording a route line can
  fail those tests without changing behaviour. Staff instead exports `server`
  and returns early with `if (require.main !== module) return;`. Do not move
  boot side effects above that guard.

## 4. Error-to-status mapping

| Situation | Status | Where |
|---|---|---|
| Binary unparseable output on recall | 502 `recall_bad_output` | observatory `/api/hrm/recall` |
| Observe failed, no cache | 503 `hrm_observe_timeout` (not 200) | observatory; the SPA keeps last-good state on `!res.ok` |
| Clusters failed | cached observe clusters marked degraded, else 503 | observatory `/api/hrm/clusters` |
| Kannaktopus cache missing | 503 `kannaktopus_warming`; never spawns observe inline | observatory |
| Remote proxy validation | 400 bad URL/path, 403 scheme or host | `validateRemoteUrl`, `normalizeRemoteHrmPath` |
| Remote proxy fetch | 504 timeout, 502 otherwise | `classifyRemoteError` |
| Radio non-2xx | `404 -> 502`, others passed through | eye `/api/radio` |
| Action result | 200 `ok`, 202 `accepted`, 500 refused or failed | staff `/action/*`; `ok: true` is never sent for queued work |
| Health | 200 only if no subsystem is stale, else 503 | staff `/api/health` (`healthSnapshot`) |

Eye `/api/radio` must not glyph the idle payload (`status: "no_perception"`).

## 5. Cache fallbacks and data dirs

- `readDiskCache(file)` attaches `_age_ms` as a **non-enumerable** property.
  `JSON.stringify` and object spread both drop it, so read it before
  spreading (as the `cachedKannaka` catch path does with `stale_ms`).
- `cachedKannaka` persists only plain objects that are not arrays and have no
  `raw` key, so a failure never becomes the disk fallback. Fallback responses
  carry `source: "disk-cache"`.
- `/api/hrm/status` priority: radio `/api/swarm` consciousness if newer than
  60s, then `STATUS_CACHE` at any age, then `cachedKannaka("status", ["status",
  "--envelope"])`. Every tier is labelled with `agent_id` and `metrics_source`
  (`stampSubject`). A reading without a subject is the bug.
- `substrateDataDir(env, opts)` order: `KANNAKA_SUBSTRATE_DATA_DIR`, then
  `<homedir>/.kannaka-substrate` if it exists, then `LEGACY_SUBSTRATE_DIR` if it
  exists, else `<homedir>/.kannaka-substrate`. `orcStemPaths` honours
  `ORC_STEM_DIR`.
- Staff `statusCachePathFor(hrmPath)` derives `status-cache.json` from the HRM
  path's directory (override `KANNAKA_STATUS_CACHE`), and `resolveRadioRepo`
  prefers an env override, then a sibling checkout.

## 6. Remote proxies and SSRF

- `/api/hrm/remote` accepts `url` and `path` from the caller. The guard is
  `parseAllowlist(KANNAKA_REMOTE_ALLOWLIST, [ORACLE_HOST, ...peerHosts(PROFILE)])`
  plus `hostAllowed`, which compares the **parsed** hostname (host:port when
  the entry has a port), never a substring. `path` only reaches the target via
  `normalizeRemoteHrmPath` (`REMOTE_HRM_PATHS` or `clusters/<digits>`). The
  body is capped at 4 MiB, and errors must not echo `target` (it may carry
  credentials).
- Outbound URLs choose the module and port from the scheme: `pickTransport`
  (`lib/http-transport.js`) for `lib/mcp-tools.js` and `lib/ooda-loop.js`. An
  implicit port is `""` in `URL`, and passing it through sends HTTPS to port
  80. Eye uses `radioRequest(subPath)` with `radioOrigin()` (`RADIO_URL`, else
  `RADIO_PORT`); staff uses `hostPortOf`. Hardcoding `http` or a loopback
  host:port in a new probe ignores a split-host deployment.
- `lib/mcp-tools.js`: a non-2xx HTTP status is an MCP `isError: true` on both
  the `fetch` path and the Node fallback (`test/mcp-tools-errors.test.js`).

## 7. Authorization (staff)

`isLocalCaller` treats only a loopback socket **without** `x-forwarded-for`
as local. Behind a same-host reverse proxy every request arrives from
loopback, so dropping the forwarded-for check opens `/action/*` to the
internet. `STAFF_REQUIRE_HMAC=true` removes the bypass. `verifyStaffHmac`
must keep the 5-minute skew check (a non-numeric timestamp becomes 0, which is
out of window) and `crypto.timingSafeEqual`. Every action appends an
`actionAuditEntry` row to `ALERTS_FILE`.

## 8. Windows

- Observatory and Eye append `.exe` to the auto-detected binary on `win32`.
  Staff's defaults (`KANNAKA_BIN` in `src/index.js` and `growth`) are Linux
  absolute paths, and `exec` runs through `cmd.exe` there, so a binary path
  with spaces breaks unless quoted. `systemctl`, `pgrep` and `df` probes fail
  on Windows; that is expected, not a regression.
- `statusCachePathFor` falls back on `process.env.HOME`, which is normally
  unset on Windows outside Git Bash. `os.homedir()` is the portable form, and
  it is what the observatory uses.
- `proc.kill()` and `child.kill("SIGKILL")` both terminate unconditionally on
  Windows. Shell scripts (`cache-observe.sh`, `ops/run-eye.sh`) are Linux-only.
- Static-file containment in the observatory compares against
  `publicDir + path.sep`. Keep `path.sep`; a hardcoded `/` breaks the check on
  Windows.

## What is NOT a bug here

- Serving `STATUS_CACHE` or the observe disk cache at any age: it is
  deliberate, and the response is labelled.
- 503 rather than 200 when observe has no cache, and 202 rather than 200 for
  a launched dream.
- `kannaka recall` without `--json`; a remote recall returning `[]`.
- A promise settled twice (resolve after reject, `settle` on both `end` and
  `close`): the second call is ignored.
- Legacy `url.parse` in Eye and staff; Eye honouring an explicit but missing
  `KANNAKA_BIN` (it warns at startup).
- Embedded page JavaScript (`getMainHtml` in Eye, `dashboardHtml` in staff)
  lives inside a template literal, so its backticks and `${` are escaped
  (`\``, `\${`). That is correct, not noise. An unescaped one is a real bug
  that `node --check` cannot see.

## Checklist

1. Does every new `kannaka` call go through `runKannaka` or `cachedKannaka`,
   with an explicit data dir, and reject or 502 on a `{ raw }` result?
2. Is a hot or heavy call (`export-json`, `observe`) behind single-flight or
   a cache, and not spawned inline per request?
3. Does every upstream request have a `timeout` handler that destroys it, and
   is there exactly one write path per request (`headersSent` or `reply`)?
4. Does a new route match on the pathname, and is staff's HMAC still computed
   over the raw `req.url`?
5. Are failures non-2xx, and are degraded or cached responses labelled?
6. Are caller-supplied values kept out of shell strings and remote URLs
   except through the existing validators?
