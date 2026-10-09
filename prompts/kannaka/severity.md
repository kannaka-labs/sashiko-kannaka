# Severity Levels

Assign a severity to every finding. `Critical` is for catastrophic failures,
`High` for severe operational breakage or systematically false output. `Medium`
is the default for real functional defects, `Low` for hardening, rare edge
cases and hygiene.

Kannaka Labs runs small services with real money, real public accounts and live
memory stores. Calibrate against those consequences.

## Calibrating the level (reason before you label)

State this reasoning at the start of `severity_explanation`.

- **Blast radius and irreversibility**: what happens when it triggers, and can
  it be undone? A duplicated public post, funds sent, a store overwritten or a
  secret published cannot be undone. A wrong status code on one route can.
- **Likelihood and preconditions**: name the concrete path. A bug on a path that
  runs every minute (a cron, a statusline refresher, a per-track hook, a
  reconnect loop) is far more likely than one behind an operator command.
  Multiply per-call cost by call frequency when judging floods and retries.
- **Who is misled**: a silent failure that reaches a person or an agent making
  decisions (a measurement, a report, a balance, a "sent" log line) ranks above
  one that only affects a log.
- **Platform**: a defect that only occurs on Windows is not lower severity for
  that; desktops run production work here.
- **Speculative findings**: if a finding rests on an unverified assumption
  about another program or platform, cap it at `Medium` and say what would
  confirm it.

## Critical

- **Question**: does this move money wrongly, publish a secret, destroy or
  corrupt a store or ledger, or publish to a public surface at scale or without
  consent? If no, it is not `Critical`.
- Examples:
  - Value reaching the house, a relayer or another account without the payer's
    authorization verified first; a payer bound before verification; a fee cap
    bypassable by repetition; a refund or settlement that can run twice.
  - A key, token, password, private key or resolved service environment in a
    log, error message, HTTP response, model prompt or commit.
  - A command, hook or probe path that writes to a live memory store, ledger or
    database when it was meant to read; two writers on a single-writer store.
  - A loop or retry that posts, mails or broadcasts repeatedly, or a test or
    dry run that publishes.
  - Remote code execution, command injection, path traversal or authorization
    bypass on a mutating route.

## High

- **Question**: does this systematically produce false output on a primary
  path, take a service down, or flood a shared resource?
- Examples:
  - A primary path that turns errors into plausible results: empty lists, zero
    scores, 200 responses, exit code 0, "sent" log lines.
  - A measurement instrument that scores a malformed or refused input as data.
  - A crash, hang or double response on a request path; a watchdog that kills a
    healthy service.
  - A retry or reconnect loop that re-sends a permanently refused request every
    call, every process or every few seconds against the shared hub.
  - A contract change (stdout shape, JSON keys, NATS subject, HTTP shape, file
    format) that breaks an existing reader in another program.
  - A timeout that silently caps a caller's requested deadline.

## Medium

- Contained defects: a wrong status code on one route, a cache fallback missing
  on one route, a config value not expanded (`~`), CRLF not handled in one
  parser, a test that is weaker than it claims but whose code is right, a
  resource leak on a rare error path, a missing log line on a refusal.
- Commit messages that claim evidence the diff does not contain.

## Low

- Defensive hardening against inputs no real path produces, stale comments,
  unused fields, confusing names, documentation drift.

> Build and compilation errors, formatting, import order and anything a
> linter, `cargo check`, `cargo clippy`, `tsc` or `node --check` reports are not
> findings. They are checked deterministically.
