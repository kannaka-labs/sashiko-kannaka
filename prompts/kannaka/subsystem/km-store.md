# kannaka-memory: HRM Store, Persistence and Store-Visible CLI Output

The store is `HrmStore` (`src/hrm_store.rs`), the only production
`MediumBackend` (trait in `src/store.rs`). It owns an authoritative
`ChiralMedium` (`src/medium/chiral.rs`, persisted by
`src/medium/chiral_persistence.rs`), a flat `Medium` view kept for older
readers, and `memory_cache: HashMap<Uuid, HyperMemory>`, which is what
`get`, `all_memories`, `count` and `search` read. `KannakaMemorySystem`
(`src/openclaw.rs`) wraps it; the CLI builds it in `init_with_hrm`
(`src/bin/kannaka.rs`). The things that matter here are data loss (a write
that never reaches disk, or a reader that overwrites the writer), on-disk
compatibility, and output shapes that other programs parse.

## 1. Which copy is authoritative

- With `chiral: Some(_)` (every production store) the **right hemisphere** is
  authoritative: `save_medium` serializes only `self.chiral`; `rebuild_cache`
  rebuilds the cache from `chiral.right.metadata`. Writing only `self.medium`
  is silent data loss. Every mutation must reach `chiral.right`, as
  `MediumBackend::insert`, `insert_raw_wavefront` and `delete` do (`delete`
  also removes the left partner via `right_to_left` and the `scales` entry).
- `HrmStore::new` is chiral from birth (`ChiralMedium::from_medium` on an
  empty `Medium`), so a store that was never reloaded has the same shape as one
  that was. `new_flat` exists for tests of the v1 path only. A diff that
  constructs production stores with `new_flat` reintroduces a "same input,
  different store" ingest bug.
- `sync_medium_from_chiral` appends right-hemisphere rows the flat view lacks
  (by id) and is called after each chiral write, or once in `end_bulk`. Readers
  of the flat view: `consciousness_metrics`, `find_associated`,
  `recall_resonance`, `recall_resonance_readonly`, `apply_observation`,
  `apply_consolidation`.
- `sync_cache_to_medium` writes back **energy only** in chiral mode
  (`min(ENERGY_CAP)`), never frequency or phase; those are owned by the
  medium and the cache copy may hold defaults (`insert_raw_wavefront`). Do not
  "complete" this sync.
- Cache-only fields survive `rebuild_cache` only because it snapshots and
  restores them: connections (filtered to live targets), `retrieval_count` and
  ghost `updated_at`, `layer_depth` / `last_consolidated_at`, `times_seen`, and
  ghosting itself (`amplitude <= 0.0`, restored last). A new cache-only field
  must be added to this snapshot/restore set or it resets on every dream and
  absorb.

## 2. Write and read paths

- `insert` rejects `DuplicateId`, clamps `amplitude` to `[0,1]` (non-finite
  -> 0), `frequency` to `[0, 1e6]`, non-finite `phase` -> 0, caps energy at
  `ENERGY_CAP`, rewrites the minted chiral id to the caller's `memory.id`
  (`update_right_id`), and pushes to `new_ids`. It deliberately does **not**
  carry `tier`, `modality` or the temporal triple across; callers that need
  them apply `set_modality` / `set_tier` / `set_temporal` after insert (import
  does, for operator-chosen files only). Do not widen `insert` to carry them:
  it is reached by wire sync.
- `get_mut` marks the whole store dirty on acquisition. `record_retrieval`
  bumps the cache and sets `retrieval_dirty` instead, so a pure recall flushes
  only the `.reactivation.json` sidecar. A diff that routes a read-only bump
  through `get_mut` turns every recall into a full `.hrm` rewrite.
- Recall observes: `apply_observation` and the chiral branch of
  `resonate_query_inner` call `observe_wavefronts` and `mark_dirty` unless
  `KANNAKA_RECALL_OBSERVE` is off. `recall_resonance_readonly` is the only
  non-mutating recall; it is used by `export-recall-scenarios`. The beam path
  (`KANNAKA_RECALL_BEAM`) returns `None` to fall back to the dense scan on any
  failure or empty beam; it must never turn a failure into "no memories".
- `search` scores normalized cosine over the live `memory_cache`, not the flat
  view (which may hold dead rows).
- Ranking knobs read by `src/medium/hemisphere.rs`:
  `KANNAKA_RECALL_ENERGY_EXP` and `KANNAKA_RECALL_TEMPORAL_EXP` both default
  to `0.0`, clamp to `[0,1]`, and are read per call. `SuppressTemporalScoring`
  forces the temporal exponent to 0 for dedup/admission recalls.

