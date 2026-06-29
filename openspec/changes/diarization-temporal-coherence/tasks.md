# Tasks

> **Apply notes (2026-06-29):** all tasks green. `cargo test --lib` = 459 passed /
> 0 failed / 13 ignored (445 baseline + 14 new). Two implementation deviations
> from the original design, both recorded below: **(A) damped self-vote** (D2) —
> the design's "exclude self (j≠i)" was changed to "include self at weight 0.5";
> excluding self erased genuine short interjections between two different speakers
> (the surrounding speakers outvoted the interjection's excluded voice), which
> would have reintroduced the user's "merged turns" complaint. Damped self keeps
> absorption recovery (a contaminated chunk's self-fit to its WRONG centroid is
> low, so unanimous neighbours still win) while anchoring genuine interjections
> (split neighbour votes can't beat the self-vote by the margin). **(B) D3 floor
> simplified** — the "OR acoustic-margin merge" branch was dropped because the
> damped-self vote's margin gate subsumes it (a mis-split short run between two
> speakers is absorbed by the vote before the floor runs). See design.md amendment.

## 1. Temporal-coherence smoothing function (pure, testable)

- [x] 1.1 **(red→green)** `smooth_flicker_islands_collapse` — `[0,1,0,1,…]` collapses via the D3 floor.
- [x] 1.2 **(red→green)** `smooth_contamination_seed_absorbed_by_neighbours` — t=0 spurious cluster absorbed.
- [x] 1.3 **(red→green)** `smooth_sustained_absorption_recovered` — ≥80 % of a 20-chunk absorbed run recovered.
- [x] 1.4 **(red→green)** `smooth_real_turn_between_different_speakers_preserved` — substantial turn (≥ floor) preserved.
- [x] 1.4b **(added)** `smooth_short_interjection_between_different_speakers_preserved` — sub-floor interjection between two different speakers preserved (damped self; would be erased under j≠i).
- [x] 1.5 **(red→green)** `smooth_nan_embedding_neighbour_skipped` — NaN/Inf neighbour skipped.
- [x] 1.6 **(red→green)** `smooth_nan_timestamp_chunk_excluded_from_windows` — NaN/Inf timestamp excluded, no panic.
- [x] 1.7 **(green)** `smooth_labels_temporal` implemented: damped-self neighbourhood vote (D2, amended), confidence-margin gate, flicker-island floor (D3, simplified), NaN/Inf clamps, deterministic winner (ties → smallest label).
- [x] 1.8 **(green, property)** `proptest_smoothing_invariants` — (a) cluster count ≤ input, (c) output length == input, no new labels invented. (b)/(d) covered by `smooth_clean_meeting_is_noop` + `smooth_real_turn…_preserved`.

## 2. Centroid recomputation

- [x] 2.1 **(red→green)** `recompute_centroids_reflects_cleaned_labels`.
- [x] 2.2 **(red→green)** `recompute_centroids_drops_zero_duration_cluster`.
- [x] 2.3 **(green)** `recompute_centroids_from_labels` — duration-weighted, drops zero-duration phantoms.

## 3. Fixed-point iteration (design D2 step 5, D7)

- [x] 3.1 **(deviation)** `smooth_fixed_point_recovers_absorption` asserts 2-iter recovers ≥80 % AND is non-inferior to a single pass. The strict "survives one pass, resolves after two" case was not constructible deterministically: with the damped-self vote, unanimous-neighbour absorbed runs recover in a SINGLE pass (the contaminated centroid's self-fit is already low). This matches design D7's own caveat ("if 1 iteration recovers ≥80 % equally well, a follow-up may reduce the cap") — the test confirms 1-iter already suffices on the adversarial absorption case; the 2-iter cap ships as defensive default.
- [x] 3.2 **(green)** `smooth_fixed_point_terminates_and_never_grows_clusters` — terminates (max_iters cap), cluster count ≤ input, returned centroids match returned labels.

## 4. Wire into the diarization pipeline (corrected insertion point)

- [x] 4.1 **(deviation — skipped, not needed)** `cluster_by_centroids` was NOT refactored to return embeddings. `Chunk` is `pub(crate)` with a `pub` `embedding` field (`sherpa_adapter.rs:146`), so `process()` reads `chunks.iter().map(|c| c.embedding.clone())` directly. Lower risk (cluster_by_centroids untouched → its cached-similarity property test passes unchanged), same data. No caller needs the embeddings vec beyond the smoothing pass.
- [x] 4.2 **(green)** smoothing wired inside `process()` immediately after `cluster_by_centroids`, before segment coalescing; shadows `labels` + `cluster_centroids` with the smoothed/recomputed values. `enforce_max_speakers_cap` (commands.rs) therefore runs on de-contaminated centroids.
- [x] 4.3 stored centroids are post-smoothing by construction: the shadowed `cluster_centroids` (= `smooth_to_fixed_point`'s recomputed output) flows through the renumber rekey + `merge_short_speakers` + return path. Verified by code inspection of the shadowing; a full `process()`-path assertion needs the sherpa model + audio (task 5.3 territory).

## 5. Scale + regression

- [x] 5.1 **(red→green)** `smooth_scales_sub_second_on_600_chunks` — n=600, dim=192, < 1 s wall-clock.
- [x] 5.2 existing diarization tests pass (459 lib tests, 0 regressions).
- [x] 5.3 **(verify, `#[ignore]`)** `smooth_verifies_prod_meeting_95db` — recipe-documented stub (re-diarize meeting-00000001-… against read-only prod DB; assert Carol labels in min 30–70, flicker < 10 %, no 5 s fragments). Manual gate; not hermetically runnable.

## 6. Spec update + archive gate

- [x] 6.1 Delta spec amended: D2 formula now includes the damped self term; D3 floor's "OR acoustic-margin" branch dropped (subsumed by the vote). Canonical `specs/speaker-diarization/spec.md` will absorb the delta at `/opsx:archive`.
- [ ] 6.2 Re-read `specs/speaker-diarization/spec.md` + `design.md` at `/opsx:archive`; amend if further drift. (design.md D2/D3 amendment done this session.)
- [ ] 6.3 Run the full merge gate before merge: `cargo test && pytest && pnpm test && pnpm lint`. **Smoke NOT required** (backend-only; no IPC/component/interaction-surface change).
