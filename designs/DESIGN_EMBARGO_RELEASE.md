# Design: Maintainer-Controlled Patchset Embargo Release

## 1. Problem Statement

When a patch series matches a subsystem policy with `embargo_hours > 0` in
`email_policy.toml`, Sashiko sets `patchsets.embargo_until` to hold back public
review disclosure until the embargo window expires:

1. While `embargo_until > now`, `get_patchsets`, `get_patchset_details`, and
   `get_patchset_summary` report the patchset status as `"Embargoed"`, hide the
   `reviews` array, and zero out finding counts.
2. During review execution, `Reviewer::process_patch_review` skips queueing
   outbound email and Patchwork notifications while `embargo_until > now`, and
   `Reviewer::queue_forge_pr_comment` queues forge outbox rows in the
   `"Embargoed"` state.
3. In the background, `Reviewer::release_embargoed_results` polls
   `Database::get_releasable_embargoed_patchsets` every 10 seconds and releases
   patchsets whose embargo has expired (`embargo_until <= now`) or that completed
   cleanly without findings.

Today, if a subsystem maintainer or reviewer inspects an embargoed patchset in
the web UI and wants to lift the embargo early without waiting for the timer to
expire, there is no API endpoint or UI control to do so.

## 2. Goals

1. Allow authorized maintainers and reviewers to release the embargo on an
   embargoed patchset early from the web UI (`#/patchset/<id>`) and via HTTP
   (`POST /api/patchset/release-embargo`).
2. Reuse Sashiko's stateless `Principal` + `MAINTAINERS` attribution model
   (`patchset_maintainer_sections`) alongside `Permission::Review` in
   `[server.acl]`.
3. Ensure releasing an embargo via the API immediately makes the review findings
   visible in the UI while preserving the asynchronous outbox release path in
   `Reviewer::release_embargoed_results` so held-back email, Patchwork, and
   forge notifications are still delivered exactly once.
4. Render a `Release Embargo` button directly beneath the `Embargoed` status
   badge in the patchset detail header only when the patchset is embargoed and
   the current user is authorized to release it.

## 3. Authorization Model

A caller may release the embargo on patchset `P` when the server is not in
`read_only` mode, the caller's presented identity or token `sid` is not in
`[server.acl].blocklist`, and at least one of the following holds:

1. **Global Review Capability (`Permission::Review`):**
   `is_authorized(&state, &headers, auth, Permission::Review)` is true. This
   covers:
   - `[server.acl].admins` and `[server.acl].review` (for interactive browser
     sessions),
   - the local operator token (`.sashiko-local-token`),
   - `server.testing_mode` and `--enable-unsafe-all-submit`.
2. **Subsystem or Global Maintainer Standing (`Principal`):**
   The caller's `Principal` (resolved from their session JWT or a scoped API
   token whose `max_bug_access` ceiling permits `Manage`) satisfies
   `principal.may_release_embargo(&titles)`, where `titles` are the normalized
   `SectionTitle`s returned by
   `Database::authorizing_sections_for_patchset(patchset_id)` (`source =
   'maintainers_section'`). This grants access to:
   - Sashiko operators (`[server.acl].admins`),
   - global maintainers (`THE REST` in `MAINTAINERS`),
   - maintainers and reviewers (`M:` / `R:`) of any `MAINTAINERS` section
     touched by the patchset.

### Security Invariants

- **Blocklist Precedence:** Blocklisted email addresses and revoked token `sid`s
  are rejected before any grant (`testing_mode`, `allow_all_submit`,
  `Permission::Review`, or `MAINTAINERS`).
- **Fail-Closed on Unattributed Patchsets:** If a patchset has no
  `source = 'maintainers_section'` rows in `patchset_maintainer_sections`,
  section-maintainer standing grants nothing; only operators, global maintainers,
  and callers holding `Permission::Review` may release its embargo.
- **Scoped API Token Attenuation:** A scoped `api_token` JWT is rejected by
  `is_authorized` for global `Permission::Review` and is subject to its
  `max_bug_access` ceiling in `Principal::may_release_embargo`, so a read-only
  or comment-only agent token cannot release an embargo.
