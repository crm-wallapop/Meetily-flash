## 1. Pass 2 performance gate

- [ ] 1.1 Add an `#[ignore]` real-audio test that runs the full cde5c264 meeting through a prototype two-pass `process()` and records Pass 2 wall-clock + chunk count. Confirms the design's compute budget before scaling to all meetings.
- [ ] 1.2 If 1.1 measures > 60s for the 70-min meeting, scope an 8kHz-downsampled Pass 2 path and amend design.md before proceeding to §3.

## 2. Token-timestamp wiring (design D4)

- [ ] 2.1 Red: a test asserting `transcripts.token_timestamps` is non-NULL after a Whisper transcription round-trip (currently always NULL). Adversarial: empty/garbled token output falls back gracefully.
- [ ] 2.2 Wire `extract_token_timestamps` into the Whisper result→DB save path; add the `token_timestamps` column to the INSERTs in `import.rs`, `retranscription.rs`, and `transcript.rs`. Make 2.1 green.
- [ ] 2.3 Red: the existing "Multi-speaker Whisper segment split at boundary" spec scenario, exercised end-to-end now that token data exists (previously dead path). Adversarial: oversized 500 kB transcript chunk; prompt-injection text in transcript.
- [ ] 2.4 Confirm `align_with_tokens` is reached in production (Rust log) and the proportional fallback still triggers when tokens are absent.

## 3. Two-pass coarse→fine re-labeling (design D1–D3)

- [ ] 3.1 Red: a unit test where a coarse run spanning a 2s second-speaker interjection is split after Pass 2 (the synthetic analog of `[46:58]`). Assert the interjection chunk takes the second speaker's label.
- [ ] 3.2 Implement Pass 2 in `sherpa_adapter.rs::process()`: after the existing cluster+smooth, re-chunk at `FINE_SPLIT_SECS=2.0`, assign each fine chunk to its nearest Pass-1 centroid (tie-break by temporal predecessor, D3), then re-run smoothing + coalescing on the fine labels. Make 3.1 green.
- [ ] 3.3 Red adversarial: single-speaker meeting through two-pass → exactly one speaker (no fragmentation). Silence-only chunk → skipped (None embedding). Equidistant chunk → takes predecessor label.
- [ ] 3.4 Red adversarial: oversized 4h meeting → Pass 2 chunk count bounded, no OOM, completes on the blocking thread.

## 4. Facet 2 temporal-presence constraint (design D5)

- [ ] 4.1 Red: a short chunk whose label has no temporal support in ±`PRESENCE_WINDOW_SECS` is relabeled to the dominant local speaker (synthetic `[0:01]` "Hello" before the speaker joins).
- [ ] 4.2 Implement the post-smoothing orphan scan (`MIN_PRESENCE_SECS`, `PRESENCE_WINDOW_SECS`) in `process()`. Make 4.1 green.
- [ ] 4.3 Red adversarial: a short run between two *different* speakers is preserved (not collapsed) — genuine interjection.

## 5. Real-audio oracle + integration

- [ ] 5.1 Add/extend the `#[ignore]` cde5c264 oracle: after two-pass, the `[46:58]` region contains a non-Cynthia (Ricardo) segment, and `[0:01]` is not labeled as the late-joining speaker. Real-audio regression guard (the export test already guards no-absorption; this guards label granularity).
- [ ] 5.2 Verify the alignment layer receives fine-grained segments and produces per-word splits on the cde5c264 late region (manual QA against the recording, per `feedback_verify_with_existing_data`).

## 6. Smoke test (UI-affecting)

- [ ] 6.1 Add `frontend/e2e/smoke/diarization-label-quality.spec.ts` asserting the transcript view renders per-speaker segment splits after a rediarize (event-bus mock → UI wiring, per `feedback_smoke_carveout`). Required deliverable per CLAUDE.md §3 for UI-affecting changes.
