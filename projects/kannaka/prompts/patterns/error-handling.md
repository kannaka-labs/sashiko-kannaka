# Kannaka Cross-Cutting Pattern: Error Handling and Crash Safety

Covers panics, crashes and error propagation across the Rust, JavaScript,
TypeScript and Python code. Silent conversion of errors into results is covered
separately in `silent-failure.md`.

## Rust (kannaka-memory, consciousness-core, kannaka-attention, QuantumOS)

- `.unwrap()` / `.expect()` on data from outside the process (NATS payloads,
  files in the data dir, `export-json` input, HTTP bodies, environment
  variables) is a crash vector; a service like `swarm serve` or the radio's
  per-track `kannaka remember` dies on it.
- `f32::clamp(min, max)` and `Ord::clamp` panic when `min > max`. Bounds that
  come from configuration structs with public fields can be inverted; clamp
  through a helper that orders them.
- NaN propagates through `clamp`, `min`, `max` comparisons and averages; a NaN
  coupling, phase or score is held forever once stored. Check `is_finite()` on
  inputs that feed persistent state.
- UTF-8: `&s[..n]` panics inside a multi-byte character; memory content, song
  titles and agent names are routinely non-ASCII.
- Division by a count that can be zero (empty store, empty pool, no rows).

## JavaScript and TypeScript (Node services, Kannaktopus, Agent-Kax, gsr-store)

- **One response per request.** In Node `http` handlers, a timeout handler and
  an error handler (or a late success callback) can both call `res.end`;
  writing twice throws `ERR_HTTP_HEADERS_SENT` and can crash the process. A
  guard (`if (res.headersSent) return` or a single `reply()` closure) is needed
  wherever more than one event can finish the request.
- `JSON.parse` on another program's stdout, a cache file or a request body
  without `try`.
- Unhandled promise rejections in fire-and-forget calls (`doThing()` without
  `await` or `.catch`).
- `child_process.execFile` errors carry `stdout`/`stderr`; a non-zero exit with
  partial stdout is not a success unless the code says why.

## Python (research harnesses, rogue-agent, kannaka-grid)

- Bare `except:` and `except Exception:` that also swallow `KeyboardInterrupt`
  intent or bus-closed signals the daemon must exit on.
- `subprocess.run` without `timeout=`, and without checking `returncode`.
- `json.loads` on files written by PowerShell (BOM) without `utf-8-sig`.

## Error messages

- Name what failed and where (the subject, the route, the file, the command),
  so an operator can act.
- Never include secrets, tokens or a resolved environment in an error message
  or HTTP error body. See `secrets-and-money.md`.
