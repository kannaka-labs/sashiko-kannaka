# kannaka-memory: NATS Transport (`src/nats.rs`)

`src/nats.rs` is a hand-written, synchronous NATS client over raw TCP (the
`nats` crate is not used). It carries swarm gossip, request/reply (`ask`,
`recall`), memory events, presence, and JetStream reads/creates. The client
speaks plaintext only: `handshake_with` refuses a server whose INFO has
`tls_required`. Everything here is a wire protocol held together by ordering
rules, so the failure modes are desync (bytes read by the wrong reader),
silent loss (a broker refusal that returned `Ok`), and storms (a refused
operation retried on every connection).

Callers: `src/bin/kannaka.rs`, `src/bin/handlers/{swarm,ask,inbox,substrate,attention}.rs`,
`src/remember_events.rs` (`NatsRememberSink`), `src/openclaw.rs`.

## 1. The connection model

- `Conn` holds `writer: TcpStream` and `reader: BufReader<TcpStream>`, two
  `try_clone()` handles of one socket. The `BufReader` **must live for the
  whole connection**: it pre-reads past the kernel cursor, so recreating or
  dropping it discards bytes and desyncs the protocol. A diff that builds a
  fresh `BufReader` per call on an existing socket is a bug.
- `Conn::write_frames` is the single choke point for outbound bytes. Any
  write error sets `dead = true`; a dead `Conn` refuses every later write,
  because a partial frame on the wire makes the server parse the next bytes as
  the tail of the truncated frame. Every writer must go through
  `write_frames` (or `write_pub`, which calls it). Direct `self.writer` access
  is reserved for `try_clone` in subscription setup.
- `Conn::set_read_timeout` sets the timeout on **both** handles. On Windows,
  `try_clone` duplicates the handle and `SO_RCVTIMEO` is per handle; setting
  it on the writer alone leaves the reader at `DEFAULT_IO_TIMEOUT` (5 s).
  `Conn::read_timeout` reports the reader's value. A diff that sets a timeout
  through `conn.writer` only reintroduces the "every request gives up at 5 s
  on Windows" bug that Linux CI cannot see.
- Every path that changes the read timeout restores the previous one
  (`prev = conn.read_timeout()` ... `set_read_timeout(prev)`) on all exits:
  `js_api_call_locked`, `request_one`, `request_many`, `ping`,
  `ProbingPublisher::publish_probing`. Check new early returns.
- `SwarmTransport` wraps `Arc<Mutex<Conn>>`. `lock_conn` maps a poisoned lock
  to `NatsError::Protocol`. Sids come from `alloc_sid` (monotonic
  `AtomicU64`); never reuse or hard-code a sid, or a timed-out request can
  collide with a later subscription. Inboxes come from `new_inbox(tag)`
  (`_INBOX.<tag>.<pid>.<uuid>.<nanos>`).

## 2. Handshake and identity

`handshake_with`: TCP connect (timeout `DEFAULT_IO_TIMEOUT`), read INFO with
`read_control_line` (bounded by `MAX_CONTROL_LINE`), send `CONNECT` +
`PING`, wait for `PONG` (at most 10 frames). `-ERR` during the handshake is a
`NatsError::Connect`.

- Credentials precedence is `resolve_creds`: explicit creds > `NATS_USER` +
  `NATS_PASSWORD` env (both non-empty) > `user:pass@` in the URL > anonymous.
  Explicit creds exist so the hive bridge does not inherit the ambient swarm
  identity; `SwarmTransport.explicit_creds` is re-presented by `reconnect`
  and `try_revive_locked`. A redial that drops `explicit_creds` reconnects as
  a different principal with different ACLs.
- `connect_payload` adds `headers`/`no_responders` only when asked
  (`ProbingPublisher`). Shared `SwarmTransport` connections must keep the
  plain CONNECT: their request paths were written against silence, not 503s.
- `Conn.authenticated` is used only to *phrase* a refusal. It must never be
  used to predict what the broker allows; the broker's answer is the only
  source.

