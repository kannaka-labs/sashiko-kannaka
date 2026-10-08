# Design: Capability Read-Only Bug Access via Bug UUID

## Status

Proposed.

## Context and Motivation

In Sashiko, every verified pre-existing bug receives an unguessable public
identifier (`bugid`, formatted as `<project>-<uuid>` with a 122-bit random
UUID v4, e.g. `linux-550e8400-e29b-41d4-a716-446655440000`) alongside its
internal sequential database row ID (`id`).

Under [`DESIGN_BUG_ACCESS_CONTROL.md`](DESIGN_BUG_ACCESS_CONTROL.md), bug routes
currently require an authenticated `Principal` whose authority comes from
`[server.acl]` (`admins`, `security`) or `MAINTAINERS` subsystem ownership. While
this prevents unauthorized enumeration and cross-subsystem harvesting of
unpatched defects, it also prevents maintainers, operators, or automated review
comments from sharing a direct link (`https://sashiko.dev/bug/<project>-<uuid>`)
with patch authors, contributors, or developers outside that subsystem's
`MAINTAINERS` entry unless they hold subsystem or global credentials.

Because a `bugid` carries 122 bits of cryptographic randomness and cannot be
enumerated, knowledge of the `bugid` itself serves as a capability for reading
that single bug.

## Goals

1. **Capability Read-Only Access by `bugid` / `slug`:**
   - Any caller (anonymous or authenticated) who addresses a bug by its
     `bugid` / `slug` query parameter on `GET /api/bug?bugid=<bugid>` or
     `GET /api/bug?slug=<bugid>` receives at least `BugAccess::Read` on that
     specific bug.
   - Sequential integer `id` lookups (`?id=<i64>`) remain enumerable and
     **never** confer capability access; they continue to require an
     authenticated `Principal` with subsystem or global authority.
2. **Single-Bug Isolation (No Lateral Leakage):**
   - Capability access via `bugid` is strictly scoped to the requested bug
     itself: its report, description, inline review, comments, fixes,
     reproducers, audit history, and its own discovery metadata.
   - Linked duplicate bugs (`duplicate_of`, `duplicates`, and other members of
     `bug_family` in `evidence`) remain filtered by the caller's `Principal`
     authority so that knowing one bug's UUID never discloses the UUID,
     internal ID, or problem statement of an unrelated or unshared bug.
   - Unless all current and historical duplicate target bugs referenced by a
     bug (`duplicate_of_id` and `old`/`new` targets in `duplicate_of_id`
     `audit` enrichments) are within the caller's `Principal` scope, `audit`
     (`duplicate_of_id`) and `deduplication` enrichments are redacted to
     generic messages so deduplication reasoning cannot leak an inaccessible
     counterpart's details.
   - Raw AI analysis transcripts (`/api/bug/logs`, `/api/bug/raw`,
     `/api/bug/input`) continue to require `Principal::has_global_bug_visibility()`
     because deduplication transcripts embed candidate bugs from across the
     database, and `GET /api/bug/enrichments` remains gated by an
     authenticated `Principal` with subsystem or global authority.
3. **Preserve Higher Authority and Token Attenuation:**
   - Authenticated callers who hold `BugAccess::Comment` (security list) or
     `BugAccess::Manage` (subsystem maintainers, global maintainers, operators)
     retain their full authority (`can_comment`, `can_manage`) when loading a
     bug by `bugid`.
   - Scoped API tokens with an explicit `max_bug_access = "none"` ceiling
     continue to attenuate effective access to `BugAccess::None`.
4. **CLI Support for Public Bug Identifiers:**
   - Extend `sashiko-cli bugs show` and `sashiko-cli bugs action` to accept
     either a `<project>-<uuid>` `bugid` or a numeric ID.

## Non-Goals

- No anonymous or unauthenticated access to `GET /api/bugs`,
  `GET /api/bugs/subsystems`, or `GET /api/bug/enrichments`.
- No mutation capabilities (`Comment` or `Manage`) granted by knowing a `bugid`.
- No access to raw AI transcripts (`/api/bug/logs`, `/api/bug/raw`,
  `/api/bug/input`) without global bug visibility.

