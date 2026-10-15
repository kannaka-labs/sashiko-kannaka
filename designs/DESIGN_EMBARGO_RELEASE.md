# Design: Maintainer Embargo Visibility and Early Release

## 1. Problem Statement

When a patch series matches a subsystem policy with `embargo_hours > 0` in
`email_policy.toml`, Sashiko sets `patchsets.embargo_until` to hold back public
review disclosure until the embargo window expires:

1. While `embargo_until > now`, `get_patchsets`, `get_patchset_details`, and
   `get_patchset_summary` report the patchset status as `"Embargoed"`, hide the
   `reviews` array, and zero out finding counts for public callers.
2. During review execution, `Reviewer::process_patch_review` skips queueing
   outbound email and Patchwork notifications while `embargo_until > now`, and
   `Reviewer::queue_forge_pr_comment` queues forge outbox rows in the
   `"Embargoed"` state.
3. In the background, `Reviewer::release_embargoed_results` polls
   `Database::get_releasable_embargoed_patchsets` every 10 seconds and releases
   patchsets whose embargo has expired (`embargo_until <= now`) or that completed
   cleanly without findings.

Public embargoes exist to give subsystem maintainers time to inspect findings
before public disclosure, not to withhold results from the maintainers
themselves. Maintainers should never have to wait for an embargo timer—or
release the embargo publicly first—just to see review results on their own
subsystems, and they should also be able to lift the public embargo early when
appropriate.

## 2. Goals

1. Allow authorized maintainers and reviewers to view embargoed review results
   and finding counts immediately on `GET /api/patch`, `GET /api/patchset`, and
   `GET /api/patchsets` without waiting for `embargo_until` to expire.
2. Keep the patchset status displayed as `"Embargoed"` (along with the
   `Release Embargo` button when mutations are permitted) so maintainers can see
   at a glance that the results are still held from the public.
3. Allow authorized maintainers and reviewers to release the embargo on an
   embargoed patchset early from the web UI (`#/patchset/<id>`) and via HTTP
   (`POST /api/patchset/release-embargo`).
4. Reuse Sashiko's stateless `Principal` + `MAINTAINERS` attribution model
   (`patchset_maintainer_sections`) alongside `Permission::Review` in
   `[server.acl]`.
5. Ensure releasing an embargo via the API immediately makes the review findings
   publicly visible while preserving the asynchronous outbox release path in
   `Reviewer::release_embargoed_results` so held-back email, Patchwork, and
   forge notifications are still delivered exactly once.

## 3. Authorization Model

A caller's embargo authority on patchset `P` requires that the caller's
presented identity or token `sid` is not in `[server.acl].blocklist`, and at
least one of the following holds:

1. **Global Review Capability (`Permission::Review`):**
   `is_authorized(&state, &headers, auth, Permission::Review)` is true. This
   covers:
   - `[server.acl].admins` and `[server.acl].review` (for interactive browser
     sessions),
   - the local operator token (`.sashiko-local-token`),
   - `server.testing_mode` and `--enable-unsafe-all-submit`.
2. **Subsystem or Global Maintainer Standing (`Principal`):**
   The caller's `Principal` (resolved from their session JWT or a scoped API
   token) satisfies `principal.may_view_embargoed(&titles)` (for reading
   embargoed results) or `principal.may_release_embargo(&titles)` (for releasing
   an embargo when `!state.read_only`), where `titles` are the normalized
   `SectionTitle`s returned by
   `Database::authorizing_sections_for_patchset(patchset_id)` (`source =
   'maintainers_section'`). This grants access to:
   - Sashiko operators (`[server.acl].admins`),
   - global maintainers (`THE REST` in `MAINTAINERS`),
   - maintainers and reviewers (`M:` / `R:`) of any `MAINTAINERS` section
     touched by the patchset.

### Read vs. Release Distinction

- **Viewing Embargoed Results (`may_view_embargoed`):** Permitted on `read_only`
  servers and for scoped API tokens whose `max_bug_access` ceiling permits at
  least `BugAccess::Read`.
- **Releasing an Embargo (`may_release_embargo`):** Mutates public visibility and
  triggers outbound notifications, so it requires `!state.read_only` and a
  ceiling of `BugAccess::Manage` (or no ceiling).

