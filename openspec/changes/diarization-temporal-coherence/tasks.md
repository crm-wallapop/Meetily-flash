# Tasks

## 1. Temporal-coherence smoothing function (pure, testable)

- [ ] 1.1 **(red)** `smooth_labels_temporal`: a flickering input
  (`[0,1,0,1,0,1,0,1,0]` with embeddings where all chunks are acoustically identical) →
  all-same-label output. Fails: function doesn't exist.
- [ ] 1.2 **(red → adversarial: contamination seed)** input where chunk 0 births a
  spurious cluster under naive assignment but its ±W temporal neighbors are all cluster 0
  → smoothing reassigns chunk 0 to cluster 0.
- [ ] 1.3 **(red → adversarial: sustained absorption)** a 20-chunk run mis-assigned to
  cluster 2 (centroid drift) whose NEIGHBORS' embeddings are acoustically closer to
  cluster 1's centroid (defined from a clean early run) → ≥ 80 % of the run recovered to
  cluster 1. (Proves the corrected formula uses neighbor embeddings e_j, not the chunk's
  own e_i — the original formula was a no-op nearest-centroid.)
- [ ] 1.4 **(red → adversarial: real turn preserved)** a genuine speaker change with
  strong acoustic shift and duration ≥ `MIN_SMOOTH_SEGMENT_SECS` is NOT smoothed away —
  including a short interjection sandwiched between two DIFFERENT speakers (the D3
  acoustic guard must not merge it).
- [ ] 1.5 **(red → adversarial: degenerate embedding)** NaN/Inf embedding in the window
  contributes 0.0 to the vote (clamped); outcome determined by finite neighbors.
- [ ] 1.6 **(red → adversarial: degenerate timestamp)** NaN/Inf timestamp in the chunk
  array excludes that chunk from the window rather than corrupting the temporal sort or
  panicking.
- [ ] 1.7 **(green)** Implement `smooth_labels_temporal` in `sherpa_adapter.rs` with
  neighborhood-voted re-assignment using NEIGHBOR embeddings (design D2, corrected
  formula), the confidence-margin gate (no-flip-on-clean-input), the acoustic-guard
  minimum-duration floor (D3), and the NaN/Inf clamps. Tests 1.1–1.6 pass.
- [ ] 1.8 **(green, property)** proptest: (a) smoothed output cluster count ≤ input;
  (b) well-separated speakers (centroid cosine < 0.3) whose runs ≥
  `MIN_SMOOTH_SEGMENT_SECS` are never merged by smoothing; (c) output length == input
  length; (d) on a clean, high-confidence input, the output differs on at most a
  negligible fraction of chunks (the no-regression guarantee).

## 2. Centroid recomputation

- [ ] 2.1 **(red)** test that centroids recomputed from smoothed labels differ from
  pre-smoothing centroids when contamination existed (a de-contaminated cluster's centroid
  shifts toward its true voice).
- [ ] 2.2 **(red → adversarial)** a cluster that lost all its chunks under smoothing is
  dropped (not preserved as a zero-duration phantom); `recompute_centroids_from_labels`
  returns only labels with ≥ 1 surviving chunk.
- [ ] 2.3 **(green)** implement `recompute_centroids_from_labels` — duration-weighted
  average over chunks with the cleaned label. Pure function.

## 3. Fixed-point iteration (design D2 step 5, D7)

- [ ] 3.1 **(red)** a contamination pattern that survives one smoothing pass but resolves
  after a centroid recompute + second pass → assert the 2-iteration cap recovers it.
- [ ] 3.2 **(green)** wire the iterate-smooth-recompute loop with a max-2-iteration cap
  (the committed default per design D7); assert it terminates and never increases cluster
  count.

## 4. Wire into the diarization pipeline (corrected insertion point)

- [ ] 4.1 **(refactor)** thread the per-chunk embedding `Vec` out of `cluster_by_centroids`
  (currently computed internally and not returned) so the smoothing pass can consume it.
  The cached-similarity property test confirms clustering output is unchanged.
- [ ] 4.2 **(green)** call `smooth_labels_temporal` + `recompute_centroids_from_labels`
  INSIDE `sherpa_adapter.rs::process()` immediately after `cluster_by_centroids` (line 199)
  and BEFORE the segment-coalescing loop (line 211). NOT in `commands.rs` — chunk-level
  data does not survive past coalescing. `enforce_max_speakers_cap` (in `commands.rs`)
  therefore runs AFTER smoothing, on de-contaminated centroids.
- [ ] 4.3 confirm the stored `speaker_embeddings` centroids are the post-smoothing values
  (assert the stored centroid equals the recomputed one, not the pre-smoothing one).

## 5. Scale + regression

- [ ] 5.1 **(red → performance)** n=600 chunks (`MAX_DIARIZATION_CHUNKS`): smoothing +
  recompute completes in < 1 s wall-clock (O(n·W·K)).
- [ ] 5.2 confirm existing diarization tests still pass (no regression for clean meetings
  where the confidence gate prevents flips).
- [ ] 5.3 **(verify, `#[ignore]`)** re-diarize `meeting-00000001-...` against prod DB +
  audio; assert: Carol labels present in min 30–70; singleton flicker < 10 %; no 5 s
  row fragments in the 30–50 min zone. (Requires the `speaker_embeddings` read path;
  document the exact re-diarize + verify recipe in the test's ignore reason.)

## 6. Spec update + archive gate

- [ ] 6.1 Update `openspec/specs/speaker-diarization/spec.md` — MODIFY the centroid-storage
  requirement ("refined by temporal-coherence pass before storage") and ADD the
  temporal-coherence requirement per this change's delta spec.
- [ ] 6.2 **Before `/opsx:archive`:** re-read `specs/speaker-diarization/spec.md` and
  `design.md`; amend if the implementation evolved during apply.
- [ ] 6.3 Run the full merge gate before declaring ready: `cargo test && pytest && pnpm
  test && pnpm lint`. **Smoke is NOT required for this change:** the change alters
  diarization output quality but not the IPC event shape, component rendering, or user
  interaction flow. Smoke tests assert UI wiring, not clustering quality; this change is
  backend-only (Rust) and adds no frontend surface.
