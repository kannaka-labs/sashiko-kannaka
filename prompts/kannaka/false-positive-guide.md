# False Positive Prevention Guide

Used where avoiding false positives matters most. Shift bias away from fast
processing and follow it.

## Core principle

**If you cannot prove an issue exists with concrete evidence from this code,
do not report it.**

Evidence is code you have read: the function, the path, the input that reaches
it. Proving a path is structurally possible is enough; you do not have to show
it runs every time. A permanent refusal retried on every call is a bug even if
the hub has not been flooded yet.

## Patterns that produce false positives here

### 1. Linters and compilers already ran

`cargo clippy`, `cargo test`, `tsc`, `node --check`, `bash -n`, `py_compile`
and each repository's CI run before a person sees the change. Do not report
formatting, naming, unused code, import order or anything those tools report.
Never claim a change "fails to compile" or "breaks the build"; if you think so,
you have misread the code.

### 2. "Add a check for safety"

Do not ask for validation unless you can show all three: the value comes from
somewhere untrusted or variable (another program's output, a NATS message, an
HTTP request, a config file, a model response, a platform difference), a
concrete path carries it to the code, and the code misbehaves on a value that
path can produce.

- Bad: "This should validate the JSON before use."
- Good: "`run_query` keeps the row when the child exits non-zero, and
  `summarize` counts it as a miss, so a binary that rejects a flag produces a
  low recall figure instead of a refused cell."

### 3. Assuming the other program behaves

Many findings here depend on what another program prints, publishes or
accepts. Do not dismiss a concern by assuming the producer always sends the
expected shape or the broker always accepts a publish, and do not report one by
assuming it never does. Read the producer or the ACL if it is visible. If it is
not, report the concern with the assumption stated ("if `kannaka` exits
non-zero here, the row is scored as a miss") and cap it at `Medium`.

### 4. Assuming a caller handles it (symmetrical proof bar)

Do not dismiss a defect in the changed code by assuming the surrounding system
masks it unless specific code makes it impossible across all callers (`git_grep`
every caller: CLI, cron scripts, HTTP routes, NATS handlers, hooks). Never
dismiss a swallowed error, an unpaired lock or claim, or an overwritten state
as a "harmless no-op".

### 5. Platform scope

Windows and Linux are both in scope. Do not dismiss a platform difference as
out of scope; do not report one without the concrete line that behaves
differently and the platform where it does. A file that only runs as a systemd
unit on Linux (`deploy/*.service`, a script invoked only by one) is not a
Windows finding.

### 6. Tests and fixtures

A missing test is a finding only when the change is non-trivial and nothing in
the existing tests would fail if the change were reverted. Do not demand tests
for documentation, prompt text or configuration.

### 7. Research code

In the research harnesses, a refusal to score (`refused`, `not evaluable`,
`None`) is the intended behaviour for bad input, not a bug. The bug is the
opposite: a score produced from input that was not a measurement.
