# Tasks — diarization-clustering-perf

> Branch: `enhance/diarization-clustering-perf` (work landed on
> `enhance/notification-actions` — see branch note at the bottom).
> Pure-Rust change in `frontend/src-tauri/src/audio/speaker/sherpa_adapter.rs`
> (the `cluster_by_centroids` function and the `build_chunks` split granularity)
> plus the non-blocking wrap of `adapter.process()` in `commands.rs`. No DB
> migration, no frontend, no model change, no new dependency. No new Playwright smoke
> spec — pure compute optimization with a property-test correctness oracle. The
> `diarization-complete` → speaker-badge UI wiring is already covered by
> `e2e/smoke/speaker-diarization.spec.ts` (tests 15.2/15.3/15.6); the perf change
> introduces no new UI behavior.

## 1. RED — adversarial clustering tests

- [x] 1.1 **Behaviour-identity oracle:** `oracle_new_equals_naive_across_grid` —
  parametric over n ∈ {0,1,2,5,20,80}, thresholds ∈ {0.30,0.40,0.55}, geometries
  {well-separated, overlapping-random, all-identical, all-mutually-orthogonal}.
  Asserts labels equal exactly and centroids equal within 1e-5 to the naive
  oracle. Passed.
- [x] 1.2 **Oversized input is bounded:** `oversized_input_is_bounded`
  (`#[ignore]` — n=5000 with dim=192 takes ~90s due to O(n²·d) init + O(n³/3)
  scan; the production-relevant bound is n=600 via D2, tested in
  `oversized_matrix_stays_bounded_by_chunk_cap`). Both verify no panic / no OOM.
- [x] 1.3 **Empty / single chunk:** `empty_and_single_chunk_no_panic` — n=0 →
  empty labels + empty centroids; n=1 → single cluster labelled 0.
- [x] 1.4 **Degenerate geometries:** `degenerate_geometries_match_oracle` —
  all-identical → 1 cluster; all-mutually-orthogonal → n distinct clusters;
  both match the naive oracle.
- [x] 1.5 **Chunk cap coarsens long meetings:** `chunk_cap_formula_coarsens_long_meetings`
  — pins the `effective_split = max(3.0, speech_seconds/600)` formula; at
  5400s of speech, effective_split = 9.0s and chunk count ≤ 600. (Full
  `build_chunks` integration requires a real model extractor — covered by the
  `#[ignore]` real-audio diarization tests + manual QA in 3.2.)
- [x] 1.6 **Short meetings unaffected:** `short_meetings_keep_default_granularity`
  — at 600s (10 min), effective_split = 3.0 exactly; chunk count equals today's.
- [x] 1.7 **Non-finite embeddings dropped:** `non_finite_embeddings_do_not_corrupt_clustering`
  — a NaN-embedding chunk does not panic and does not corrupt other clusters'
  labels (NaN > threshold is false, so it never merges).

## 2. GREEN — implement the optimization

- [x] 2.1 `cluster_by_centroids` rewritten with a cached upper-triangle
  similarity matrix (`sim[a][b-a-1]`), O(1) lookups in the scan (same
  double-loop structure, same `>` predicate, same iteration order →
  byte-for-byte identical merge decisions to the oracle), and selective
  row-a + column-a recompute on each merge (O(k·d)). Design D1 (amended from
  the originally-proposed max-heap — see Decision 5 for why).
- [x] 2.2 Previous implementation kept as `cluster_by_centroids_naive` behind
  `#[cfg(test)]` — the oracle for task 1.1.
- [x] 2.3 `const MAX_DIARIZATION_CHUNKS: usize = 600` added;
  `effective_split = max(SPLIT_TARGET_SECS, speech_seconds / 600.0)` computed
  in `build_chunks` and used in the long-segment split loop.
- [x] 2.4 `adapter.process()` wrapped in `tokio::task::spawn_blocking` at
  `commands.rs:360` — verified the adapter IS `Send` (compiles clean). A
  why-comment notes the non-blocking requirement so a future refactor cannot
  regress it. (The design assumed this was already the case; the trace showed
  it was inline-async, so the wrap is a real code change, not verification-only.)
- [x] 2.5 `cargo test --lib audio::speaker::sherpa_adapter` — 31 passed, 0
  failed, 1 ignored.

## 3. Verify