Constructors: `connect` / `connect_with_creds` run the JetStream probes
(stream create for authenticated identities, then `stream_readable`) and set
`jetstream_ok` / `jetstream_writable`. `connect_request_only` and
`connect_events_only` both call `connect_handshake_only`: handshake, no
probes, no JetStream, `explicit_creds: None`. Use them only for callers that
never read retained state (`recall --remote`, `NatsRememberSink`). A caller
that reads presence, phases or streams must use `connect`.

## 3. Publish paths: pick the right one

| Method | Confirms with broker? | Buffers on disconnect? | Use for |
|---|---|---|---|
| `publish_raw` (and `publish`) | no | yes | heartbeats, phases, presence, `reply`, KV puts |
| `publish_raw_confirmed` | yes, `Conn::confirm` PING barrier on the shared conn | via `publish_raw` | `KANNAKA.dreams`, `KANNAKA.exemplar.*`, `KANNAKA.memory.new` |
| `publish_memory_event` | first event per (identity, subject) on a separate probe `Conn` | falls back to `publish_raw` | `KANNAKA.events.memory.*` (routed by `publish_event` via `is_memory_event_subject`) |

Contracts:

- NATS refuses a PUB/SUB with an **async** `-ERR`, after the client has
  already returned. Confirmation works because PING/PONG is ordered behind the
  preceding frame (`Conn::confirm_outcome`). Silence (`ReadOutcome::TimedOut`)
  is `Ok(false)`: neither refusal nor acceptance. Never record silence as a
  verdict.
- `publish_raw_confirmed` reads frames from the transport's own reader, so it
  is **only safe on a transport with no live subscription**: a `MSG` it
  consumes is gone from the subscription forever. `reply()` deliberately uses
  `publish_raw` because `swarm serve` replies on a subscribed transport. A
  diff that switches a publish on a subscribed transport to the confirmed path
  is a message-loss bug.
- `publish_memory_event` confirms on a short-lived probe connection with the
  same URL and `explicit_creds`, so it is safe on a subscribed transport.
  Verdicts live in `EVENT_VERDICTS` keyed by `event_verdict_key` (explicit
  user + NUL + subject). `Accepted` -> plain `publish_raw` thereafter;
  `Denied` -> `NatsError::DeniedAgain` and nothing sent; no verdict ->
  `note_event_probe_unanswered` and back off for `EVENT_PROBE_RETRY`
  (= `REVIVE_INTERVAL`). If the shared transport is disconnected it skips the
  probe entirely.
- `publish_raw` replays the disconnect buffer (`flush_buffer_locked`) before
  the new message so wire order matches call order. The buffer is bounded
  (`PUBLISH_BUFFER_LIMIT`, drops oldest, logs). A head message that fails
  `MAX_REPLAY_ATTEMPTS` times is dropped loudly so it cannot pin the queue.
- `try_revive_locked` replaces a dead `Conn` in place, rate-limited to one dial
  per `REVIVE_INTERVAL`. It does not re-ensure streams and does not touch
  subscriptions.
- `reconnect` (needs `&mut self`) re-handshakes, re-runs the JetStream probes,
  then replays the buffer; a failed replay returns `Disconnected` so callers do
  not log "reconnected". **Subscriptions do not survive it**: they read clones
  of the old socket and will report `SubEvent::Closed`. Repair with
  `NatsSubscription::resubscribe_via` (single-subject only) or exit for restart.

## 4. Refusals: learned, remembered, never predicted

Classification: `is_auth_error` (`authorization`/`authentication`) kills the
connection; `is_permissions_error` (`Permissions Violation`) is an
*operation* error and the connection stays usable. Keep them separate; folding
permissions into auth tears down a working connection over one denied subject.
`permissions_error(op, subject, raw, authenticated)` builds
`"<op> denied by broker for ..."`; `NatsError::is_subscribe_refusal` and
`is_publish_refusal` match on that prefix, so the wording is load-bearing.

In-process memory (per identity, deliberately **not** reset on reconnect):

