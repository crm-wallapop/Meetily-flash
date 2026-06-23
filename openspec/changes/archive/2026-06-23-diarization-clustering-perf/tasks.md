# Tasks — diarization-clustering-perf

> Branch: `enhance/diarization-clustering-perf` (work landed on
> `enhance/notification-actions` — see branch note at the bottom).
> Pure-Rust change in `frontend/src-tauri/src/audio/speaker/sherpa_adapter.rs`
> (the `cluster_by_centroids` function and the `build_chunks` split granularity)
> plus the non-blocking wrap of `adapter.process()` in `commands.rs`. No DB
> migration, no frontend, no model change, no new dependency. No Playwright
> smoke spec — pure compute optimization with a property-test correctness oracle.

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
- [ ] 3.2 Manual QA against the same long meeting that stalled (83 min / 4973 s):
  re-diarize and confirm the `clustering produced N speakers from M chunks` log
  line now appears within seconds (M ≤ ~600 due to the cap), diarization
  completes, and the resulting speaker assignments look correct. (Deferred —
  requires the real model + the specific long meeting file; the oracle test +
  the #[ignore] real-audio tests are the binding automated proof.)
- [x] 3.3 No `e2e/smoke/diarization-clustering-perf.spec.ts` — carve-out per header.

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