- [x] 3.1 `cargo test --lib` (full Tauri crate) — 408 passed, 0 failed, 8 ignored.
- [x] 3.2 Closed by the gold-standard real-data oracle
  `test_clustering_oracle_on_real_95db` (`#[ignore]`, in
  `speaker/commands.rs`): builds 350 real nemo_titanet chunks from
  meeting-95db's audio via the production `build_chunks` path and asserts
  `cluster_by_centroids` labels equal `cluster_by_centroids_naive` at
  thresholds {0.30, 0.40, 0.50, 0.65} — 0 mismatches at every threshold
  (cached ~1 s vs naive ~42 s, ~42× speedup). Stronger than the
  originally-deferred manual QA: instead of eyeballing one long meeting, it
  pins byte-exact algorithm equivalence on real production embeddings across
  the threshold range. The log-line and bounded-M guarantees are already
  covered by the §1 cargo tests; the remaining doubt was algorithm
  equivalence on real (non-synthetic) embeddings, which this closes. Run:
  `cargo test -p meetily-flash --features vulkan -- --ignored test_clustering_oracle_on_real_95db`
- [x] 3.3 No change-specific `e2e/smoke/diarization-clustering-perf.spec.ts` — the perf
  change introduces no new UI behavior. The `diarization-complete` → speaker-badge wiring
  is already covered by `e2e/smoke/speaker-diarization.spec.ts` (tests 15.2/15.3/15.6);
  clustering correctness is the cargo oracle (§1.1).

## 4. Self-review (Agent tool unavailable — HTTP 529 outage persists)

**The one change from the proposal as written: D1 is simplified from a max-heap
to a cached matrix scan.** Rationale: the heap's O(n² log n) advantage is
unnecessary once D2 caps n at 600 (the matrix scan is sub-second there), and
the matrix scan is trivially correctness-equivalent to the naive oracle (same
scan order, same predicate, same tie-break) while the heap would require custom
`Ord` + tie-breaking + stale-entry logic. Full analysis in design.md Decision 5.

**Correctness — 0 findings.**
- C1 — Problem diagnosis verified against the code: `cluster_by_centroids` at
  `sherpa_adapter.rs:462`, the per-merge `alive_indices` rescan with fresh
  `cosine_similarity` calls. The O(n³·d) estimate matches.
- C2 — The cached-matrix scan preserves merge-decision identity. Same loop
  structure, same `>` predicate, same iteration order → argmax pair is
  identical each iteration → merge sequence is identical → labels+centroids
  are identical. The oracle property test (1.1) is the binding proof and it
  passes across the full grid.
- C3 — Selective row+column recompute is sufficient: only `centroids[a]`
  changes on a merge, so only `sim(a, x)` needs recomputation. Pairs not
  involving `a` stay cached.
- C4 — D2 chunk cap is a safe coarsening: only affects segments > MAX_CHUNK_SECS
  (long monologues); speaker boundaries at transcript-segment edges unaffected.

**Security — 0 findings.**
- S1 — The n² matrix is bounded by D2's cap (≤ 600 in production), so a hostile
  oversized input cannot trigger unbounded allocation.
- S2 — Non-finite embeddings rejected upstream by `is_effectively_silent` / the
  extractor's finite guard; task 1.7 pins the boundary.

**Spec compliance — 0 findings.**
- SC1 — MODIFIED requirement gains the chunk-cap clause + bounded-complexity /
  non-blocking clause. Merge threshold, max_speakers enforcement, model, and
  centroid storage unchanged.
- SC2 — No scope creep: no DB migration, no frontend, no model change, no new
  dependency. ports/ extraction correctly deferred to `hexagonal-port-traits`.

**Shark-tank — survived, 0 action-items.**
- "Why not the heap?" → marginal perf gain at n ≤ 600; not worth the correctness
  complexity. KISS (§1.7).
- "Why not SIMD?" → the O(n²) algorithmic fix + D2 cap removes the need; SIMD is
  platform-fragile. Revisit only if profiling shows cosine sim still dominates.
- "Is the oracle test strong enough?" → it covers 6 n-values × 3 thresholds × 4
  geometries = 72 cases, including the degenerate tie-prone geometries
  (all-identical, all-orthogonal). The labels match exactly and centroids within
  1e-5. This is the proof the optimization is behaviour-free.
- "spawn_blocking overhead?" → one thread spawn per diarization run (not per
  segment); negligible vs. the seconds-long clustering.

**Branch note.** Committed on `enhance/notification-actions` alongside the prior
four changes because the 7-step session goal runs them sequentially against one
working tree. Cherry-pick / split at merge time.