---

## Architecture

### 1. Access Resolution (`src/access.rs`)

Extend [`Principal`](../src/access.rs) with `access_to_with_bugid`:

```rust
pub fn access_to(&self, attributed: &[SectionTitle]) -> BugAccess {
    self.access_to_with_bugid(attributed, false)
}

pub fn access_to_with_bugid(
    &self,
    attributed: &[SectionTitle],
    has_bugid: bool,
) -> BugAccess {
    let base = if self.operator
        || self.global_maintainer
        || attributed
            .iter()
            .any(|title| self.maintained_sections.contains(title))
    {
        BugAccess::Manage
    } else if self.security {
        BugAccess::Comment
    } else if has_bugid {
        BugAccess::Read
    } else {
        BugAccess::None
    };
    match self.max_access {
        Some(limit) => base.min(limit),
        None => base,
    }
}
```

### 2. Query Discrimination (`src/api.rs` & `src/server.rs`)

Add helpers on [`BugQuery`](../src/api.rs):

- `effective_bugid(&self) -> Option<&str>`: returns the trimmed non-empty
  `bugid` or `slug` only when `self.id.is_none()`.
- `by_bugid(&self) -> bool`: true when the lookup is keyed on `bugid`/`slug`
  rather than the sequential integer `id`.

Update `get_bug` and `get_bug_enrichments` in [`src/server.rs`](../src/server.rs)
(and document the gate in [`prompts/sashiko/subsystem/api-auth.md`](../prompts/sashiko/subsystem/api-auth.md)):

- In `get_bug`:
  - Extract `principal: Result<Principal, (StatusCode, &'static str)>` via
    `resolve_bug_read_principal`:
    - When `query.by_bugid()` is `true`, a missing or expired token falls back to
      `Principal::anonymous()` rather than rejecting with `401 Unauthorized`.
    - When `query.by_bugid()` is `false` (e.g. `?id=42`), a valid `Principal` is
      required (`401 Unauthorized` when absent or invalid).
  - `readable_bug_for_view` evaluates
    `bug_access_for_query(state, principal, bug.id, query.by_bugid())`.
  - `attach_duplicate_relations` continues to evaluate counterpart bugs using
    `bug_access` (`by_bugid = false`) and `readable_bug_ids` so counterpart
    bugs outside the caller's `Principal` scope are redacted.
  - When filtering `bug_family` for `evidence`, insert `bug.id` into the
    `readable` set so the requested bug's own discoveries, comments, fixes,
    reproducers, and audit trail are always included, while other family
    members remain gated by `readable_bug_ids`.
  - Redact `audit` (`duplicate_of_id`) and `deduplication` enrichments on any
    family member unless all of its current and historical duplicate target bugs
    are readable by `principal`, and omit raw `candidate` / `raw_candidate`
    events from `evidence.activity` when
    `!principal.has_global_bug_visibility()`.
- In `get_bug_enrichments`:
  - Retain `principal: Principal` and `readable_bug` (`by_bugid = false`),
    redact `audit` (`duplicate_of_id`) and `deduplication` enrichments unless
    all current and historical duplicate target bugs are readable by
    `principal`, and omit `candidate` / `raw_candidate` records when
    `!principal.has_global_bug_visibility()`.

### 3. CLI (`src/bin/sashiko-cli.rs`)

- Change `BugCommands::Show { id: String }` and
  `BugCommands::Action { id: String, duplicate_of: Option<String>, .. }` to
  accept `<project>-<uuid>` strings as well as numeric IDs.
- Route numeric strings to `?id=<id>` and `bugid` strings to `?bugid=<bugid>`.

---

## Implementation Plan

1. **Step 1:** Add design document `designs/DESIGN_BUG_UUID_READ_ACCESS.md`.
2. **Step 2:** Implement `Principal::access_to_with_bugid`, `BugQuery::by_bugid`,
   and single-bug capability read access on `/api/bug` with enrichment
   redaction and unit/integration tests.
3. **Step 3:** Update `sashiko-cli bugs show` and `sashiko-cli bugs action` to
   accept `<project>-<uuid>` bug identifiers.
