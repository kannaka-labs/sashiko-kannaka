# Kannaka Cross-Cutting Pattern: Secrets, Money and Live State

## Secrets

Kannaka services hold: NATS credentials, social account tokens, mail
credentials, model API keys, a relayer's private key, payment provider keys,
admin tokens, JWTs for partner platforms. They live in environment files on the
hosts and in local config files on desktops, never in a repository.

Must never happen:
- A secret in source, a test fixture, a commit message, a log line, an error
  message, an HTTP response body, a prompt sent to a model, or a NATS payload.
- A resolved service environment printed (`systemctl show -p Environment`,
  `env`, `printenv`, a debug dump of `process.env`).
- A token in a URL that gets logged (query string, `user:pass@host` in a NATS
  URL written to stderr). kannaka-memory's NATS code strips credentials from
  URLs before logging (`url_host`); new log lines must do the same.
- Admin or webhook tokens compared with `===` / `==` where a timing side
  channel matters; use a constant-time compare.

## Money

Two systems move value: the Agent-Kax credit ledger (offers, escrow, house
fees, paymaster) and gsr-store (USDC on Base: gasless EIP-3009 transfers
through a relayer, an ATM for onramp and swaps). See `subsystem/kax-ledger.md`
and `subsystem/gsr-store.md` for their invariants.

Must never happen:
- Value reaching the house, a relayer, a fee recipient or another account
  without the paying party's authorization verified first.
- A payer, purchase or claim bound to an identity before its signature or
  authorization is verified.
- A per-transaction limit (fee cap, rate, amount) that can be exceeded by
  repeating a transaction that is individually legal.
- A refund, settlement or relay that can execute twice for one purchase.
- The zero address, a placeholder or an unparsed amount accepted as real.
- A client mistake reported as a server error (retried forever) or a server
  failure reported as a client mistake (abandoned).
- Rate limits keyed on a value the caller controls (a forwarded-for header the
  proxy appends to rather than replaces).

## Live state

- Commands that look read-only must be read-only. In kannaka-memory, several
  subcommands have no `--help` and RUN when given it; a probe, help or status
  path that writes the store, restarts a service, sends mail or posts is a
  critical finding.
- Destructive operations on a store (re-encode, import, prune, dream) need an
  explicit flag or confirmation and a snapshot first.
