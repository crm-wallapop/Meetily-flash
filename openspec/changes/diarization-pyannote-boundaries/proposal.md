> **STATUS (2026-07-26): EMPIRICAL BLOCK RESOLVED — re-converging.** Prior blockers cleared:
>
> 1. ~~Empirical block~~ — **RESOLVED.** The `pyannote_cde5c264_threshold_sweep` probe (`frontend/src-tauri/tests/pyannote_ort_probe.rs`, branch `diarize/pyannote-threshold-probe`, commit `8426e65`) ran pyannote via the project's `ort = 2.0.0-rc.10` dep on the real cde5c264 recording at onset thresholds 0.3/0.5/0.7. Results (33s wall, 19.7s inference on 165 ROI windows):
>    - **Banter 5.7–32.5s:** 1 boundary (baseline chunk grid) → **36–42 boundaries** (pyannote)
>    - **Ricardo join 17:37:** sparse → **35–43 boundaries**
>    - **Ricardo interjection 46:58:** collapsed → **22–27 boundaries**
>    - Thresholds nearly equivalent (203–210 total change-points); onset **0.5** (pyannote default) is the chosen value.
>    - Core premise confirmed: pyannote provides boundaries the chunk grid structurally cannot.
>
> 2. ~~Hard environment block~~ — **RESOLVED via unblock lever (b).** sherpa-onnx 1.13.x's bundled ORT 1.17.1 cannot load pyannote-3.0, but the project's existing `ort = 2.0.0-rc.10` dep (Cargo.toml:113) loads it cleanly via `Session::commit_from_file` + `session.run()`. No new dependency, no sherpa-onnx upgrade; pyannote runs alongside nemo_titanet+AHC+registry (preserved).
>
> **Perf gate met:** 120ms/window on CPU; full 83-min recording projects to ~10min. Acceptable for a diarization-time-only cost (does not block transcription).
>
> **Next:** re-panel to convergence (D1 needs reframing around the empirically-chosen onset 0.5 + Meetily's existing `MIN_SPEECH_SECS=1.5s` floor for coalescing sub-second noise boundaries), then `/opsx:apply`.
>
> Part A (`diarization-speaker-split-persistence`) is converged and **archived** (2026-07-25) independently.

## Why

The companion change `diarization-speaker-split-persistence` makes the per-speaker split persist correctly — but on `meeting-cde5c264-…` it yields only a **2-way split** of the 26.8 s evidence window, not the per-turn resolution the transcript text implies. The diarization emits boundaries only where the uniform chunk grid (`SPLIT_TARGET_SECS = 3.0`, `FINE_SPLIT_SECS = 2.0`) changes AHC label, so rapid back-and-forth *within* one grid cell is collapsed into a single run. Storage persistence is necessary but not sufficient; the complaint is not resolved until there are more boundaries.

The boundary source already exists, unused. `pyannote-segmentation.onnx` is a **phantom dependency**: `sherpa_adapter.rs:103-106` accepts `segmentation_model_path`, checks the file exists, and never passes it to any sherpa-onnx config — the only ONNX object built is a `SpeakerEmbeddingExtractor` (nemo_titanet). sherpa-onnx 1.13.2 (pinned `Cargo.toml:141`) ships a complete pyannote-backed `offline_speaker_diarization` module (`sherpa-onnx-1.13.2/src/offline_speaker_diarization.rs`) whose segmentation model operates at ~100 ms frame resolution — far finer than the 2–3 s chunk grid. The canonical `post-meeting-pipeline` spec even names `OfflineSpeakerDiarization::process(samples)` at step 3 (`spec.md:290`); the implementation doesn't conform. Wiring it up closes an existing spec/impl gap.

**Empirical confirmation (2026-07-26):** The probe ran pyannote segmentation via `ort 2.0.0-rc.10` on the real cde5c264 recording. The 5.7–32.5s banter window — which the chunk grid collapses to **1 boundary at 21.36s** — produces **36 boundaries at onset 0.5**. The 46:58 Ricardo interjection — collapsed to a single run in production — produces **24 boundaries**. The premise that pyannote provides finer boundaries than the chunk grid is empirically verified.

> **Depends on `diarization-speaker-split-persistence` being archived first.** Finer boundaries produce more N>1 splits, which need the corrected persist path. This change assumes that path is in place.

## What Changes

Wire sherpa's `OfflineSpeakerDiarization` as a **boundary pre-splitter** inside `SherpaOnnxDiarizationAdapter::process()`, before `build_chunks`. Run it on the 16 kHz mono samples (reusing the existing 48 kHz→16 kHz resample buffer), take its segment boundaries, and use them to subdivide the chunk layout so embeddings are computed on pyannote-bounded sub-segments rather than uniform 3 s windows. Then run Meetily's existing embedding + AHC + most-isolated-cluster cap + temporal-coherence smoothing + `refine_pass2` + cross-meeting registry pipeline unchanged. Remove the phantom pyannote load-or-skip from the adapter constructor — pyannote becomes genuinely consumed.

Two technical points drive the design (both surfaced by the shark-tank and verified against the sherpa-onnx source, including the official `sherpa-onnx-offline-speaker-diarization.cc`):

1. **`process()` returns post-clustering segments** (`OfflineSpeakerDiarizationSegment { start, end, speaker }`), and applies three coalescing steps (ReLabel, contiguous-run extraction, gap-merge) regardless of config. To obtain maximally fragmented candidate boundaries, `FastClusteringConfig` SHALL be `{ num_clusters: -1, threshold: 0.0 }` with `min_duration_on: 0.0` and `min_duration_off: 0.0`. The semantics are non-obvious: `threshold` is a cosine-**dissimilarity** cutoff where *smaller → more clusters* (so 0.0 fragments maximally; the design's original `1.0` would have over-merged — worse than the status quo). The output is post-ReLabel but dense candidate splits (≈100 ms), NOT raw segmentation and NOT clean turns. Meetily's own AHC + temporal-coherence smoothing re-derive real turns from these candidates and remain authoritative for labeling.
2. **The chunk cap (`MAX_DIARIZATION_CHUNKS = 600`) forces shedding on long meetings.** Because the pyannote boundaries are dense candidates (not turns), uniform shed-to-cap (every k-th boundary, before embedding) is acceptable — it lowers per-region resolution without destroying turns, since turns are recovered downstream by AHC+smoothing. Part B REPLACES `build_chunks`'s own `effective_split` uniform grid so the cap is enforced once. The load-bearing hypothesis (turns are recoverable from the shed candidate set) is gated by an adversarial test on a long alternation fixture; if it fails, the fallback is embedding-delta-weighted shedding.

**Out of scope:** a segmentation-only FFI fast path (none exists in `sherpa-onnx-sys-1.13.2`; embedding extraction runs twice — sherpa's internal pass plus Meetily's AHC — accepted cost, gated by a perf test); the third-party `pyannote-rs` crate (different embedding model breaks the nemo_titanet registry; v0.1.2 has no semver guarantees — rejected, see design).

## Capabilities

### New Capabilities
(none)

### Modified Capabilities
- `speaker-diarization`: amend the "Diarization segment granularity resolves speaker turns…" requirement so speaker change-point boundaries are sourced from the pyannote-backed `offline_speaker_diarization` pre-splitter (`FastClusteringConfig` threshold 1.0), not solely the uniform chunk grid; add a region-fair decimation requirement so the chunk cap does not preferentially strip turn boundaries; add a requirement that the pyannote segmentation model is actually consumed (closing the phantom-dependency state).

## Impact

- **Code**: `audio/speaker/sherpa_adapter.rs` (new pre-splitter step in `process()`; remove phantom constructor load-or-skip at 103-106; pin `FastClusteringConfig`); region-fair decimation when the chunk cap is exceeded. `Cargo.toml` needs no change — `sherpa-onnx = "1.13"` (caret) already resolves to 1.13.2 and exposes the module (note: the caret also permits 1.13.3, already in the registry; this change uses only the stable `offline_speaker_diarization` API surface).
- **Performance**: embedding extraction runs twice (sherpa's internal pass + Meetily's AHC on the finer chunks). Bounded by a perf gate; the cached-AHC cost bound (already shipped) holds for the clustering half.
- **Spec**: one delta against `speaker-diarization`.
- **Tests**: §4 adversarial — cde5c264 boundary oracle (the Ricardo interjection at ≈46:58, the actual complaint, yields a boundary the chunk-grid baseline misses); single-speaker meeting not fragmented; no sherpa-label string reaches `speaker_embeddings`; chunk count ≤ `MAX_DIARIZATION_CHUNKS`; region-fair decimation on a long two-speaker rapid-alternation fixture (alternation region not stripped more than a monologue region); silent/empty audio (no crash); perf gate on cde5c264; phantom-model consumption (swap the model file for a dummy and assert behavior changes).
