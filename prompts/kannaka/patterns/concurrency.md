# Kannaka Cross-Cutting Pattern: Concurrency, Locks and Processes

## Single-writer stores

The HRM store in the kannaka data directory (`KANNAKA_DATA_DIR`, default
`~/.kannaka`) has one writer at a time. Dream consolidation, `remember`,
imports and `swarm serve` coordinate through a write lock; a second writer
racing the first loses or corrupts memories. Flag any new code path that
writes the store without taking the same lock, and any lock that is not
released on every exit path (early return, `?`, panic, signal).

On Linux the lock is a file lock; check what the code does on Windows, where
the same primitive may not exist or behave the same way.

## Statics and shared process state

kannaka-memory keeps per-process verdict tables in statics (`OnceLock<Mutex<…>>`)
for broker refusals and event verdicts. Tests that reset or redirect them
(`reset_*_for_test`, `TEST_REFUSALS_FILE`) run in parallel by default; two
tests touching the same static must share a lock or they read each other's
state.

## Locks across blocking work

- Rust: a `Mutex` guard held across network I/O (a NATS round-trip, a PING
  confirmation) blocks every other user of the connection for the whole wait.
  `SwarmTransport` serialises its connection through `lock_conn()`; code that
  holds that guard while waiting on something else stalls publishes and
  subscriptions.
- Node: synchronous `fs` or `execFileSync` on a request path blocks the event
  loop for every client.

## Child processes

Node services and harnesses shell out to `kannaka` constantly. Each child
needs: a timeout, both pipes drained, the exit code checked, and a kill on
timeout. A long-lived service that spawns a child per request or per track
also needs a bound on how many run at once.

## Two paths finishing one request

A request that can complete by success, error or timeout must complete exactly
once. Look for timers that are not cleared on success and callbacks that run
after a timeout already answered.

## Watchdogs

A watchdog that restarts a service because a step was slow must know the step's
real budget. A restart drops whatever was in progress (a scheduled show, a
TTS render, a review); a watchdog threshold below a legitimate step duration
turns slowness into an outage.
