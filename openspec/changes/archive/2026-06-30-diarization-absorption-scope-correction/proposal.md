## Why

The archived `2026-06-29-diarization-temporal-coherence` change synced a **"Absorbed speaker is recovered mid-meeting"** scenario into the canonical `speaker-diarization` spec (≥80% recovery, "C's cluster does not vanish"). Verification on the production meeting that motivated the change (`meeting-00000001-…`, 3 speakers, 70 min) **disproved** that claim: one speaker is still absorbed from minute ~30 onward. A read-only diagnostic **ruled out** the embedding-drift hypothesis originally suspected — the absorbed speaker's OWN late chunks are cos ≈ 0.85 to her early centroid (same-speaker range, not drift); the ≈ 0.22 figure cited earlier was a misread of the mean cosine of ALL late chunks to her centroid. The root cause is not yet determined. Regardless of cause, the label-level smoothing **structurally** cannot recover a sustained regional mis-assignment (a neighborhood vote reinforces a regional consensus), so the canonical claim must be corrected. The canonical spec currently asserts behavior the system cannot deliver, and the production regression test was asserting it on a 1-row technicality. This change corrects the spec to what is actually true and commits the characterization evidence so a future change does not re-attempt a label-level fix before establishing the cause.

## What Changes

- **Correct the canonical spec.** Under the temporal-coherence requirement, REPLACE the "Absorbed speaker is recovered mid-meeting" scenario (sustained regional recovery, ≥80%) with "Local mis-assignment is recovered when neighbors are clean" — and add an explicit out-of-scope note that sustained absorption originates at the embedding layer and is filed for a separate embedding-stability change.
- **Commit the absorption characterization.** Add `test_00000001_embedding_drift_diagnostic` (`#[ignore]`, read-only, no DB mutation) that builds real chunks on the production recording and characterizes the sustained absorption three ways: (a) the absorbed speaker's OWN late chunks are cos ≈ 0.85 to her early centroid — **ruling out** the embedding-drift hypothesis (same-speaker range; the ≈ 0.22 figure originally suspected was a misread of the mean cosine of ALL late chunks to her centroid); (b) the global AHC is faithful to its own centroids (only ~1 of the dominant speaker's 234 late chunks is nearer the absorbed speaker's centroid); (c) a sequential online-centroid-tracking prototype reproduces the absorption (the absorbed speaker's cluster keeps only ~7 late chunks), showing the obvious clustering-level alternative does not recover her either.
- **Narrow the production regression test to what is verified.** The 00000001 regression test keeps its verified flicker assertion (short-run singletons < 10% in min 30–70, measured at 1.6%). It DROPS the no-absorption assertion (passed only on a ≥1-row technicality) and the fragment assertion (which measured Whisper segment length — a different layer diarization cannot change). It adds a full label reset before re-diarization so it measures fresh output, not stale manual labels.

No runtime behavior changes — the smoothing implementation on `main` is untouched. This is a spec-truth and test-truth correction.

## Capabilities

### New Capabilities
<!-- none -->

### Modified Capabilities
- `speaker-diarization`: the temporal-coherence requirement's "Absorbed speaker is recovered mid-meeting" scenario is withdrawn and replaced with a local-recovery scenario + an out-of-scope note documenting that sustained absorption is a structural limitation of any local smoothing pass (root cause filed separately).

## Impact

- `openspec/specs/speaker-diarization/spec.md` — one scenario replaced under the temporal-coherence requirement.
- `frontend/src-tauri/src/audio/speaker/commands.rs` — one `#[ignore]` diagnostic test added; the existing 00000001 regression test narrowed to the flicker assertion (with a pre-diarization label reset).
- No adapter, port, storage, or UI changes. No migration. The smoothing constants and pipeline ordering on `main` are unchanged.
