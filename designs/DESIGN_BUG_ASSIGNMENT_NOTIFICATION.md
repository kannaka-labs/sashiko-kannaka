# Design: Bug Assignment Notification

## Status
Proposed.

## Context and Motivation
A maintainer can assign a bug to anyone by email address (`BugAction::Assign`
on `POST /api/bug/action`, or the Assign button in the web UI). The assignment
is recorded, but the assignee is not notified: they learn about it only if
the maintainer forwards the link by hand.

Sashiko should email the assignee. The email must make clear who made the
assignment, so that a recipient who did not expect it knows whom to ask, and
so that Sashiko is never the anonymous sender of email to an arbitrary address.

## Who Can Trigger It
`Assign` requires `BugAccess::Manage` over the bug: a maintainer of a
`MAINTAINERS` section the bug is attributed to, a global maintainer, or an
operator. The assigner in the email is the address in the caller's
session or API token, the same one the bug's audit trail records: an
assignment a tool makes with someone's API token carries that person's
address.

There is no rate limit. Who can assign bounds the senders, and every
notification carries the assigner's address and has them on Cc, so a
maintainer who assigns a bug back and forth receives a copy of every
message it sends.

## When a Notification Is Queued
An assignment queues a notification only when all of the following hold:
- SMTP is configured (`[smtp]`). The row is queued `Dry-Run` when
  `smtp.dry_run` is set and `Pending` otherwise, as sign-in links are.
- The assignee is set and differs from the previous assignee. Unassigning a
  bug, or saving the same assignee again, sends nothing. The previous
  assignee is the one `assign_bug()` reads in the transaction that writes
  the new one, so two concurrent assignments of one address queue one
  notice.
- The assignee is not the assigner. Assigning a bug to oneself sends
  nothing.
- The assignee is a plain address, not on a known mailing-list domain
  (`email_router::KNOWN_MAILING_LIST_DOMAINS`), not Sashiko's sender
  address, and not blocklisted. Email to a list lands in its public
  archive, bug link included. Sign-in links are not sent to a blocklisted
  address either.
- Addresses are compared ignoring ASCII case for the assignee checks, since
  `Dev@example.org` and `dev@example.org` name one person, although the
  assignee is stored as given.
- The assigner has an address. In testing mode the server authenticates
  nobody, so there is nobody to name and nothing is sent.

The notification goes to the new assignee only, one message per
assignment: on a reassignment, the previous assignee is not notified.

## The Message
- **Kind:** `EmailKind::BugAssignment`, stored as `bug_assignment`.
- **To:** the assignee. **Cc:** the assigner, so the assigner receives
  every message sent in their name, and a reply-all reaches them as well as
  the sender address.
- **Subject:** `[sashiko] <assigner> assigned you a bug: <title>`, with the
  title's whitespace collapsed to single spaces and the title truncated at
  80 bytes, ending in `...` when truncated.
- **Headers:** `Auto-Submitted: auto-generated` and
  `List-Id: <sashiko-bugs.<sender domain>>`, as for sign-in links, so the
  email does not trigger autoresponders and recipients can filter it. No
  `Reply-To`: on sashiko.dev `smtp.reply_to` is a review list, whose public
  archive would hold a reply quoting the link, so sign-in links and other
  transactional email leave it off and replies go to the sender.
- **Link:** `<public_base_url>/bug/<bugid>`, built only when
  `server.public_base_url` has a reachable host
  (`ServerSettings::mailed_link_base_url()`). Without one no notice is sent
  and an error is logged, rather than mailing the bind address that
  `sign_in_base_url()` falls back to.
- **Body:** the assigner, the bug's title, its link, the note given with the
  assignment when there is one (truncated at 4 KiB, with a pointer to the
  whole note on the bug page), a line on who can open the link, and a line
  giving the assigner's address for questions. The Assign dialog's placeholder
  ends "If the assignee is notified by email, this note is included."

The body carries nothing else from the bug. The report itself stays behind
the link, where the bug page applies its own access rules.

## Embargo and Access
No embargo check is applied. A bug has no embargo state of its own
([`DESIGN_BUG_ACCESS_CONTROL.md`](DESIGN_BUG_ACCESS_CONTROL.md) lists
per-bug embargo states as a non-goal), and under
[`DESIGN_BUG_UUID_READ_ACCESS.md`](DESIGN_BUG_UUID_READ_ACCESS.md) the
`bugid` link already grants read access to that one bug. The email therefore
gives the assignee nothing that the link alone would not. Assigning a bug,
and so sharing its link, is the decision of a maintainer with `Manage`
authority over it. A mistyped address sends the link to whoever owns that
address, as sending the link by hand would.

## Failure Handling
The notification is inserted into the outbox in the transaction that
writes the assignment, so the two commit together or not at all. A failure
to queue it fails the request with the assignment rolled back, and a
retry of the request finds the old assignee and queues the notice then. A
client that disconnects after the commit leaves both in place.