- **Zero-Query Anonymous Fast Path:** When a request carries no credentials and
  neither `testing_mode` nor `allow_all_submit` is active, authorization resolves
  to `false` in memory without querying `patchset_maintainer_sections`.

## 4. Database & Notification Release Semantics

A naive implementation that sets `embargo_until = NULL` directly in the HTTP
handler would silently drop held-back email, Patchwork, and forge notifications,
because `Reviewer::release_embargoed_results` selects patchsets with:

```sql
WHERE p.status = 'Reviewed' AND p.embargo_until IS NOT NULL
  AND (p.embargo_release_started_at IS NULL OR p.embargo_release_started_at <= ?)
  AND (p.embargo_until <= ? OR (...))
```

Instead, `Database::release_patchset_embargo(id, now)` expires the embargo
timestamp in place:

```sql
UPDATE patchsets
SET embargo_until = ?
WHERE id = ? AND embargo_until IS NOT NULL AND embargo_until > ?
```

Setting `embargo_until = now` achieves both requirements without race conditions
or duplicate notifications:

1. **Immediate UI & API Visibility:** `get_patchsets`, `get_patchset_details`,
   and `get_patchset_summary` compute `is_embargoed` as `embargo_until > now`.
   As soon as `embargo_until` is set to `now`, `is_embargoed` evaluates to
   `false`, so the very next read returns `status: "Reviewed"`, all completed
   `reviews`, and full finding counts.
2. **Guaranteed Outbox Delivery:** Because `embargo_until` remains `NOT NULL`
   and satisfies `embargo_until <= now`, the background `Reviewer` loop claims
   the patchset via `claim_patchset_embargo_release`, queues email and Patchwork
   notifications, transitions `forge_outbox` rows from `'Embargoed'` to
   `'Pending'`, and finally clears `embargo_until = NULL` via
   `clear_patchset_embargo`.

## 5. HTTP API Changes

### 5.1 `GET /api/patch` and `GET /api/patchset`

Both `get_patchset` (`GET /api/patch`) and `get_patchset_summary`
(`GET /api/patchset`) include a boolean field in the returned JSON object:

```json
{
  "can_release_embargo": true
}
```

`can_release_embargo` is `true` only when the patchset is currently embargoed
(`status == "Embargoed"`) and `can_release_patchset_embargo` succeeds for the
request's caller.

### 5.2 `POST /api/patchset/release-embargo`

- **Query Parameters:** `PatchQuery` (`?id=<i64>`, matching `POST /api/patchset/rerun`).
- **Responses:**
  - `200 OK` with `{ "status": "released", "released": true }` when an active
    embargo was present and has been expired (also invalidating
    `patchsets_homepage_cache`).
  - `200 OK` with `{ "status": "released", "released": false }` (idempotent)
    when the caller is authorized for the patchset, but no active embargo was
    present (e.g., already expired or released concurrently).
  - `400 Bad Request` when `query.id` is not a valid `i64`.
  - `403 Forbidden` when `state.read_only` is true or the caller lacks authority
    to release the embargo on this patchset.

## 6. Web UI Changes (`static/index.html`)

In `renderPatchsetView`:

1. Wrap the right side of the `<h1>` header in a right-aligned vertical flex
   container holding the `.status-badge` and, when
   `currentStatus === 'Embargoed' && data.can_release_embargo`, a
   `Release Embargo` button (`#releaseEmbargoBtn.embargo-release-btn`) directly
   under the status badge.
2. Clicking `Release Embargo` disables the button (`Releasing...`), sends
   `POST /api/patchset/release-embargo?id=<data.id>`, and re-renders the
   patchset detail view so the released review findings appear immediately.

## 7. Implementation Plan

1. **Step 1:** Commit `designs/DESIGN_EMBARGO_RELEASE.md`.
2. **Step 2:** Implement `Principal::may_release_embargo`,
   `Database::release_patchset_embargo`, `can_release_embargo` on
   `GET /api/patch` and `GET /api/patchset`, and
   `POST /api/patchset/release-embargo` with unit tests and documentation in
   `prompts/sashiko/subsystem/api-auth.md`.
3. **Step 3:** Add the `Release Embargo` button and handler to
   `static/index.html`.
