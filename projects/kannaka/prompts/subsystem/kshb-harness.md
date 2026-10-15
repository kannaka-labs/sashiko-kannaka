# KSHB Harness: the System Health Benchmark Instruments

Covers `kshb/` in kannaka-scientist: `plant.py`, `recall_probe.py`,
`vitals.py`, `gram.py`, `recovery.py`, `pack.py`, `report.py`,
`thresholds-v0.json`, `report.schema.json`, and `kshb/tests/` (including
`fake_kannaka.py` and `fixtures/`). The contract is
`docs/specs/2026-10-08-kshb-v0-DRAFT.md`; section numbers in code comments
(2.a, 2.d, 3, ...) refer to it.

These instruments turn the output of a real `kannaka` binary into research
measurements that get published. The failure that matters is not a crash. It
is a number that looks like a measurement and is not one: a refused child
scored as a miss, a wrong key read as zero, a proportion coloured green on
luck, a threshold moved after the data was seen. Four invariants, all
enforced in code today:

1. **Refuse rather than score.** When an instrument's input is not a
   measurement, the output carries no figure and says why (`refused`,
   `cell_refused`, `evaluable: False`, `unmeasured`, or a raised
   `ValueError`). A refusal must never be readable as a low or a passing value.
2. **Real output shapes, captured.** Parsers are tested against output written
   by the real binary (`kshb/tests/fixtures/*-<version>.json`) or a fake that
   reproduces the captured shape, not against a shape someone assumed.
3. **Proportions carry intervals.** Wilson intervals, never a bare point on 20
   or 40 items.
4. **Pre-registration is fixed before data.** The planted file is sealed by
   sha256; the thresholds file's sha256 is stamped into every report.

The package is stdlib-only except `gram.py` (numpy, imported inside the
function). It makes no paid API call and never starts, restarts or connects to
a live service; a diff that adds either is a deviation the spec requires an
amendment for.

## 1. The seal (`plant.py`)

`generate(seed)` builds 40 synthetic facts with 12-hex nonces, paraphrases that
share no non-stopword 4-gram with their fact (`shares_4gram`), and exactly ten
`SELF_QUESTIONS`; `canonical()` is `json.dumps(sort_keys=True,
separators=(",", ":"), ensure_ascii=True)`. `check(doc)` enforces: unique
nonces, `[0-9a-f]{12}`, nonce absent from its paraphrase, each nonce occurring
exactly once across facts, paraphrases and questions, no shared 4-gram, ten
questions. `verify(path, sha)` returns False on a hash mismatch and **raises**
on a matching hash with broken invariants.

Any change to `canonical`, `STOPWORDS`, the vocabularies or `check` changes
what a seed produces or what a seal accepts. That is a pre-registration change
and must not land between freeze and the end of a window.

## 2. Recall (`recall_probe.py`)

`main` refuses (exit 2, nothing run) unless `plant.verify` passes; it catches
`OSError`, `ValueError`, `KeyError`, `JSONDecodeError` as refusals. Each query
runs `<bin> recall "<query>" --top-k K` as its own child with
`KANNAKA_NATS_URL` set to `UNREACHABLE_NATS` (`child_env`), so a planted query
never reaches the swarm. A diff that drops or makes that override optional
publishes planted facts.

The command has **no `--json` flag**: on 0.16.15 `recall` prints a bare JSON
array on stdout by default and exits 2 on `--json`. `fake_kannaka.py` exits 2
on `--json` to keep that pinned, and `test_recall_probe.py` asserts the argv.
Adding the flag back makes every row a refusal.

`run_query`: a non-zero exit or a timeout sets `refused: True`, `rank: None`,
`hit: False`, keeps the last 300 characters of stderr. `summarize`: if any row
is refused, `cell_refused` is True and `recall@K` is `None`; `main` then exits
3. **A refused row must never be counted as a miss**: an rc-2 child scored as
rank None reads exactly like forgetting.
`rank: None` on a successful child is a legitimate miss: the binary has no
similarity floor and always returns top-k rows.

Parsing: `rank_in_json` searches `extract_results` (bare list, or a list under
`RESULT_KEYS`) for the needle in each result's serialisation. If stdout is not
JSON, `rank_in_text` ranks by non-empty stdout line, and the row records
`parser: "text"`. A text-parsed rank is a line number, not a result rank; the
summary lists `parsers` so a cell that fell back is visible. Nothing refuses a
text-parsed cell today, so a diff that broadens the text fallback widens what
is scored without a captured shape behind it.

## 3. Substrate (`vitals.py`, `gram.py`)

`vitals.parse_status` maps `kannaka status --json` through `FIELDS` with a
depth-first `find`. `num()` coerces numeric strings (0.16.15 prints
`effective_dimensionality.d_eff` and `.ratio` as strings) and turns anything
else, including bools, into `None`, so a later comparison never compares text.
Flags: `memories_without_embeddings > 0` and `active > total`. A missing field
is `None`, not 0. `main` reads with `utf-8-sig` because a PowerShell-captured
file carries a BOM.

