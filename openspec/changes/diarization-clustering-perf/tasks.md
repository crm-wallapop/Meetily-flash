# Tasks — diarization-clustering-perf

> Branch: `enhance/diarization-clustering-perf`.
> Pure-Rust change in `frontend/src-tauri/src/audio/speaker/sherpa_adapter.rs`
> (the `cluster_by_centroids` function and the `build_chunks` split granularity).
> No DB migration, no frontend, no model change, no new dependency. No Playwright
> smoke spec — this is a pure compute optimization with a property-test
> correctness oracle; UI behaviour is unchanged and the binding proof is the
> cargo adversarial tests + a manual long-meeting run (carve-out consistent with
> the other speaker changes).

## 1. RED — adversarial clustering tests (must fail on current code or pin its behaviour)

- [ ] 1.1 **Behaviour-identity oracle (the core proof):** keep the current `cluster_by_centroids` body as `cluster_by_centroids_naive` behind `#[cfg(test)]`. Write a parametric test over n ∈ {0,1,2,5,20,80,300}, thresholds ∈ {0.30,0.40,0.55}, and synthetic cluster geometries (well-separated, overlapping, all-identical, all-mutually-orthogonal), asserting the NEW implementation's labels and centroids (epsilon-compared) equal the naive oracle. (Fails today: the new impl does not exist yet.)
- [ ] 1.2 **Oversized input is bounded:** a synthetic 5000-chunk input (random unit-norm embeddings, no real audio) clusters in < 10 s and the n² similarity matrix stays under ~50 MB; no OOM, no panic.
- [ ] 1.3 **Empty / single chunk:** n=0 → empty labels + empty centroids; n=1 → single cluster labelled 0; no panic.
- [ ] 1.4 **Degenerate geometries:** all-identical embeddings → exactly 1 cluster; all-mutually-orthogonal → zero merges, n clusters out; both match the naive oracle.
- [ ] 1.5 **Chunk cap coarsens long meetings:** given synthetic transcript segments totalling more than `MAX_DIARIZATION_CHUNKS × SPLIT_TARGET_SECS` seconds of speech, `build_chunks` produces ≤ `MAX_DIARIZATION_CHUNKS` chunks and the effective granularity equals `speech_seconds / MAX_DIARIZATION_CHUNKS`.
- [ ] 1.6 **Short meetings unaffected:** given segments totalling less than `MAX_DIARIZATION_CHUNKS × SPLIT_TARGET_SECS` seconds, `build_chunks` uses `SPLIT_TARGET_SECS` (3.0) — identical chunk count and boundaries to today.
- [ ] 1.7 **Non-finite embeddings dropped:** a chunk embedding containing NaN/Inf is excluded before clustering (matches today's `is_effectively_silent` / finite-guard path), so it cannot corrupt the similarity matrix.

## 2. GREEN — implement the optimization

- [ ] 2.1 In `cluster_by_centroids`, replace the per-merge full pairwise rescan with: an upper-triangle cached `sim` matrix + a lazy-deletion max-heap of `(sim, i, j)`. On a merge, recompute `sim(new_cluster, x)` for each surviving cluster `x` only, write it back to the matrix, push the new entry, mark the merged-away cluster dead; pop stale heap entries (either side dead, or stored sim ≠ matrix sim) until a live pair with sim `> threshold` is found or the heap is empty. Keep the identical duration-weighted centroid update rule and the `> threshold` predicate.
- [ ] 2.2 Rename the previous implementation `cluster_by_centroids_naive` and gate it behind `#[cfg(test)]` as the oracle for task 1.1.
- [ ] 2.3 Add `const MAX_DIARIZATION_CHUNKS: usize = 600;` and compute `effective_split = max(SPLIT_TARGET_SECS, speech_seconds / MAX_DIARIZATION_CHUNKS)` in `build_chunks`; use `effective_split` in the long-segment split loop. Keep `MIN_SPEECH_SECS` / `MAX_CHUNK_SECS` bounds unchanged.
- [ ] 2.4 Verify (do not regress) that `adapter.process()` runs clustering off the async executor — trace the call from the `Diarizing` queue worker to confirm it is on a blocking thread; if it is not, wrap the clustering in `spawn_blocking`. Add a short why-comment noting the non-blocking requirement so a future refactor cannot regress it.
- [ ] 2.5 `cargo test -p app_lib audio::speaker::sherpa_adapter` green — the §1 RED tests pass and the oracle property test proves behaviour-identity.

## 3. Verify

- [ ] 3.1 `cargo test` (full Tauri crate) green. (Requires the sherpa-onnx prebuilt lib not to be file-locked by a running dev app — close the app first, or the build fails with Windows OS error 32.)
- [ ] 3.2 Manual QA against the SAME long meeting that stalled (83 min / 4973 s): re-diarize and confirm the `clustering produced N speakers from M chunks` log line now appears within seconds (M ≤ ~600 due to the cap), diarization completes, and the resulting speaker assignments look correct. Compare total wall-clock to the prior ~1-hour stall.
- [ ] 3.3 No `e2e/smoke/diarization-clustering-perf.spec.ts` — see the carve-out in the header (cargo adversarial tests + a manual long-meeting run are the binding proof; a Playwright spec cannot assert a GPU clustering timing without being heavy and flaky).
