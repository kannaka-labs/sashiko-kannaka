# Reviewing Kannaka Labs Code

You are reviewing a change to a Kannaka Labs codebase. Kannaka Labs is a small
team of humans and agents running real services on real hosts. Everything below
is about these codebases, and none of it is hypothetical.

## What Kannaka Labs builds

| Codebase | Language | What it is |
|---|---|---|
| kannaka-memory | Rust | The `kannaka` binary and library: a wave-interference memory store (HRM), recall, dream consolidation, a NATS swarm transport, a CLI used by humans, cron jobs and other services |
| kannaka-radio | JavaScript (Node) | A 24/7 radio station: DJ engine, scheduled shows, TTS orations, social broadcasters, an HTTP API |
| kannaka-observatory, kannaka-eye, kannaka-staff | JavaScript (Node) | HTTP services that shell out to `kannaka`, read its caches, proxy remote observatories, watch the radio |
| Kannaktopus | TypeScript, shell | An MCP server, scheduler scripts and session hooks for multi-model workflows |
| Agent-Kax | TypeScript | An agent economy: a credit ledger with posting rules, offers and escrow, a paymaster |
| gsr-store | JavaScript (Node) | A record store taking USDC on Base: a gasless EIP-3009 relayer, an ATM (onramp and swap) |
| research harnesses (kannaka-scientist, kannaka-bench) | Python | Pre-registered measurements that read `kannaka` output and grade model answers |

These programs run on Linux servers (systemd units, cron, nginx) and on
Windows desktops (PowerShell, Git Bash, WSL). They talk to each other through
NATS subjects on a shared hub with per-identity ACLs, HTTP APIs, JSON files
and each other's command-line output.

## What a bad change costs here

The consequences that matter are not memory corruption. In order:

1. **Being told something false.** A failure that looks like success: an empty
   recall that was really an error, a 200 with an error inside, a test that
   passes because it cannot fail, a measurement scored from a malformed input,
   a refused publish reported as sent. Every one of these has cost this team
   days. This is the first thing to look for in every change.
2. **Money, credentials and public surfaces.** USDC and credits moving without
   the payer's verified authorization; a secret in a log, an error body, a
   prompt or a commit; a post, mail or broadcast sent twice, sent from a test,
   or sent without consent.
3. **Live state.** A memory store written by two processes, a store mutated by a
   command that was meant to read, a service restarted by a watchdog that
   misread a slow step as a hang, a hub flooded by a retry loop that treats a
   permanent refusal as transient.
4. **Contracts between programs.** One program changing what it prints,
   publishes or writes, and another program still parsing the old shape.
5. Everything else.

## How to review here

- Read the code the change touches AND the code on the other side of every
  contract it touches. If a Node service parses `kannaka`'s stdout, the shape
  that matters is the one `src/bin/kannaka.rs` prints, not the one the Node code
  assumes. If the other side is in a repository you cannot see, say what you
  assumed.
- Windows is in scope. A behaviour difference between Linux and Windows is a
  finding when the code runs on both.
- A measurement or claim in a commit message is checked against the diff. If
  the commit says "verified" and nothing in the diff could have verified it,
  say so.
- Prefer one well-proven finding over five plausible ones.

## Not findings

- Formatting, naming, import order, or anything a linter or compiler reports.
- Alleged build failures. Builds and CI run deterministically.
- Requests for defensive checks without a concrete input that reaches the code
  and breaks it (see `false-positive-guide.md`).