`gram.gram_stats` reads vectors via `vector_of` (`vector` from `export-json`,
`embedding` from older fixtures), skips and counts rows without one, and raises
`ValueError("no embeddings in rows")` when none remain; `main` falls back to
`[]` for an unrecognised top-level shape, which therefore raises rather than
reporting zeros. `is_dream` accepts top-level `hallucinated` (0.16.15) or
`metadata.hallucinated`. `d_eff` is the participation ratio of the clipped Gram
eigenvalues. The scored quantity is `dream_gram_mass_share` =
`||G_dd||_F^2 / ||G||_F^2` (squared), not the unsquared Frobenius ratio and
not the trace share; a diff that swaps one for another changes every published
figure.

`score_dream_share` is **not evaluable** when either day is missing its count
or share, or when both days have zero dream rows: a dreamless store and an
instrument reading the wrong key both print 0.00 with zero tremor, and only the
row count tells them apart. `dream_share_tremor` computes drop-one exactly from
the full Gram and seeded drop-fraction repeats; `inside_tremor` is reported
beside the delta.

## 4. Recovery (`recovery.py`)

Pure functions over a nats-server log and a connz dump; nothing here signals
anything. `r2_refusals` returns `total: 0` both when no violation occurred and
when no line matched the `LINE` regex, and `r3_connections` skips connections
whose `uptime` does not parse. Those zeros are only as good as the regex, so a
change to `LINE`, `UPTIME` or the field names must come with a line captured
from a real journal (`test_the_real_journal_and_connz_shapes_are_read`).
`r1_time_to_first_correct` returns `None`, not a large number, when no probe
was correct.

## 5. Blinding (`pack.py`)

`build_pack` shuffles with `random.Random(seed)`, assigns opaque `P0001` ids,
strips every `--strip` string case-insensitively (longest first) and **raises
if any survives**, and copies provenance (`id` plus `WITHHELD`) only into the
key. `answer_text` raises when a row has neither `text` nor `answer`; an empty
answer packed as "" would blind nothing and grade as nothing. `unblind` raises
on an unknown pid. A field added to the pack entry beyond `pid`, `text`,
`question` is a blinding leak unless it is provably non-identifying.

## 6. The report (`report.py`, `thresholds-v0.json`)

`colour_metric`:
- raises unless the threshold names an `instrument_author`;
- `unmeasured` with a reason is amber with `point_pass: None` (scores 0.5);
- `k`/`n` without `point` becomes `point = k/n` with a `wilson()` interval;
- raises when there is no point;
- raises when `needs_interval` is set and no interval is present;
- red when the point fails; green only when the point and both interval ends
  pass; amber otherwise.

At n = 20 to 40 a proportion is two-colour (amber or red), so the dimension
**score** reads `point_pass`, and the dimension **colour** is the worst metric
colour. `build_report` raises on a metric name absent from the thresholds
table, and on a footprint missing any of `FOOTPRINT_REQUIRED`. The composite is
always printed with its weights and `NOTE`; it is never the headline.
`instrumentation.self_instrumented_metrics` lists every metric whose
`instrument_author` contains the claimant. `thresholds_sha256` is the hash of
the thresholds file bytes as read.

`wilson` uses z = 1.959963984540054 and raises on `n <= 0` or `k` out of range.
`t2_slope` is not evaluable under `min_rows` (20) decided rows in either pool;
rising means `late.lower > early.upper`; it reports `late_min_rising`, the
smallest late count that could have read rising. `sign_test` is exact two-sided
and returns 1.0 for no untied pairs.

`build_report` does not read a metric's `status` field. The three recovery rows
whose `status` says "calibration ... reported, not scored" are coloured and
scored like any other if a value is supplied; keeping them out of the score
currently means supplying them as `unmeasured` with a reason.

**Editing `thresholds-v0.json` is changing the pre-registration.** A diff that
moves a pass value, an op, `needs_interval`, a weight, or a metric's dimension
must cite a spec amendment dated before the window's data. Removing
`needs_interval` from a proportion re-enables colouring a bare point.

## 7. Tests and fixtures

Run with `python -m unittest discover -s kshb/tests -t .`. `test_binary_fixtures.py`
loads `status-0.16.15.json` and `export-json-0.16.15.json`; each fixture
records its `binary`. When a new binary changes a shape, the rule is to capture
a new fixture under a new versioned name and keep the old one, so the change
fails in these tests rather than in a campaign. Overwriting the existing
fixture with the new shape deletes the evidence of the old one. Recall has no
captured fixture; its shape is pinned by `fake_kannaka.py`'s `json` mode.

## 8. What is NOT a bug

- `rank: None` with `refused: False`: a genuine miss.
- An amber dimension with score 1.0: a perfect small-n cell whose interval
  cannot fit the pass range.
- `recall@K: None` on a cell with counts and timings: a refused cell.
- `evaluable: False` with a reason on the 2.d or T2 clause: the clause could
  not run, and must colour nothing.
- `gram.py` failing without numpy: the test is skipped, the rest of the
  package does not need it.

## Checklist

- [ ] Any new path where non-zero exit, timeout, missing key, unparseable line
      or empty input yields a number instead of a refusal?
- [ ] Any parser change without a fixture captured from the binary it claims
      to read, with the version in the file name?
- [ ] Any proportion reported, scored or coloured without its interval?
- [ ] Any change to `plant.py` generation or `check`, the thresholds file, or
      the scored quantity in `gram.py`, outside a dated amendment?
- [ ] Any child process launched without the `UNREACHABLE_NATS` override, or
      any network or paid-API call added?
- [ ] Any pack field that could carry system, day, model or host identity to a
      grader?