- `PROCESS_STREAM_CREATE_DENIED` plus per-connection `Conn.stream_create_denied`,
  set in `js_api_call_locked` when `classify_server_err` sees
  `$JS.API.STREAM.CREATE` named in a permissions error. `ensure_js_stream`
  checks `should_issue_stream_create` before sending.
- `SUBSCRIBE_REFUSALS`: `subscribe_with_queue` returns
  `NatsError::SubscribeDeniedAgain` without touching the wire;
  `subscribe_phases_and_memories` leaves the subject out of the bundle and
  reports it via `denied_subjects()`. Learned in `confirm_accepted` and in
  `next_event` (late refusal) through `learn_subscribe_refusal`, which prints
  once.
- `PUBLISH_DENIED` for confirmed publishes.

Cross-process memory: `<data dir>/nats-refusals.json` (`refusals_file()` uses
`crate::acp::data_dir()`, i.e. `KANNAKA_DATA_DIR` or `~/.kannaka`). Keys come
from `refusal_key`: explicit user, else `NATS_USER`, else `anonymous`, `@`
`url_host(url)`. Shape: `{"stream_create": {key: {"at", ...}}, "publish":
{key: {subject: {"at", ...}}}}`. Entries are believed for
`STREAM_CREATE_REFUSAL_TTL` (24 h); `KANNAKA_NATS_RETRY_REFUSED=1` ignores
the file. Writes are read-modify-write so other keys survive. A missing,
unreadable or corrupt file means "ask the broker"; that is the safe direction
and must stay that way.

Callers must treat `DeniedAgain` and `SubscribeDeniedAgain` as already
reported: the CLI's `remember` path skips its warning for `DeniedAgain`, and a
retry loop must stop on `is_subscribe_refusal()` instead of retrying in a few
seconds. `DENIED_AGAIN_SUFFIX` is matched by callers that hold only the error
string, so the `Display` text of those variants is a contract.

## 5. Subscriptions and liveness

`subscribe_with_queue` writes `SUB`, clones the socket, sets a finite read
timeout (`SUB_POLL_MAX`), then `confirm_accepted` on the **subscription's own
reader**; `MSG` frames raced ahead of the PONG go to `pending`, never dropped.
`next_event` drains `pending` first.

Liveness (`liveness_action`): after `SUB_PING_IDLE` of silence send one client
PING (`probe_sent`), after `SUB_LIVENESS_TIMEOUT` with no frame return
`SubEvent::Closed`. Any frame calls `mark_frame`. `set_timeout(None)` and
values above `SUB_POLL_MAX` are clamped, because an infinite read never wakes
to run the check. Serve loops must use `next_event` (distinguishes `Timeout`
from `Closed`); `next_message` conflates them and spins on a closed socket.

While a subscription is open, the transport's RPC methods (`request_*`,
`js_api_call_locked`, `publish_raw_confirmed`, `ping`) must not run on the same
connection: two readers steal each other's bytes. Serving agents open a
dedicated `SwarmTransport` per subscription.

## 6. Request/reply

- `request_one(subject, payload, timeout)`: `SUB inbox` + `PUB` + `UNSUB sid 1`
  in one write, then reads until a `MSG` on *that inbox*; other frames are
  skipped, PINGs answered. Timeout is `Err(Protocol("request_one timed out"))`.
  On error it sends an explicit `UNSUB`. Closed / read error / auth error mark
  the transport disconnected.
- `request_many`: per-read timeout 500 ms, collects until the overall deadline,
  **returns `Ok(empty)` on timeout**, always `UNSUB`s. Callers decide what "no
  replies" means (`ask --remote broadcast` exits 2).
- `reply(reply_to, payload)` refuses anything `serve_guard::is_valid_reply_inbox`
  rejects. The reply subject is caller-controlled input reaching a privileged
  publisher; do not add a reply path that bypasses `reply()`.
- JetStream API calls use `js_api_call_locked` with `JS_API_TIMEOUT` (3 s): a
  permission denial gets no reply, so it costs the full timeout. `Ok(None)` =
  no reply.

