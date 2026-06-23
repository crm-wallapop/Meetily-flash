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

### D1 — Cached upper-triangle similarity matrix (O(n²·d) init + O(k·d) per-merge recompute)

Maintain an upper-triangle `sim: Vec<Vec<f32>>` where `sim[a][b-a-1]` holds
`cosine_similarity(centroids[a], centroids[b])` for `a < b`. Memory: ~n²/2 · 4 B
(n=600 → 0.7 MB, n=1553 → 4.8 MB).

- **Init:** compute every `sim(i,j)` for `i < j` once → O(n²·d). No heap, no
  stale entries.
- **Best-pair scan:** identical double loop to today's naive code
  (`for a in alive { for b in alive, b>a { if sim[a][b-a-1] > best_sim … } }`),
  but each lookup is **O(1)** against the cached matrix instead of an O(d)
  `cosine_similarity` recompute. The `>` predicate and the `(a,b)` iteration
  order are unchanged, so **tie-breaking is byte-for-byte identical** to the
  naive oracle — no custom `Ord`, no heap-staleness reasoning required.
- **Merge step:** merge `b` into `a` (identical duration-weighted centroid
  rule). Then recompute **only** row `a` and column `a` of the similarity
  matrix (`sim(a, x)` for each surviving `x`) — O(k·d), not O(k²·d). Pairs not
  involving `a` are unchanged and stay cached.

Total: O(n²·d) init + Σ O(k²) O(1)-lookup scans + Σ O(k·d) recomputes ≈
**O(n³/3) O(1) comparisons + O(n²·d) recomputes**. For n=600 (D2's ceiling):
~7×10⁷ comparisons → **sub-second**. For n=1553 (worst case pre-D2):
~1.2×10⁹ comparisons → **~10 s**. Both are a **~200× speedup** over the
~60-minute naive on n=1553.

**Why this is correct, not just fast:** the centroid update rule and the
`sim > threshold` merge predicate are byte-for-byte the same as today, so the
sequence of merge decisions is identical. Centroid values are identical (same
weighted average). The naive code redundantly recomputed pairs whose similarity
hadn't changed; the matrix just remembers them. Because the scan logic is
unchanged (only the per-pair cost drops from O(d) to O(1)), the new labels and
centroids are **trivially equal** to the naive oracle — no tie-breaking
reasoning needed.

**Why not the max-heap originally proposed:** a lazy-deletion binary heap of
`(sim, i, j)` would give O(n² log n) on paper, but: (a) `f32` has no `Ord`
(NaN), requiring a `to_bits()` wrapper or `OrderedFloat`; (b) to produce
*identical* merge decisions the heap's tie-break must exactly mirror the naive
double-loop's lowest-`(i,j)`-first ordering — a custom `Ord` with reversed
indices; (c) stale-entry discarding (stored sim ≠ matrix sim) adds edge-case
surface. Given D2 caps n at 600, the heap's asymptotic win is unnecessary and
the simpler matrix scan is already sub-second. KISS (CLAUDE.md §1.7) wins.

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

## Decision 5 — Round-1 self-review (0 findings, one design simplification)

The `Agent` tool is not available in this session (persistent HTTP 529 outage);
self-review fallback as with the prior four changes in this session.

**The one change from the proposal as written: D1 is simplified from a max-heap
to a cached matrix scan.** Rationale in D1 above. Summary: the heap's O(n² log n)
advantage is unnecessary once D2 caps n at 600, and the cached-matrix scan is
trivially correctness-equivalent to the naive oracle (same scan order, same
predicate) while the heap would require custom `Ord` + tie-breaking + stale-entry
logic to achieve the same guarantee. KISS.

**Correctness — 0 findings.**

- **C1 — Problem diagnosis verified.** `cluster_by_centroids` at
  `sherpa_adapter.rs:462`; the per-merge `alive_indices` rescan with fresh
  `cosine_similarity` calls at `:484-494`; the duration-weighted merge at
  `:498-513`. The O(n³·d) FLOP estimate in the proposal matches the code.
- **C2 — The cached-matrix scan preserves merge-decision identity.** The scan
  loop (`for a { for b>a { if sim > best_sim { … } } }`) is identical in
  structure and iteration order to the naive; only the per-pair cost changes
  (O(d) → O(1)). The `>` strict predicate is preserved. Therefore the argmax
  pair selected each iteration is identical ⇒ the merge sequence is identical ⇒
  labels and centroids are identical. The oracle property test (D3) is the
  binding proof.
- **C3 — Selective row recompute is sufficient.** When `b` merges into `a`,
  only `centroids[a]` changes; `centroids[x]` for all other live `x` is
  unchanged. So `sim(x,y)` for pairs not involving `a` is unchanged, and only
  `sim(a,x)` needs recomputation. The implementation recomputes row `a` and
  column `a` (the upper-triangle entries `sim[x][a-x-1]` for `x<a` and
  `sim[a][x-a-1]` for `x>a`). Correct.
- **C4 — D2 chunk cap is a safe coarsening.** `effective_split = max(3.0,
  speech_seconds/600)` only increases the split target for long meetings.
  Segments under `MAX_CHUNK_SECS` (10 s) are never split regardless, so only
  long monologues get coarser chunks. Speaker boundaries (at transcript-segment
  edges) are unaffected. The `MIN_SPEECH_SECS` / `MAX_CHUNK_SECS` bounds are
  unchanged.

**Security — 0 findings.**

- **S1 — No new external input.** Audio samples and transcript timestamps are
  validated upstream. The n² matrix is bounded by `MAX_DIARIZATION_CHUNKS`
  (D2), so a hostile oversized input cannot trigger unbounded allocation — the
  cap is a resource-exhaustion guard as well as a perf one.
- **S2 — Non-finite embeddings.** Already rejected by
  `is_effectively_silent` / the extractor's finite guard before reaching
  `cluster_by_centroids`. Task 1.7 pins this at the clustering boundary.

**Spec compliance — 0 findings.**

- **SC1 — MODIFIED requirement** ("Transcript-timestamp-driven speaker
  diarization runs as a post-processing queue phase") gains a chunk-cap clause
  (step 3) and a bounded-complexity / non-blocking clause (step 5). The merge
  threshold (0.40), `max_speakers` enforcement, nemo_titanet model, and
  centroid storage are all explicitly unchanged (Out of Scope).
- **SC2 — No scope creep.** No DB migration, no frontend, no model change, no
  new dependency. The `ports/` extraction is correctly deferred to
  `hexagonal-port-traits`.

**Conclusion.** Proceed to `/opsx:apply` with the cached-matrix D1 and the D2
chunk cap.
