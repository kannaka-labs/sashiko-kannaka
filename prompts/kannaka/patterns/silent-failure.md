# Kannaka Cross-Cutting Pattern: Silent Failure

The most expensive bug class in Kannaka Labs code: a failure that produces a
result indistinguishable from a real one. The caller, a person or an agent then
acts on it. Look for it in every change before anything else.

## The question to ask of every error path

**Can the caller tell this result apart from a genuine one?** An empty list
from "nothing matched" and an empty list from "the subprocess crashed" are the
same bytes. If the answer is no, it is a finding.

## Shapes it takes here

### 1. Error mapped to a default value

- Rust: `.unwrap_or_default()`, `.unwrap_or(serde_json::Value::Null)`,
  `.unwrap_or_else(|| json!([]))`, `.ok()` followed by a default, `let _ =` on a
  call whose failure changes what the user is told.
- JavaScript/TypeScript: `catch {}` or `catch (e) { return [] }`, `|| []`,
  `?? {}` applied to a failed fetch or child process, a `Promise` that resolves
  with partial data on error.
- Python: `except Exception: pass`, `except: return None` where `None` is also
  a valid result, `dict.get` defaults on a malformed payload.

Valid only when the default is genuinely the correct answer for that failure
and the failure is logged where an operator will see it.

### 2. Wrong status or exit code

- An HTTP handler that sends 200 with an error body or a raw-output wrapper.
  Upstream failures are 502/503/504; client mistakes are 4xx.
- A CLI that prints nothing (or `[]`) and exits 0 on failure. In
  `src/bin/kannaka.rs` stdout is machine-read by cron scripts, Node services and
  Python harnesses; a failure must go to stderr with a non-zero exit.
- A child process whose exit code is not checked before its stdout is parsed.

### 3. "Sent" without confirmation

- A NATS core publish is fire-and-forget: the broker's permission refusal
  arrives later as an async `-ERR`, if at all. In kannaka-memory,
  `SwarmTransport::publish_raw_confirmed` and `publish_memory_event` exist to
  learn the verdict; a path that logs "published" after plain `publish_raw` on
  a subject the identity may not have is reporting an unconfirmed send.
- Mail, social posts and broadcasts: a provider 5xx after the request reached
  it is ambiguous, not "not sent". Logging "failed" and retrying can double-post.

### 4. Measurements scored from non-measurements

In research harnesses (`kshb/`, kannaka-bench): a row produced from a child
that exited non-zero, a parser that fell back to scraping text, a counter that
reads 0 because its pattern matched nothing, a share of 0.00 from a store the
instrument could not read. Each must refuse (`refused`, `not evaluable`, `None`)
rather than produce a number.

### 5. Retries that hide permanent refusals

A refused subscribe, publish or API call retried on a timer looks like a
transient blip in each process's log while flooding the shared hub. See
`retries-and-refusals.md`.

## What is NOT a silent failure

- A documented best-effort side effect whose failure is logged and does not
  change what the caller is told (a statusline refresh, a metrics publish).
- A cache fallback that labels itself (a `source: "cache"` field, an
  `X-Kannaka-Source` header, a `degraded` flag).
