# Kannaka Cross-Cutting Pattern: Wire Contracts Between Programs

Kannaka Labs is many small programs reading each other's output. Most
cross-program bugs are a producer and a consumer disagreeing about a shape that
neither side tests against the other.

## The contracts that exist

| Producer | Contract | Known consumers |
|---|---|---|
| `kannaka recall` | stdout: a bare JSON array of `{id, content, similarity, strength, layer, age_hours, times_seen}`; diagnostics on stderr; there is no `--json` flag | Node services' `runKannaka` helpers, research harness `recall_probe.py`, cron scripts |
| `kannaka status --json` | flat object; `effective_dimensionality.d_eff` and `.ratio` are strings; level under `consciousness_level` | observatory, staff, `vitals.py` |
| `kannaka export-json` | bare JSON list; each row's vector under `vector`; `hallucinated` a top-level bool | `gram.py`, backups, importers |
| `kannaka recall --remote` / `--collective` | the reply on `KANNAKA.recall.<agent>` must be an object with a `results` array | `swarm serve` responders, the CLI |
| NATS subjects | names under `KANNAKA.*`, `QUEEN.*`, `RADIO.*`, `KAX.*`, `EYE.*`; payload shapes per subject | every service; the hub ACL decides who may publish or subscribe |
| HTTP APIs | `/api/*` routes on radio, observatory, eye, staff, store | dashboards, other services, agents |
| Research row files | e.g. probe cells write the model's answer under `answer`, not `text` | grading packs (`pack.py`) |

## Rules a change must keep

1. **A reader is written against the producer's code, not memory of it.** When
   a change parses another program's output, check the producer at its current
   default branch: key names, string vs number, list vs object, which stream.
2. **A producer change names its readers.** When a change alters what a program
   prints, publishes or writes, every reader must still parse it, or the change
   must update them.
3. **Fixtures carry the producer's version.** A test of a reader uses output
   captured from the real producer with its version recorded, so a new producer
   version fails the test instead of production.
4. **Tolerate the platform.** Output captured through PowerShell redirection
   gains a UTF-8 BOM; Windows files gain CRLF. JSON readers use `utf-8-sig`
   (Python) or strip `﻿`; line splitters handle `\r\n`.
5. **Unknown shape is an error.** A reader that cannot find the field it needs
   reports that, with the producer and the field named; it does not return an
   empty result.

## On-disk formats

The HRM store, its snapshots, the refusals file, ledgers and caches are read by
later versions of the same program and sometimes by other programs. A change to
a format must keep reading the old one, or migrate it explicitly. Writes that
replace a file must be atomic (write a temporary file, then rename).
