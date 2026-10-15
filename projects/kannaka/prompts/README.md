# Kannaka Labs Review Prompts

First-party review prompts for Kannaka Labs codebases, used by the `kannaka`
project profile (`--project kannaka`). Kannaka Labs addition to the
`sashiko-kannaka` fork; the Sashiko engine, schemas and validators are unchanged.

The workflow that loads them is `src/workflows/kannaka_patch_review.rs`. It runs
the same graph as Sashiko's own service review (pre-screen, planning, parallel
analysis stages, verification, per-finding post-verification, report, summary)
with Kannaka stage text and these stages:

| Stage | Always runs | Guides it loads |
|---|---|---|
| `goal` | yes | — |
| `implementation` | yes | — |
| `execution-flow` | yes | `patterns/silent-failure.md`, `patterns/error-handling.md` |
| `concurrency` | planner | `patterns/concurrency.md`, `patterns/retries-and-refusals.md` |
| `wire-contracts` | planner | `patterns/wire-contracts.md` |
| `platform` | planner | `patterns/platform-differences.md` |
| `security` | planner | `prompt-injection.md`, `patterns/secrets-and-money.md` |
| `interfaces-compat` | planner | `patterns/public-surfaces.md` |
| `tests` | planner | `patterns/tests-that-can-fail.md` |

## Layout

| Path | Loaded by |
|---|---|
| `review-core.md` | Orientation: what Kannaka Labs builds and what a bad change costs |
| `subsystem/subsystem.md` | The index the pre-screen stage selects component guides from |
| `subsystem/*.md` | Per-codebase invariants, contracts and bug patterns |
| `patterns/*.md` | Cross-cutting concerns, loaded by the stages above |
| `false-positive-guide.md`, `severity.md` | Verification and post-verification |
| `github-summary-template.md` | The report stage |
| `prompt-injection.md` | The security stage |

A selected guide name is looked up in both `subsystem/` and `patterns/`, so
names must be unique across the two.

## The bar for content

Everything stated must be true of the code it names, anchored to real file
paths and symbols, and useful for deciding whether a specific change is wrong.
No style or lint advice; no hypotheticals. When code changes, the guide that
describes it changes in the same pull request.
