# diarization-clustering-perf

## Why

Offline speaker diarization stalls for **~1 hour+ at 95%** on long meetings.
Confirmed from the live dev-server log (`tasks/b3xqso4fy.output`, 2026-06-22):
an 83-minute meeting (4973 s of audio, 238 transcript segments) produced
**1553 chunks**, and the pipeline logged:

```
DIARIZATION: chunked + embedded 1553 chunks from 238 segments in 97.66s
```

…then **no `clustering produced` line ever appeared** — `cluster_by_centroids`
(`audio/speaker/sherpa_adapter.rs:462`) never returned. The app stayed alive
(the detection poll lines kept streaming), so it was grinding, not crashed.

Root cause: `cluster_by_centroids` is **O(n³·d)** — every agglomerative merge
step does a *full pairwise rescan* of all alive centroids (`cosine_similarity`
recomputed fresh, nothing cached). For n=1553, d=192 (nemo_titanet):

```
Σ k²·d for k=1553→1  ≈  3.7×10¹¹ FLOPs
```

in plain Rust f32 (no SIMD, a fresh `alive_indices` Vec allocated every
iteration). At ~1×10⁸ effective FLOP/s that is **~60 minutes** — matching the
observed stall to the minute. The function is *not* an infinite loop (each
iteration kills exactly one cluster via `alive[b] = false`); it is simply
catastrophically slow on a large n. And n itself is unbounded: the chunk
granularity is fixed at `SPLIT_TARGET_SECS = 3.0`, so **n ≈ speech_seconds / 3**
grows linearly with meeting length with no ceiling.

This makes diarization effectively unusable for any meeting longer than ~30–40
minutes, which is the common case for this app.

## What Changes

1. **Incremental AHC — no full rescans.** Replace the per-merge O(k²·d) pairwise
   rescan with a cached upper-triangle similarity matrix + a lazy-deletion
   max-heap. On a merge, recompute similarity only for the **new** cluster
   against each surviving cluster (O(k·d)), and find the next best pair by
   popping stale heap entries (O(log n) amortized). Total cost drops from
   O(n³·d) to **O(n²·d + n² log n)** ≈ O(n² log n) — for n=1553 that is ~800×
   fewer similarity evaluations → seconds, not an hour. The merge *decisions*
   and resulting centroids are **identical** to today's implementation (same
   duration-weighted centroid update, same `> threshold` predicate).
2. **Bounded chunk count for long meetings.** Today `SPLIT_TARGET_SECS = 3.0`
   is fixed, so n grows without bound. Add `MAX_DIARIZATION_CHUNKS` (default
   600): the effective split granularity becomes
   `max(SPLIT_TARGET_SECS, speech_seconds / MAX_DIARIZATION_CHUNKS)`. Meetings
   under ~30 min of speech are unchanged; longer meetings get proportionally
   coarser chunks so n ≤ ~600 regardless of length. Coarsening only affects
   segments longer than `MAX_CHUNK_SECS` (long monologues within one speaker),
   so speaker-boundary resolution is essentially unaffected.
3. **Correctness oracle.** Keep the current O(n³) implementation as a
   `#[cfg(test)]` reference oracle and assert, over a property-test grid of
   inputs (varying n, cluster geometry, threshold), that the new implementation
   produces **identical labels and centroids** to the oracle. This is the
   adversarial test that proves the perf change is behaviour-free.
4. **Non-blocking + bounded-time guarantee.** The `Diarizing` phase SHALL run
   clustering off the async executor (the detection polls kept streaming during
   the stall, which suggests it already does; this promotes that to a
   requirement so a future refactor cannot regress it) and SHALL complete in
   bounded wall-clock for any meeting length.

## Affected Capabilities

- **Modified:** `speaker-diarization` — the "Transcript-timestamp-driven speaker
  diarization runs as a post-processing queue phase" requirement gains a chunk
  cap (step 3) and a bounded-complexity / non-blocking clustering clause
  (step 5). No other requirement changes; the 0.40 merge threshold,
  `max_speakers` enforcement, short-speaker merge, nemo_titanet model, and
  centroid storage are all unchanged.

## Out of Scope

- nemo_titanet model, the 0.40 merge threshold, `enforce_max_speakers_cap`, the
  short-speaker merge — all unchanged.
- Different clustering family (k-means, spectral) — rejected; AHC's
  duration-weighted centroid output is the contract the max_speakers
  enforcement and `speaker_embeddings` storage depend on.
- SIMD/BLAS acceleration — rejected; the O(n²) algorithmic fix removes the need
  and SIMD is platform-fragile across the Windows Vulkan/CUDA/CPU matrix.
- Hexagonal `ports/` extraction for the speaker adapter — blocked on
  `hexagonal-port-traits`; clustering stays in `sherpa_adapter.rs`.

## Impact

- `frontend/src-tauri/src/audio/speaker/sherpa_adapter.rs` — rewrite
  `cluster_by_centroids` (cached matrix + lazy heap); add
  `MAX_DIARIZATION_CHUNKS` + adaptive granularity in `build_chunks`; keep the
  old impl behind `#[cfg(test)]` as the oracle.
- `openspec/specs/speaker-diarization/spec.md` — MODIFIED requirement.
- No DB migration, no frontend change, no model change, no new dependency.