## 7. JetStream and the single-writer rule

Stream configs are data: `ensure_stream` (`QUEEN_PHASES`, `QUEEN.phase.>`,
1 per subject), `ensure_events_stream` (`QUEEN_EVENTS`, `QUEEN.event.>`),
`ensure_presence_stream` (`KANNAKA_PRESENCE`, 24 h), `ensure_exemplar_stream`,
`ensure_cores_stream`, and `StreamKind::spec` for `KANNAKA_MEMORY_EVENTS`,
`KANNAKA_SUBSTRATE_EVENTS`, `KANNAKA_SNAPSHOTS` (`max_msg_size` 1 MiB, must
stay at or below `MAX_MSG_PAYLOAD`). `ensure_js_stream` does CREATE, and on
err_code 10058 a best-effort UPDATE, so **a spec change is pushed to the live
stream** by the next authenticated writer that connects.

Only the writer identity creates or updates streams. `jetstream_ok` means
readable, `jetstream_writable` means create/update succeeded; gate stream or
bucket management on `has_jetstream_write()`, never on `has_jetstream()`.
Anonymous connections skip the create (`should_attempt_stream_create`) except
`ensure_presence_stream`, which creates only when the stream is absent
(`should_attempt_presence_create`). `stream_walk` advances `next_seq` even past
undecodable messages and ends on any error reply.

## 8. Subjects and payloads

`QUEEN.phase.<agent>`, `QUEEN.announce` (legacy), `KANNAKA.memory.new`,
`KANNAKA.dreams`, `KANNAKA.consciousness`, `KANNAKA.exemplar.<agent>.<cluster>`,
`KANNAKA.cores.<agent>`, `KANNAKA.presence.<agent>`,
`KANNAKA.events.memory.<agent>.{remember,forget,recall}`,
`KANNAKA.events.substrate.absorb`, `KANNAKA.snapshots.<agent>.full`,
`KANNAKA.substrate.absorb.<agent>`, `KANNAKA.substrate.phi`,
`KANNAKA.{ask,recall,neighbors,inbox}.<agent>`, `queen.event.<type>`,
`queen.memory.shared.<target>`. Subjects are case-sensitive:
`announce_event` publishes lowercase `queen.event.<type>` (the consumer
contract) while `QUEEN_EVENTS` binds uppercase `QUEEN.event.>`. Do not change
either case alone. Payloads go through `add_envelope` (`schema_version`
string `"1.0"`, `ts` unix-ms number); `EventPayload::payload_json` sets them
itself and omits `content` (absent, not null) when `None`.
`MemoryRecall` carries ids and `query_sha256`, never the query text.

## Bug patterns to look for

1. A new publish of an advertised capability on `publish_raw` (refusal reads as success), or a confirmed publish on a subscribed transport (message loss).
2. A retry loop around `subscribe`/`ensure_*` that does not stop on `is_subscribe_refusal()` / `DeniedAgain` (broker storm).
3. Reading or writing `conn.writer` directly, or setting a timeout on one handle.
4. A new early return in a timeout-changing function that skips restoring `prev` or skips the `UNSUB`.
5. Changing `permissions_error` wording, `DENIED_AGAIN_SUFFIX`, or the `Display` of the `*DeniedAgain` variants.
6. Making the refusals file authoritative in the other direction (treating a read error as "refused"), or writing a refusal the broker did not send.
7. A stream spec whose `max_msg_size` exceeds `MAX_MSG_PAYLOAD` (8 MiB), or that drops a retention bound.

## Not a bug here

- `PROCESS_STREAM_CREATE_DENIED` and the refusal sets never resetting on reconnect: intended; the broker judges the identity.
- `confirm_outcome` returning `Ok(false)` on silence: intended.
- `request_many` returning `Ok(vec![])` on timeout, `request_one` returning `Err`.
- Plain `std::fs::write` in `persist_*_refusal`: a torn file reads as "no refusal", which is the safe direction.
- Lowercase `queen.*` subjects next to uppercase `QUEEN.*`.
