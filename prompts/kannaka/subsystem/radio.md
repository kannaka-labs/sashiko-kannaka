# kannaka-radio Server

This guide covers `server/` in kannaka-radio and the root-level
`memory-bridge.js` it requires. The process is a single long-lived Node server
(`server/index.js`) that drives a 24/7 public stream: the DJ engine picks
tracks, `server/icecast-source.js` pipes them through one `ffmpeg` child into
Icecast, schedulers interrupt the rotation for shows and spoken segments, and
`server/routes.js` serves the web player and `/api/*`.

What matters: dead air, a segment announced but never aired (or aired twice),
a once-a-day slot silently lost, an unauthenticated caller making the station
speak or post, and fabricated numbers presented as measurements.

## 1. Process wiring (`server/index.js`)

- `installCrashGuards()` (`server/crash-guards.js`) is the first thing the
  process does. It logs `unhandledRejection` / `uncaughtException` and keeps
  serving, exiting only after `DEFAULT_MAX_IN_WINDOW` (10) failures inside
  `DEFAULT_WINDOW_MS` (60 s). Relying on it instead of handling an error is
  still wrong: the request that threw never gets a response.
- `KANNAKA_BIN` is the env var, else
  `<repo>/../kannaka-memory/target/release/kannaka[.exe]`, passed as
  `kannakabin` to the modules and `config.kannakabin` to routes, and into
  `memoryBridge.configure({ bin })`. `server/agent-endpoint.js` resolves its
  own `KANNAKA_BIN` with the same rule; the two must stay identical.
- The `DJEngine` `onTrackChange` hook is guarded by `_inTrackChange`
  (re-entrancy: `programming.onTrackChange` may call `loadAlbum`). It runs
  programming FIRST, then re-reads `djEngine.getCurrentTrack()` as `actual`,
  and every side effect uses `actual`. It has two branches, the talk-segment
  branch (inside the `voiceDJ.executeTalkSegment` callback) and the normal
  branch. Both must perform the same side effects: `broadcastState`,
  `flux.publishTrackChange`, `perception_.hearTrack(actual, (perc) =>
  onTrackHeard(actual, perc))`, `icecast-metadata.updateMetadata`,
  `syncManager.trackChanged`, and the `gsHub.createMarket` guarded by
  `orcMarketAskable`. `test/track-change-side-effects.test.js` pins this; a
  diff adding a side effect to one branch only is a bug.
- `shutdown()` is idempotent (`shuttingDown`), awaits
  `icecastSource.stop({ drain: true })` before tearing down anything else, and
  calls `peaceOration.releaseInFlightSlot(...)` when the drain did not report
  `reason: "completed"`.

## 2. The DJ engine (`server/dj-engine.js`)

- `advanceTrack(justFinishedFile, { aired })` and `peekNextTrack()` are
  synchronous and drive the live stream. Nothing async (sqlite, HTTP) may be
  awaited inside them; sponsor and guest confirmation go through injected
  callbacks (`_confirmSponsor`, `_confirmGuest`) wrapped in `try/catch`.
- Confirmation of a paid sponsor or guest spot requires `aired === true` AND
  `prev.file === justFinishedFile`. `icecast-source` passes `aired: false`
  when the file was missing or the stream errored. A diff that confirms on
  start, or without the file match, charges for audio nobody heard.
- `swappedMidStream`: when the finished file is not the current track (the
  playlist was replaced mid-song), track 0 airs without incrementing.
- `peekNextTrack` may reshuffle at the wrap and sets `state._reshufflePending`
  so `advanceTrack` does not reshuffle again; `loadAlbum` clears it. Peek and
  advance must agree, because the DJ intro for the peeked track is generated
  before the current track ends.
- `_onPlaylistExhausted` is one field shared by the four `PodcastScheduler`
  instances in index.js; each sets it on load and nulls it in `_onPodcastEnd`.
- `loadAlbum(name)` returns `null` when nothing is playable; new
  album-selection code should use `ProgrammingSchedule.loadAlbumOrNext`.
