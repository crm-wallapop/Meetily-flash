## 1. Pass 2 performance gate

- [ ] 1.1 Add an `#[ignore]` real-audio test that runs the full cde5c264 meeting through a prototype `refine_pass2` (coarse `process()` → `enforce_max_speakers_cap` → `refine_pass2` with the **post-cap** centroid set) and records Pass 2 wall-clock + chunk count. The prototype MUST assign to post-cap centroids (not raw AHC output) so the gate measures the configured design, not a third variant. Confirms the compute budget before scaling to all meetings.
- [ ] 1.2 If 1.1 measures > 60s for the 70-min meeting, scope an 8kHz-downsampled Pass 2 path and amend design.md before proceeding to §3.

## 2. Token-timestamp wiring (compliance fix for `whisper-model-selection` §"Whisper provider stores token timestamps in the database")

- [ ] 2.1 Red: a test asserting `transcripts.token_timestamps` is non-NULL after a Whisper transcription round-trip (currently always NULL — spec violation). Adversarial: empty/garbled token output falls back gracefully (NULL, not a crash).
- [ ] 2.2 Wire `extract_token_timestamps` into the Whisper result→DB save path; add the `token_timestamps` column to the INSERTs in `import.rs`, `retranscription.rs`, and `transcript.rs`. Make 2.1 green.
- [ ] 2.3 Red: the existing "Multi-speaker Whisper segment split at boundary" spec scenario, exercised end-to-end now that token data exists (previously dead path). Adversarial: oversized 500 kB transcript chunk; prompt-injection text in transcript.
- [ ] 2.4 Red (replaces a manual log check): a unit test asserting `align_with_tokens` is selected when `token_timestamps` is non-NULL and `align_proportional` when NULL (extract the dispatch into a testable helper if needed). Adversarial: NULL tokens → proportional fallback, no panic.

## 3. Two-pass coarse→fine re-labeling (design D1–D3)

- [ ] 3.1 Red: a unit test where a coarse run spanning a 2s second-speaker interjection is split after Pass 2 (the synthetic analog of `[46:58]`). Assert the interjection chunk takes the second speaker's label.
- [ ] 3.2 Implement `build_fine_chunks(samples, sr, fine_split_secs)`: uniform non-overlapping chunks at `FINE_SPLIT_SECS = 2.0` across the full audio, IGNORING `MAX_CHUNK_SECS` (unlike `build_chunks`, which leaves ≤10s segments as single chunks). Adversarial: silence-only chunk → skipped (None embedding); tail chunk shorter than `FINE_SPLIT_SECS` handled deterministically.
- [ ] 3.3 Implement `refine_pass2(&self, samples, sr, final_centroids) -> Result<Vec<SpeakerSegment>>` on `SherpaOnnxDiarizationAdapter`: `build_fine_chunks` → embed → nearest-final-centroid label (tie-break by temporal predecessor, D3) → smooth → coalesce → min-segment-floor. Wire it into `commands.rs` immediately after `enforce_max_speakers_cap` (`process()` → cap → `refine_pass2(post_cap_centroids)`). Make 3.1 green.
- [ ] 3.4 Red invariant: Pass 1 yields k speakers; after Pass 2 every output label is in the Pass-1 final (post-cap) label set — Pass 2 never invents a speaker (the cap is not re-run and remains satisfied). Template: existing `proptest_smoothing_invariants` (sherpa_adapter.rs:2132) lifted to the Pass-1→Pass-2 boundary.
- [ ] 3.5 Red property (duration conservation): for any valid segment list with total speech duration D, after Pass 2 the sum of fine-segment durations is in `[D − silence_skipped, D]` (silence_skipped bounded by the silence detector). Cheaper alternative if a full proptest is heavy: a unit test with two known segments asserting exact fine-chunk count and total duration.
- [ ] 3.6 Red adversarial: single-speaker meeting through two-pass → exactly one speaker (no fragmentation). Equidistant chunk → takes predecessor label (D3).
- [ ] 3.7 Red adversarial: oversized 4h meeting → Pass 2 chunk count bounded, no OOM, completes on the blocking thread.

## 4. Facet 2 temporal-presence constraint (design D5)

- [ ] 4.1 Red: a short chunk whose label has no same-label temporal support in ±`PRESENCE_WINDOW_SECS` is relabeled to the dominant local speaker (synthetic `[0:01]` "Hello" before the speaker joins).
- [ ] 4.2 Implement the orphan scan (`MIN_PRESENCE_SECS`, `PRESENCE_WINDOW_SECS`, symmetric ±W edge-clipped) as the last step of `refine_pass2`, after the min-segment-floor. Do NOT re-run `merge_short_speakers`. Make 4.1 green.
- [ ] 4.3 Red (genuine interjection preserved): a short Ricardo chunk between Cynthia (left) and Carlos (right), WITH another Ricardo segment within ±W, is NOT relabeled.
- [ ] 4.4 Red (first-turn misfire): at t=0 a short chunk labeled X with no PRIOR support (edge-clipped) but X appears at t=5s and t=10s (future support within ±W) → NOT relabeled. Pins "surrounding" to mean symmetric ±W, not backward-only.

## 5. Real-audio oracle + integration

- [ ] 5.1 Extend the `#[ignore]` cde5c264 oracle: after two-pass, the `[46:58]` region contains a segment whose speaker centroid has cos ≥ 0.85 to the validated Ricardo reference (identity, not just "non-Cynthia"); the segment spans ≥ ~10s (the spike found 18–20s); and the `[0:01]` chunk is not labeled as the late-joining speaker. Load the validated Ricardo reference from a fixture (or hardcode the vector from the spike). Real-audio regression guard.
- [ ] 5.2 Red `#[ignore]` test (replaces manual QA, per `feedback_verify_with_existing_data`): load cde5c264 audio + transcripts (with newly-populated `token_timestamps`), run `process()` → cap → `refine_pass2` → alignment, and assert per-word splits on `[46:42–47:02]` (≥1 word attributed to each of Ricardo and Cynthia across the ~47:00 boundary).

## 6. Smoke test (UI-affecting)

- [ ] 6.1 Add `frontend/e2e/smoke/diarization-label-quality.spec.ts` asserting the transcript view renders per-speaker segment splits after a rediarize (event-bus mock → UI wiring, per `feedback_smoke_carveout`). Required deliverable per CLAUDE.md §3 for UI-affecting changes.