## 3. Persistence and on-disk compatibility

- `.hrm` format: v1 (flat) or v2 (`HRM_MAGIC_V2`, chiral). `HrmStore::load`
  sniffs the magic; `ChiralMedium::load` handles both and auto-converts v1; a v2
  file that fails to load is a hard error (no v1 fallback). Files end in a
  content id, `CONTENT_ID_TAG` and a 32-byte blake3 checksum
  (`verify_blake3_trailing`). Any change to `write_hemisphere`, the header, or
  the trailer must keep existing files loading; there is no migration step.
- `ChiralMedium::save` writes `<file>.hrm.tmp.<pid>.<nanos>`, `sync_all`,
  then `rename`, and sweeps old siblings. Sidecars go through
  `fs_util::atomic_write_bytes` (UUID temp sibling, `sync_all`, rename).
  `HrmStore::load` sweeps `.kannaka-tmp-*` older than an hour. New file writes
  next to the store should use `fs_util`, not `std::fs::write`.
- Sidecars, all named from the hrm path with `with_extension`:
  `links.json`, `reactivation.json`, `times_seen.json`, `clusters.json`. The
  reactivation and times_seen savers merge on write (`*_merge`); only the full
  save (`prune_stale = true`) may drop stale ids. `<data_dir>/.encoder` stamps
  the encoder; `build_encoding_pipeline` refuses a mismatch (exit 2) unless
  `KANNAKA_ENCODER_FORCE=1`.
- `clamp_persisted_energy` runs on every load, before `rebuild_cache`.
- `Drop for HrmStore` saves when `dirty || retrieval_dirty`. Anything that
  writes the `.hrm` file directly while a loaded `HrmStore` is alive in the same
  process can be overwritten when that store drops.

## 4. Single writer

- `KANNAKA_READONLY` (any non-empty value other than `0`/`false`, see
  `env_readonly`) or `set_readonly(true)` makes `save_medium` clear both dirty
  flags and return `Ok`, and makes `take_new_memory_ids` return nothing.
  `swarm serve` and `attention serve` force it on themselves. A reader that can
  persist clobbers the writer (last writer wins over the whole file).
- The advisory write lock is `<data_dir>/.kannaka-write.lock`
  (`try_acquire_write_lock`, `libc::flock`, Unix only; on other platforms it
  always succeeds). `dream` probes it before the HRM load and acquires it
  authoritatively in the dream arm, exiting 0 when held. `swarm join` holds it
  for its lifetime via `acquire_write_lock_blocking(60)`, which **returns
  `None` and proceeds** after the timeout. `remember`, `import` and most other
  writers take no lock. The lock path is under `data_dir()`, not `store_dir`.
- Two parsers of `KANNAKA_READONLY` exist with different rules: `env_readonly`
  / `readonly_env_active` (non-empty, not `0`/`false`) and the `== "1" ||
  "true"` checks in `swarm join`'s lock decision and `KannakaConfig::load`.
  A diff that adds a third, or relies on them agreeing for values like `yes`,
  is wrong.

## 5. Dream and consolidation

`KannakaMemorySystem::dream` runs `store.dream_native` (wave annealing), then
resonance-merge consolidation (`ConsolidateOpts::from_env`; destructive apply
only with `KANNAKA_CONSOLIDATE=on`, otherwise a dry-run plan), the particle
`ConsolidationEngine::consolidate` stages in `src/consolidation.rs`
(replay, detect, bundle, strengthen, sync or `DREAM_MODE=interference_relax`,
xi-repulsion, hallucinate, prune, transfer, retention triage when
`KANNAKA_TRIAGE=1`, compact ghosts, wire, optional chiral perturbation,
kannaktopus), then saves if `auto_save`.

- Pruning ghosts (`amplitude = 0.0`); only `stage_compact_ghosts` hard-deletes,
  and only ghosts older than `KANNAKA_GHOST_RETAIN_DAYS` (default 7) that were
  not ghosted in this cycle (`cycle_started_at`), never `Tier::Pinned`.
- Determinism: with `KANNAKA_DREAM_ENTROPY` off (default) `apply_dream_entropy`
  draws nothing, perturbs nothing and stamps no provenance; with it on, a draw
  happens only when `dream_entropy_touched_count() > 0`. Tests named
  `dream_entropy_gate_off_is_deterministic_and_records_no_provenance` and
  `dryrun_apply_parity*` pin this; a change that draws randomness on the default
  path breaks them.
- Remember events from a dream are announced only after the dream's save
  (`begin_dream_writes` / `end_dream_writes`); `save` publishes pending ids
  only after a successful flush.

