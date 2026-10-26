# Kannaka Cross-Cutting Pattern: Tests That Can Fail

Kannaka Labs keeps a rule: a test is believed only after a deliberate mutation
of the code under test makes it fail. A passing test that could not fail has
been found many times here, and each one hid a real bug.

## Shapes of a test that cannot fail

1. **The test re-derives the answer.** It computes the expected value with the
   same formula as the code, so a wrong formula passes. The test must read the
   code's own output and compare it to an independent computation or a
   recorded value.
2. **The fixture is written from memory.** A test of a reader whose fixture was
   typed by hand from what the author believed the producer prints. If the
   producer prints something else, the test still passes. Fixtures of another
   program's output are captured from that program with its version recorded
   (see `wire-contracts.md`).
3. **The set is empty.** A loop over rows, files or findings that asserts
   something about each one passes when there are none. Assert the count first.
4. **The check is weaker than the claim.** The test name says "refuses" but it
   asserts only that no exception was raised; it says "every caller" but it
   checks one.
5. **The test filter ran nothing.** `cargo test <filter>` with a filter that
   matches no test reports success. A mutation runner must assert the expected
   test actually ran.
6. **The mock encodes the bug.** A fake broker, fake binary or fake API that
   behaves the way the code assumes, not the way the real one does.
7. **Shared state between tests.** Tests that set environment variables,
   statics or a shared temp path, run in parallel, and pass or fail depending on
   order.

## Platform

A fix for a Windows-only or Linux-only behaviour needs a test that fails on the
affected platform without the fix, or the change must say where it was
verified. CI runs on Linux; a Windows fix whose test passes on Linux either
way proves nothing about Windows there, and the change should say so.

## Environment

kannaka-memory tests that read `KANNAKA_RECALL_TEMPORAL_EXP` or
`KANNAKA_RECALL_ENERGY_EXP` behave differently when a developer's shell exports
them; tests must not depend on the caller's environment.

## Do not demand tests

- For documentation, prompt text, configuration or trivial changes.
- Where an existing test already fails on the bug the change fixes.

## When a test concern is a finding

A weak or missing test is evidence about the code, not a defect in itself. Report
it as a finding only when one of these holds:

1. **The code under it is wrong.** Then the finding is the code defect, named by
   mechanism and location; the weak test is supporting evidence in it, not a
   separate finding.
2. **New logic has no test at all.** A new function, module, binary or branch of
   real behaviour that no test executes.
3. **The change claims a test it does not contain.** The commit message or a
   comment says something is tested, and the diff does not show it.

Otherwise (the changed code is tested and the concern is more branches, stronger
assertions, another kind of test, or a fixture you would have written
differently) it is not a finding. If it is worth saying, say it once in the
summary as a suggestion, with no severity.

## Not defects of this change

- Behaviour that is pre-existing, bounded and ends in a correct state (a retry
  capped by a limit that ends in a recorded failure) is not a defect of the
  change under review, even if you would have designed it differently.
- Two outputs that report different things by design (a status that says
  "findings exist" and an exit code that says "new high findings exist") do not
  contradict each other. Check what each one is documented to mean before
  calling them inconsistent.
