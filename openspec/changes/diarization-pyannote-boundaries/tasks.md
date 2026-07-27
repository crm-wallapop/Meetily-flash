## Prerequisite

- [ ] 0.1 Confirm `diarization-speaker-split-persistence` is archived and the split-and-persist path is on `main`. This change's finer boundaries produce extra N>1 splits that depend on that path.

## 1. Child binary productionization (red→green)

The `embed-probe-ort/` crate already exists from the panel's harness (~80% of the production child). These tasks productionize it.

- [ ] 1.1 Write failing test `child_binary_emits_pyannote_boundaries` — invoke the `embed-probe-ort` binary on a committed multi-speaker fixture (or `#[ignore]` on cde5c264), parse its JSON stdout, assert (a) a non-empty `boundaries` array, (b) each boundary has `start < end`, (c) all boundaries within `[0, audio_duration]`. (Real-audio harness; `#[ignore]` if the recording is absent.)
- [ ] 1.2 Write failing test `child_binary_consumes_segmentation_model` — swap the model in the child's `model_dir` for a COMMITTED dummy fixture; assert the child's behavior changes deterministically (construction error or distinct/empty segmentation output — not a flaky live-filesystem mutation). Red test for the "pyannote segmentation model is actually consumed" requirement.
- [ ] 1.3 Write failing test `child_binary_smooths_at_pyannote_defaults` — assert the child's smoothing config is median rad=3, min_on=0.3s, max_off=0.5s at onset 0.5 (the only Phase 2b config that hit BOTH anchors). Inspect via a `--dump-config` flag or the stderr progress log.
- [ ] 1.4 Write failing test `child_binary_silent_audio_emits_empty` — silent/empty audio fixture → child emits `{"boundaries": [], ...}` and exits 0 (no crash).
- [ ] 1.5 Write failing test `child_binary_exit_codes` — assert exit 0 on success, 2 on model missing, 3 on decode failure, 4 on inference failure. (Drive each via a malformed input or `--crash-test` dev flag.)
- [ ] 1.6 Implement the production child: extend `embed-probe-ort/src/main.rs` with (a) the IPC contract (JSON-over-stdio per design D2), (b) pyannote smoothing (already present from the probe — verify the productionized version uses median rad=3 / on=0.3s / off=0.5s / onset 0.5), (c) uniform shed-to-cap (D5) before emit, (d) the exit-code contract. Make 1.1–1.5 pass.

## 2. Parent integration + fallback (red→green)

- [ ] 2.1 Write failing test `parent_invokes_child_and_uses_boundaries` — at `commands.rs:413-432`, when the child succeeds, its boundary set is passed as `transcript_segments` to `adapter.process()` (mock the child or use a fixture binary that emits a known boundary set; assert `process()` receives exactly that set).
- [ ] 2.2 Write failing test `parent_falls_back_on_child_crash` — simulate child crash (non-zero exit) via a `--crash-test` flag or a mock; assert the parent logs the failure and proceeds with the existing chunk-grid (transcript-timestamp) boundaries as `transcript_segments`. The meeting still diarizes; no panic.
- [ ] 2.3 Write failing test `parent_falls_back_on_child_timeout` — child exceeds the configurable timeout (default 2× the cde5c264 budget); parent kills the child and falls back to chunk-grid boundaries.
- [ ] 2.4 Write failing test `parent_falls_back_on_schema_mismatch` — child emits unparseable JSON or a missing required field; parent falls back (defensive parser, unknown fields ignored, missing fields → fallback).
- [ ] 2.5 Write failing test `boundary_oracle_cde5c264` — on the real cde5c264 recording, assert a SPECIFIC boundary exists in the child's emitted set AND is absent in the chunk-grid-only baseline, for TWO windows: (a) the Ricardo interjection at ≈46:58 and (b) the actual complaint window 5.7–32.5 s. Assert the specific boundary, not a window-wide count delta. (Real-audio harness; `#[ignore]` if the recording is absent.)
- [ ] 2.6 Write failing test `persistence_oracle_cde5c264` — end-to-end (Part B on top of Part A): strictly MORE speaker-split rows persisted for the complaint window than the chunk-grid baseline.
- [ ] 2.7 Write failing test `single_speaker_not_fragmented` — a single-speaker fixture yields exactly one cluster after AHC+smoothing (no spurious second cluster from the finer candidate boundaries).
- [ ] 2.8 Implement the parent spawn+parse helper at `commands.rs:413-432`: spawn child with audio path + params, parse JSON stdout into `Vec<(f64, f64)>`, fallback-to-chunk-grid on any failure. Make 2.1–2.7 pass.