## 6. Snapshot and restore (`src/bin/handlers/substrate.rs`)

`capture_and_publish_snapshot` flushes, gzips `data_dir().join("kannaka.hrm")`
to `<data_dir>/snapshots/<ts>-<agent>.hrm.gz` (plain `std::fs::write`), prunes
to `KANNAKA_SNAPSHOT_RETAIN` (default 24 for `kannaka-substrate`, else 168),
and publishes a manifest-only `EventPayload::SnapshotFull`.
`handle_events_restore` gunzips a body, renames the current `kannaka.hrm` to
`kannaka.hrm.pre-restore-<ts>` and writes the new one; it takes no write lock
and runs after `main` has already loaded the HRM. Both hard-code
`data_dir().join("kannaka.hrm")` rather than the configured `hrm.path`.

## 7. Data directory resolution

Three copies of the same precedence exist and differ at the edge:
`kannaka.rs::data_dir` / `dirs_or_default` and `acp::data_dir` use
`KANNAKA_DATA_DIR`, else `~/.kannaka` **only if it exists**, else relative
`.kannaka`; `KannakaConfig::data_dir` returns `~/.kannaka` whether or not it
exists. `KannakaConfig::load` resolves `hrm.path` with `resolve_hrm_path`: a
relative path joins `data_dir`; an absolute path outside an explicit
`KANNAKA_DATA_DIR` is replaced by `<data_dir>/<file name>` unless
`KANNAKA_ALLOW_EXTERNAL_HRM` is truthy. Store-coupled sidecar state belongs
under `store_dir(cfg)` (parent of `hrm.path`), not raw `data_dir()`.

## 8. Store-visible output shapes

- `export-json` prints one line: a **bare JSON array** (no envelope). Each
  object: `id` (string), `content`, `amplitude`, `frequency`, `phase`,
  `decay_rate`, `created_at` (RFC 3339 string), `layer_depth`,
  `hallucinated` (top-level bool), `modality`, `tier`, `effective_at` /
  `observed_at` / `expires_at` (string or null), `provenance`, `parents`,
  `connections` (`target_id`, `strength`, `span`); without `--slim` also
  `vector`, `xi_signature`, `geometry`. The embedding key is `vector`.
- `export` (`handle_export` in `handlers/ops.rs`) is a different shape: always
  `vector`, but no `modality`, `tier`, temporal stamps or `provenance`.
- `import` / `import-json` share `import_memories_from_file`: parses a bare
  array, keeps `id`, skips existing ids and empty content, re-encodes when
  `vector` is missing or empty, applies `modality` after insert.
  `import-json` prints `{"imported","skipped","errors","total_input"}`.
- `status` always prints pretty JSON (there is no `--json` flag; unknown flags
  are ignored). Numbers: `total_memories`, `active_memories`, `phi`, `xi`,
  `mean_order`, `num_clusters`, `memories_without_embeddings`,
  `irrationality`, `hemispheric_divergence`, `callosal_efficiency`,
  `effective_dimensionality.nominal`. **Strings**: `consciousness_level`,
  `last_dream` (RFC 3339 or null), `field_mode` (`"HRM"`), and
  `effective_dimensionality.d_eff`, `.ratio`, `.irrational_remainder`
  (`format!`-ed). `modality_distribution` is an object of counts. `--envelope`
  wraps it as `{schema_version, command, data, errors}`.

## Bug patterns to look for

1. A mutation applied to `self.medium` or the cache but not `chiral.right` (lost at next load).
2. A cache-only field not added to `rebuild_cache`'s snapshot/restore.
3. A read path that calls `get_mut` or `mark_dirty` without need, or a recall path that skips `KANNAKA_RECALL_OBSERVE`.
4. A long-running reader that does not force read-only, or a writer that persists under `KANNAKA_READONLY`.
5. A new sidecar or snapshot write with `std::fs::write` beside the store, or a path built from `data_dir()` where `store_dir`/`hrm.path` applies.
6. Changing a key name, type (string vs number) or nesting in `export-json` or `status`; downstream programs parse them.
7. A change to the `.hrm` byte layout without load compatibility for existing v1/v2 files.

## Not a bug here

- `insert` dropping `tier`/`modality`/temporal fields; `bias`, `dream` and recall mutating energy.
- Ghosts (`amplitude == 0`) remaining in `all_memories()` until compaction.
- The flat view lagging until `sync_medium_from_chiral`; `medium()` may be stale by design.
- `status` emitting some numbers as strings; changing them to numbers is the breaking change.