### Security Invariants

- **Blocklist Precedence:** Blocklisted email addresses and revoked token `sid`s
  are rejected before any grant (`testing_mode`, `allow_all_submit`,
  `Permission::Review`, or `MAINTAINERS`).
- **Fail-Closed on Unattributed Patchsets:** If a patchset has no
  `source = 'maintainers_section'` rows in `patchset_maintainer_sections`,
  section-maintainer standing grants nothing; only operators, global maintainers,
  and callers holding `Permission::Review` may view or release its embargo.
- **Scoped API Token Attenuation:** A scoped `api_token` JWT is rejected by
  `is_authorized` for global `Permission::Review`. In `Principal`, a `"none"`
  ceiling denies both viewing and releasing embargoed results, while a `"read"`
  or `"comment"` ceiling permits viewing embargoed results on maintained
  sections without permitting early release.
- **Zero-Query Anonymous Fast Path:** When a request carries no credentials and
  neither `testing_mode` nor `allow_all_submit` is active, authorization resolves
  to `false` in memory without querying `patchset_maintainer_sections`.
- **Cache Isolation on `GET /api/patchsets`:** `patchsets_homepage_cache` stores
  the public (embargo-redacted) `Vec<PatchsetRow>`. Per-caller finding counts for
  embargoed rows are populated on the cloned response vector after cache lookup,
  so one maintainer's embargo visibility never leaks into the shared homepage
  cache.

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

1. **Immediate Public UI & API Visibility:** `get_patchsets`,
   `get_patchset_details`, and `get_patchset_summary` compute `is_embargoed` as
   `embargo_until > now`. As soon as `embargo_until` is set to `now`,
   `is_embargoed` evaluates to `false`, so the very next read returns
   `status: "Reviewed"`, all completed `reviews`, and full finding counts to all
   callers.
2. **Guaranteed Outbox Delivery:** Because `embargo_until` remains `NOT NULL`
   and satisfies `embargo_until <= now`, the background `Reviewer` loop claims
   the patchset via `claim_patchset_embargo_release`, queues email and Patchwork
   notifications, transitions `forge_outbox` rows from `'Embargoed'` to
   `'Pending'`, and finally clears `embargo_until = NULL` via
   `clear_patchset_embargo`.

## 5. HTTP API Changes

### 5.1 `GET /api/patch` and `GET /api/patchset`

When a patchset is currently embargoed (`status == "Embargoed"`):

- The patchset `status` remains `"Embargoed"` so the caller knows public
  disclosure is still held back.
- If the caller is authorized to view embargoed results on the patchset, the
  `reviews` array is populated via `Database::get_patchset_reviews_for_patches`
  (`include_logs = true` for `/api/patch`, `false` for `/api/patchset`) rather
  than being redacted to `[]`.
- `can_release_embargo` is `true` when the patchset is embargoed and the caller
  is authorized to release it (`!state.read_only`).

### 5.2 `GET /api/patchsets`

When `list_patchsets` returns a page containing rows with
`status == Some("Embargoed")`:

- The row `status` remains `"Embargoed"`.
- For each embargoed row the caller is authorized to view,
  `findings_low`, `findings_medium`, `findings_high`, and `findings_critical`
  are populated via `Database::get_patchset_finding_counts` in a single batch
  query. Embargoed rows outside the caller's authority retain zeroed counts.

### 5.3 `POST /api/patchset/release-embargo`

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
   container holding the `.status-badge` (with an `Embargoed until ...` tooltip
   when `embargo_until` is present) and, when
   `currentStatus === 'Embargoed' && data.can_release_embargo`, a
   `Release Embargo` button (`#releaseEmbargoBtn.embargo-release-btn`) directly
   under the status badge.
2. When `data.reviews` is populated for an authorized maintainer on an embargoed
   patchset, `renderPatchsetView` renders the full per-patch review cards and
   the "Potential Regressions" summary table immediately while keeping the
   `Embargoed` badge and `Release Embargo` button in the header.
3. Clicking `Release Embargo` disables the button (`Releasing...`), sends
   `POST /api/patchset/release-embargo?id=<data.id>`, and re-renders the
   patchset detail view.
