# Design — diarization-clustering-perf

## Context

`cluster_by_centroids(chunks, threshold) -> (labels: Vec<u32>, centroids: HashMap<u32, Vec<f32>>)`
is a pure function in the speaker adapter
(`audio/speaker/sherpa_adapter.rs:462`). It is called once per diarization run
from the `Diarizing` queue phase (`commands.rs`), and its `centroids` output is
consumed downstream by `merge_short_speakers`, `enforce_max_speakers_cap`, and
the `speaker_embeddings` storage. **The output contract (per-chunk labels +
duration-weighted centroids) is fixed** and depended upon; only the internal
algorithm and its cost change.

## Decisions

### D1 — Cached similarity matrix + lazy-deletion max-heap (O(n² log n))

Maintain an upper-triangle `sim: Vec<Vec<f32>>` (n×n, ~n²/2 · 4 bytes; n=600 →
0.7 MB, n=1553 → 4.8 MB) and a binary max-heap of `(sim, i, j)` entries.

- **Init:** compute `sim(i,j)` for all `i < j` once → O(n²·d). Push every entry
  onto the heap.
- **Merge step:** pop the heap until the top is a *live* pair (both endpoints
  still `alive` AND the stored `sim` equals the matrix entry — stale entries
  are discarded). If the top's sim ≤ threshold, stop. Otherwise merge `b` into
  `a`:
  - New centroid `c` = duration-weighted average (identical rule to today).
  - For every other live cluster `x`: recompute `sim(c, x)` from the **new**
    centroid, write it back to the matrix, push `(sim, a, x)` onto the heap.
    Mark `b` dead. → O(k·d) per merge, **not** O(k²·d).

Total: O(n²·d) init + O(n·k̄·d) merges + O(n² log n) heap ops ≈ **O(n² log n)**.
For n=1553: ~5×10⁹ ops → low tens of seconds worst case; with D2's cap
(n ≤ 600) it is **sub-second to a few seconds**.

**Why this is correct, not just fast:** the centroid update rule and the
`sim > threshold` merge predicate are byte-for-byte the same as today, so the
*sequence* of merge decisions is identical. Centroid values are identical (same
weighted average). The naive code redundantly recomputed pairs whose similarity
hadn't changed; the matrix just remembers them.

### D2 — Adaptive chunk granularity, `MAX_DIARIZATION_CHUNKS` ceiling

In `build_chunks`, before splitting, compute the effective split target:

```
effective_split = max(SPLIT_TARGET_SECS, speech_seconds / MAX_DIARIZATION_CHUNKS)
```

where `speech_seconds = Σ(end_s − start_s)` over the transcript segments and
`MAX_DIARIZATION_CHUNKS = 600`. Use `effective_split` in place of
`SPLIT_TARGET_SECS` in the long-segment split loop. Net effect:

- Meetings under ~30 min speech: `speech_seconds / 600 < 3.0`, so
  `effective_split = 3.0` — **unchanged**.
- Longer meetings: granularity coarsens just enough to keep n ≤ ~600.

Coarsening only touches segments longer than `MAX_CHUNK_SECS` (10 s) — long
single-speaker monologues. Speaker *boundaries* are still resolved at the
transcript-segment level (the chunker honours segment edges; it only *splits*
over-long segments), so alignment quality is essentially unchanged. D2 is the
safeguard against pathological meeting length; D1 is the algorithmic fix that
makes even an uncapped n=1553 finish in seconds.

### D3 — Correctness oracle (the adversarial proof)

Keep the current O(n³) body, renamed `cluster_by_centroids_naive`, behind
`#[cfg(test)]`. A parametric test over a grid — n ∈ {0,1,2,5,20,80,300},
thresholds ∈ {0.30,0.40,0.55}, and synthetic geometries (well-separated,
overlapping, all-identical, all-mutually-orthogonal) — asserts the new
implementation's labels and centroids (epsilon-compared) **equal the naive
oracle**. This is the proof that the optimization is behaviour-free. The naive
oracle is O(n³) but only ever runs in tests on small n.

### D4 — Non-blocking, bounded-time requirement

The live detection-poll logs kept streaming during the 1-hour stall, which
indicates `adapter.process()` already executes off the async executor (the
queue worker appears to dispatch it on a blocking thread). This change promotes
that to a **requirement**: clustering SHALL run on a blocking thread so it can
never freeze the async runtime / UI, regardless of n. No code change is
expected here (verification only), but pinning the requirement prevents a
future refactor from regressing it into an executor-blocking call.

## Adversarial tests (CLAUDE.md §4) — mandatory before GREEN

| Category | Test |
|---|---|
| Correctness (core proof) | new AHC labels+centroids == naive oracle across the n/threshold/geometry grid |
| Oversized input | synthetic n=5000 chunks (random unit embeddings, no real audio) cluster in < 10 s; matrix ≤ ~50 MB; no OOM/panic |
| Empty / single | n=0 → empty result; n=1 → single cluster; no panic |
| Degenerate geometry | all-identical embeddings → exactly 1 cluster; all-mutually-orthogonal → zero merges, n clusters; both match the oracle |
| Chunk cap | a >30-min synthetic meeting coarsens granularity so chunks ≤ `MAX_DIARIZATION_CHUNKS`; a <30-min meeting is unaffected |
| Non-finite embeddings | a chunk whose embedding contains NaN/Inf is dropped before clustering, matching today's `is_effectively_silent` / finite guard |

## Hexagonal boundaries

Clustering is pure-Rust domain-adjacent logic currently in the adapter
(`sherpa_adapter.rs`). This change keeps it there — extracting a `ports/`
speaker-clustering trait is blocked on `hexagonal-port-traits` and is out of
scope (YAGNI). No I/O, no Tauri, no async inside the clustering function; it
remains a pure `fn(&[Chunk], f32) -> (Vec<u32>, HashMap<_, Vec<f32>>)`, which is
exactly what makes the oracle property-test possible.

## Security

No new external input crosses the boundary. Audio samples and transcript
timestamps are already validated upstream (`to_whisper_format`, segment bounds
clamped in `build_chunks`). The n² similarity matrix is bounded by
`MAX_DIARIZATION_CHUNKS` (D2), so a hostile/oversized input cannot trigger
unbounded memory allocation — the cap is a resource-exhaustion guard as well as
a perf one. Non-finite embeddings are rejected at the boundary (D3 test row)
so they cannot poison the matrix with NaN.

## Considered alternatives (rejected)

- **k-means / spectral clustering.** Rejects: changes the output contract that
  `enforce_max_speakers_cap` and `speaker_embeddings` storage depend on; AHC's
  hierarchical merge + duration-weighted centroid is the established semantics.
- **SIMD / BLAS (`wide`, `matrixmultiply`).** Rejects: the O(n²) algorithmic
  fix removes the need; SIMD is platform-fragile across the Windows
  Vulkan/CUDA/CPU build matrix and adds a native dep. Revisit only if profiling
  shows cosine sim still dominates *after* D1 (it will not — D1 cuts
  evaluations ~800×).
- **Subsample-then-assign.** Embed all chunks, cluster a random sample of ~600,
  assign the rest to the nearest centroid. Rejects: coarser granularity (D2) is
  simpler, deterministic, keeps every chunk in the clustering (no sampling
  variance), and is trivially property-testable against the oracle.
- **Raise `SPLIT_TARGET_SECS` globally** (e.g. to 6 s). Rejects: regresses
  short-meeting boundary resolution; the adaptive ceiling (D2) gives long
  meetings the coarser granularity only when they actually need it.