- History is `state.history` (cap 200, `playedAt`). `_recentlyPlayed` (12 h
  no-repeat, trimmed to 24 h) and `PlayLedger` (`server/play-ledger.js`,
  untrimmed, `.tmp` + `renameSync`) have opposite retention; keep them apart.

## 3. Scheduling (`programming.js`, `podcast-scheduler.js`, `lib/scheduler-helpers.js`)

- All slot time is America/Chicago, computed as
  `new Date(now.toLocaleString("en-US", { timeZone: "America/Chicago" }))`
  (`chicagoNow`, `_chicagoNow`). Dedup keys are built from that Date's local
  fields (`keyForChicago`, `_keyFor`, `_hourKey`). Mixing in `getUTC*` or the
  host's local time produces keys that never match.
- Every slot has a retry window, not a single minute: peace orations and
  showcases run in minutes 0..14, shows start within `LATE_START_GRACE_MIN`
  (5). The once-per-slot guard is the persisted key (`loadState` / `saveState`
  in `scheduler-helpers.js`, pruned to 3 days), plus an in-memory "preparing"
  guard (`_preparingKey`, `_preparingShowcase`) that must be cleared on every
  exit path including `.catch`.
- `PodcastScheduler.pickTodayEpisode()` is the single source of truth for both
  the airing and `/api/schedule`; do not compute the line-up a second way.
- `_startScheduledPodcast` refuses to run off the `dj` channel or while another
  scheduled show is airing (`playlistMeta[...].isPodcastScheduled`). Shows are
  recorded on disk by `lib/onair-state.js` (`record` on stream start, `clear`
  on end, written via `.tmp` + rename) and resumed by `_resumeIfInterrupted`
  through `resumePlan` (bounded by `MAX_AGE_MS` and `END_MARGIN_MS`).
- `_onPodcastEnd` (exhaustion hook, 5 s poll, 4 h timeout) must stay
  idempotent on `_podcastPlaying`.
- `composeResilient` caps each `opts.slot` at `SLOT_CAP` (3 primary + 1
  direct) across ticks; the direct-Anthropic fallback is opt-in
  (`allowDirect: true`). A scheduler calling compose without `slot` is
  uncapped and will retry every tick.

## 4. Voice, TTS and orations (`voice-engine.js`, `voice-dj.js`, `peace-oration.js`)

- `voiceEngine.synthesize` walks the persona's engine order from
  `server/voice-personas.json` (hot-reloaded), always appends `edge`, and adds
  `sapi` only on win32. ElevenLabs runs only when `elevenLabsEnabled()`.
  Timeouts scale with words: `edgeTimeoutMs` (60 s + 150 ms/word, cap
  `EDGE_TTS_CAP_MS` 300 s), `piperTimeoutMs` (cap 360 s); piper is skipped
  above `piperMaxWords()` (default 300). `test/tts-long-form-budget.test.js`
  requires the edge budget to stay at least twice the measured long-form time.
