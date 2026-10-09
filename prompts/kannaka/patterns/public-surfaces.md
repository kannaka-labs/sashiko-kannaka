# Kannaka Cross-Cutting Pattern: Public Surfaces and Interfaces

## Public surfaces

Kannaka services publish: social posts (Bluesky, Mastodon, Nostr, Telegram,
partner city feeds), mail from agent mailboxes, radio broadcasts and
commercials, YouTube uploads, TV slates, prediction markets, GitHub comments.
Anything published is effectively permanent and is read as the agent's own
voice.

A change that touches a publishing path must keep:
- **Consent and approval gates.** Paths that publish on someone else's behalf,
  name a person, or post a paid or money-related message check the recorded
  consent or approval; a missing record refuses.
- **Idempotency.** A retry after an ambiguous failure (timeout, 5xx after the
  request was sent) must not publish twice; use an idempotency key, check what
  was already published, or treat the send as possibly delivered.
- **Dry runs that are dry.** A `--dry-run`, test or probe path must not reach
  the network call. A probe that can post is a post.
- **Rate and length limits of each platform** (character caps that count the
  appended link, per-day post limits), checked before sending, not discovered
  by the platform's refusal.

## Command-line interfaces

- `src/bin/kannaka.rs` is driven by cron jobs, systemd units, Node services and
  Python harnesses. Changing a subcommand's stdout shape, exit codes, or flag
  names breaks them; name the readers or keep the old behaviour.
- Subcommands without `--help` handling run their action when given `--help`.
  A new subcommand that mutates state must handle `--help` (or refuse unknown
  flags) before acting.
- Machine output on stdout, diagnostics on stderr.

## Configuration

- New keys have defaults and are documented; renamed keys keep reading the old
  name or fail loudly.
- Environment variables that change behaviour are named in the change. Note
  which ones a service sets in its unit file and which a desktop shell profile
  sets (desktop profiles export variables that change test behaviour).
