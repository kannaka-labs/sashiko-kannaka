# Kannaka Cross-Cutting Pattern: Retries, Reconnects and Refusals

The NATS hub is shared by every Kannaka service, every agent seat and anonymous
public clients. Each identity has an ACL. A request the ACL refuses will be
refused again; a client that retries it turns one refusal into a steady flood
on the hub's log and a stall in its own startup.

## Rules a change must keep

1. **Classify before retrying.** Authentication failures, permission
   violations (`Permissions Violation for Publish/Subscription to "…"`), 4xx
   responses and "unknown flag" errors are permanent. Timeouts, connection
   resets and 5xx are transient. Only transient failures are retried.
2. **Learn a refusal once.** kannaka-memory remembers broker refusals per
   identity and subject for the life of the process
   (`NatsError::DeniedAgain`, `NatsError::SubscribeDeniedAgain`,
   `is_subscribe_refusal()`), and across processes in
   `<data dir>/nats-refusals.json` (stream creates and confirmed publishes,
   believed for a day, overridden by `KANNAKA_NATS_RETRY_REFUSED=1`). A new
   request path that can be refused must use the same machinery, not a fresh
   retry loop.
3. **Count per process, not per call.** Many clients are short-lived: a cron
   job, a statusline refresher that runs `kannaka status` every minute, the
   radio's per-track `kannaka remember`. A "once per process" guard on such a
   client is once per call. Multiply what one process puts on the wire by how
   often the process starts.
4. **Back off and bound.** A reconnect loop backs off, caps its attempts or its
   rate, and does not open a new connection while the old one is alive.
5. **Credentials, not defaults.** A client must take its server URL and
   credentials from configuration; a hardcoded default pointing at the public
   hub with no credentials publishes into refusals it cannot see.

## What to report

- A loop, timer or reconnect path that re-sends a request after a permanent
  refusal, with the period and what is re-sent.
- A refusal swallowed without one log line naming the subject and identity.
- A new subject published or subscribed without checking the identity's ACL
  allows it (the ACL lives in the hub's configuration, not in the code; say so
  and state the assumption).