- **Budget invariant.** `VoiceDJ.executeOration` arms `ttsSafetyMs` (420 s) per
  attempt and retries up to `MAX_TTS_ATTEMPTS` (3) with 60 s x attempt
  backoff, holding the talk lock throughout. The safety must exceed the
  worst-case sum of one `synthesize` pass over the persona's engine order
  (engine timeouts plus `postProcess`' 30 s ffmpeg each). Raising an engine
  timeout, adding an engine to the `oration`/`news`/`gossip` order, or
  lowering `ttsSafetyMs` without rechecking that sum lets the safety
  force-release the lock while a render is still running.
- **Lock discipline.** `_inTalkSegment` and `_speaking` gate every spoken
  segment (`shouldTalk`, `executeOration`, `executeTalkSegment`). Every path
  that sets them must have a release: the TTS-failure branch, the inject
  callback, the 720 s inject ceiling, the no-icecast estimated timer, and the
  safety timers. `executeOration` guards `onDone` with `released`;
  `executeTalkSegment` has no such flag, so its 180 s `safetyTimer` and the
  later TTS callback can both call `onDone`. A diff adding a release path must
  ensure `onDone` cannot fire twice.
- Orations are released by playback, not by a word-count estimate:
  `icecastSource.injectAudio(path, meta, onDone)` calls back after the file
  streamed. `injectAudio` calls `onDone(err)` synchronously when the source is
  not running. Queued items left behind at shutdown never get a callback;
  that case is covered by `releaseInFlightSlot` in `shutdown()`.
- `PeaceOration._say` sets `_inFlightKey` BEFORE `executeOration` (the callback
  may run synchronously). Only the owner of the in-flight slot may release it;
  `deliverNow` and `showcaseAlbum` own no slot.
- `_publishOnce` writes the published mark to disk BEFORE posting to socials
  and OpenClawCity, and a re-air of a released slot reuses
  `_published[key].text`. A diff that posts before persisting, or recomposes a
  released slot, can publish two different orations for one slot; external
  posts cannot be deleted.
- `icecast-source` drops a voice item whose `meta.introFor` no longer matches
  the next track, calling `onDone(new Error("stale intro"))`.

## 5. Calling the `kannaka` binary

Every call is `execFile`/`spawn` with an argv array (no shell) and a `timeout`
(the `/agent/audit` SSE tail excepted), often with `KANNAKA_QUIET: "1"`.

- Failure is a failure. `/api/dreams` answers 503/502 with `degraded: true`
  and an empty list; `/api/dreams/trigger` answers `ok: false` for an error,
  for the single-writer notice (`/holds the write lock|single-writer policy/`
  on exit 0) and for unparseable stdout. `/api/similar` answers 503 when
  `memoryBridge.recallSimilarTracks` returns null. A diff that reintroduces a
  mock or fallback result on these paths is a regression.
- `/api/swarm/peers` serves the last good list marked `stale` on failure and
  projects records through the `PUBLIC_PEER_FIELDS` allowlist
  (`publicPeerFields`) before caching and again on output.
- Real perception only: `PerceptionEngine.hearTrack` seeds `current` with
  `generateMockPerception` and calls `onRealPerception` only when
  `_parsePerceptionOutput` yields `source === "kannaka-ear"`.
  `publishEarAttention` and `memoryBridge.storeHeardTrack` (`skipReason`)
  both refuse anything else. Never feed `getCurrentPerception()` to NATS or
  memory straight after a track change. The `hear` callback does not check
  that its track is still current.
- Flags must exist on the binary. Code comments record that `kannaka dream`
  silently ignores unknown flags and that `kannaka recall` has no `--json`.
- Positional arguments from requests can be parsed as flags.
  `agent-endpoint.js` `rejectFlagInjection` refuses `to`/`verb`/arg keys or
  values starting with `-`. `/api/similar` passes `track` (trimmed, max 200
  chars) as the positional `recall` query with no such check. A new route
  that forwards request text as a positional argument needs the guard.

## 6. HTTP routes (`server/routes.js`, `server/agent-endpoint.js`)

- `handleRequest` parses once: `parsed = new URL(req.url, ...)`. Routes match
  `parsed.pathname` (exact `===`, `startsWith`, or anchored regex) and read
  queries from `parsed.searchParams`. Matching on `req.url` breaks as soon as a
  query string is present. Order is first-match-wins, and the final fallthrough
  is `404 Not found`; many GET routes have no method check.
- One response per request. Each branch writes and `return`s. Callback routes
  (`readBody`, `execFile`) must `return` immediately after starting the async
  work. `readBody` already sent 413 when it rejects and never calls back. The
  GSA block uses `gsaJson`, which checks `res.headersSent || res.writableEnded`
  and swallows throws from a vanished client.
- `readBody` (64 KB `MAX_BODY`) and `readBodyLimited` collect Buffers and
  decode once (the Stripe webhook HMAC needs exact bytes). The gshub `readJson`
  and the NATS client (`this._buffer += data.toString()`) decode per chunk,
  which corrupts a multibyte character split across chunks; do not copy them.
- Auth gates fail closed when unset: `adminTriggerOk` (`RADIO_ADMIN_TOKEN`;
  503 unset, 401 wrong) guards `/api/oration/now`, `/api/album/showcase`,
  `/api/dreams/trigger`; `checkDeletePassword` (`RADIO_DELETE_TOKEN`) guards
  `DELETE /api/library/:file`; `oracleAuthorized`/`denyOracle`
  (`GSHUB_ORACLE_TOKEN`) guards market resolution and `/api/broadcast`;
  `checkAgentAuth` (`RADIO_AGENT_TOKEN`) guards `/agent/send` and
  `/agent/audit`. A new route that makes the station speak, post as Kannaka,
  spend money or run a long `kannaka` command must use one of these. Several
  existing mutating routes (`/api/set-music-dir`, `/api/programming/override`,
  `/api/channel`) carry no token check, so "the neighbours are open" is not a
  justification.
- Static file routes (`/models/`, `/audio/`, `/audio-voice/`,
  `/audio-generated/`) `decodeURIComponent` the tail, `path.resolve` it and
  check `resolved.startsWith(path.resolve(baseDir))`. That prefix check has no
  trailing separator, so a sibling directory whose name begins with the base
  name passes. Do not copy it; compare against `base + path.sep`.

## 7. NATS (`server/nats-client.js`) and broadcasters

- Raw-TCP client. Every socket handler checks `this._client === sock`;
  `close` nulls `_client` and reconnects after 5 s unless `_manualDisconnect`.
  `publish` returns false (and counts `_droppedPublishes`) on a dead socket.
  `connect()` clears the previous `_pruneInterval`.
- `_processBuffer` is byte-counted (`MSG ... <#bytes>`), caps a frame at
  1 MiB, and wraps `_handleMessage` in `try/catch` because the bus is
  publicly writable. Envelopes carry `schema_version: "1.0"` (string), `ts`
  (unix ms) and `agent_id` (`publishEarAttention` in index.js); inbound
  messages missing their subject's required fields are dropped by
  `_validateSchema`.
- `broadcasters/index.js` `broadcastPost` never throws and isolates adapters.
  It posts under Kannaka's own accounts, so callers must be gated (section 6).

## Windows notes

- `process.on("SIGTERM")` does not fire on Windows, and `child.kill("SIGTERM")`
  terminates the child outright. The voice drain in `shutdown()` therefore only
  runs on Ctrl+C (`SIGINT`) there.
- `edge-tts` receives the whole utterance as a `--text` argument, and
  `composeViaKannakaAsk` passes the prompt as argv. Windows caps the whole
  command line near 32 K characters, far below Linux. Piper reads stdin.
- `track.file` is built with `path.join`, so keys in `recently-played.json`,
  `play-history.json`, `track-features.json` and `radio-onair.json` use the
  host's separator and do not carry across operating systems.
- Use `os.homedir()`. `peace-oration.js` reads oration notes from
  `process.env.HOME` with a Linux fallback; `HOME` is usually unset on Windows.
- `_parsePerceptionOutput` splits on `\n` and trims each line, so CRLF output
  parses; keep the trim.

## What is NOT a bug here

- Fire-and-forget `execFile(kannakabin, ["hear", ...], ..., () => {})` in
  `voice-dj.js`. The self-hearing is deliberately best-effort.
- Logging and continuing in `installCrashGuards`; the storm valve is the exit.
- Using `generateMockPerception` for the visualizer. It is the mock reaching
  NATS, memory or a published number that is wrong.
- A `routes.js` comment saying there is no process-level guard (stale).
- `/api/dreams/trigger` answering HTTP 200 with `ok: false` on failure.
- Orations ignoring the selected channel; only talk segments are `dj`-only.

## Checklist

1. Track-change side effects added to both branches of `onTrackChange`?
2. Every set of `_inTalkSegment`/`_speaking` released on every path, and
   `onDone` at most once?
3. TTS timeout changes rechecked against `ttsSafetyMs` and the persona order?
4. Slot keys in Chicago local fields, persisted before external side effects?
5. Route matched on `parsed.pathname`, one response, `return` after async start?
6. New speaking/posting/spending/`kannaka`-running route behind a fail-closed
   gate; request text guarded against leading `-` before reaching argv?
7. No mock or fallback presented as a measurement or a success?
8. New `test/*.test.js` added to the `npm test` chain
   (`test/every-test-runs.test.js` fails otherwise)?