## 3. Cap enforcement + shedding (red→green)

- [ ] 3.1 Write failing test `child_sheds_to_chunk_cap` — candidate-boundary counts at exactly `MAX_DIARIZATION_CHUNKS` (no shed) and at 601 (shed by one) both yield a child-emitted boundary count ≤ the cap; a stress fixture at ≈10× cap sheds without OOM.
- [ ] 3.2 Write failing test `child_uniform_shed_recovers_turns` — the LOAD-BEARING hypothesis test. A long (≥45 min, or a synthetic fixture scaled past the cap) two-speaker meeting with a rapid-alternation region and a comparable monologue region: after the child's uniform shed + parent's AHC + temporal-coherence smoothing, the alternation region recovers ≥80% of its within-region turns. If this cannot be made green with uniform shedding, the fallback is embedding-delta-weighted shedding (keep high-delta boundaries preferentially) — re-open the design at that point.
- [ ] 3.3 Write failing test `child_adversarial_audio_no_collapse` — noise/music that triggers many false change-points does not collapse the meeting to one speaker (AHC+smoothing rejects the noise fragments) nor fragment a true single-speaker meeting.
- [ ] 3.4 Write failing test `child_single_speaker_at_cap` — a long single-speaker meeting whose candidate boundaries hit the cap: AHC+smoothing re-coalesces all surviving fragments back to one speaker.
- [ ] 3.5 Implement uniform shed-to-cap in the child (D5): when the candidate-boundary count exceeds `MAX_DIARIZATION_CHUNKS`, shed every k-th boundary by position BEFORE emitting; merge sub-`MIN_SPEECH_SECS` survivors into their time-neighbor. Remove `build_chunks`'s `effective_split` uniform-grid subdivision (`sherpa_adapter.rs:331`) so the cap is enforced once. Make 3.1–3.4 pass.

## 4. Packaging + signing (release gate)

- [ ] 4.1 Add the child binary as a Tauri resource in `tauri.conf.json` (resolve path relative to the main app binary at runtime).
- [ ] 4.2 Code-sign the child with the same cert as the main app; document in the release runbook that the sidecar is part of every signed release.
- [ ] 4.3 Manual release gate: on a clean Windows machine with Defender, confirm the signed child spawns without quarantine and completes the IPC round-trip within the documented budget. (Not automatable in CI; this is the panel's most-cited operational risk.)

## 5. Performance + reconciliation

- [ ] 5.1 Write failing perf test `subprocess_cde5c264_within_budget` — the full subprocess flow (spawn + decode + inference + IPC + parent AHC + refine_pass2) on cde5c264 completes within the `process()` time budget. If over budget, investigate Open Question 1 (temp-file audio passing to eliminate the re-decode). (`#[ignore]` unless the on-disk recording is available.)
- [ ] 5.2 Run the existing diarization oracle tests on cde5c264 (label-quality, temporal-coherence, refine_pass2) with the subprocess enabled; assert no regression in label quality. (Label quality is preserved by construction — sherpa's extractor is untouched — but the test confirms end-to-end behavior.)

## 6. Pre-archive gate

- [ ] 6.1 Run `cargo test` (incl. `-- --ignored` for the cde5c264 boundary oracle and perf gate if the recording is available), `pytest backend/`, `pnpm test`, `pnpm lint` — all green. (No smoke test required: this change affects boundary placement in the diarization pass, not a new user-visible frontend behavior beyond what the companion change's smoke already covers.)
- [ ] 6.2 Re-read `openspec/specs/speaker-diarization/spec.md` + this change's `design.md`; amend the delta if implementation evolved (especially Open Questions 1–3 — re-decode vs temp-file, packaging, two-pass smart decimation).
- [ ] 6.3 `/opsx:archive` after spec/design reconciliation.
