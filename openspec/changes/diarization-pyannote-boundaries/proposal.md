> **STATUS (2026-07-27): ARCHITECTURE CONVERGED via adversarial panel — Option 3 (subprocess).** Prior status blocks ( claiming "RESOLVED via unblock lever (b)" ) are SUPERSEDED — that claim was empirically falsified. The current state:
>
> 1. ~~Empirical block (does pyannote provide finer boundaries than the chunk grid?)~~ — **RESOLVED.** The `pyannote_cde5c264_threshold_sweep` + `pyannote_cde5c264_smoothed_precision` probes (`frontend/src-tauri/tests/pyannote_ort_probe.rs`, branch `diarize/pyannote-threshold-probe`) ran pyannote via the project's `ort = 2.0.0-rc.10` dep on the real cde5c264 recording. Pyannote-default smoothing (median rad=3, on=0.3s, off=0.5s) gives **24 banter turns** (vs 1 at 21.36s today) and **hits both known anchors** (Ricardo join 17:37, interjection 46:58). Core premise confirmed.
>
> 2. ~~Hard environment block (sherpa's ORT 1.17.1 can't load pyannote-3.0)~~ — **DIAGNOSIS CORRECTED.** The block is NOT model-level; it's runtime-linkage-level. sherpa-onnx-sys 1.13.4 statically bundles ORT 1.17.1 (C-API ≤17); `ort 2.0.0-rc.10` (Cargo.toml:113, used for Parakeet) brings C-API 27. The two runtimes collide on the global C-API symbol table the moment both are linked into one process → STATUS_ACCESS_VIOLATION. Confirmed by the `pyannote_sherpa_load_crux` probe (sherpa crashes even with no `use ort::*` because `ort` is linked for Parakeet).
>
> 3. **Architecture decision (Option 3 — subprocess):** a 3-way adversarial panel (all-ort / all-sherpa / process-boundary, two rounds, full transcripts in the exploration doc) converged here. All-sherpa FALSIFIED (crux probe — `ort` linked for Parakeet regardless of diarization path). All-ort GATED OUT (the panel's cosine-equivalence gate `cosine(emb_sherpa, emb_port) > 0.99` FAILED — max cosine 0.95 on the best clip; the panel's pre-agreed sequencing rule was "if the gate fails, Option 3"). Process-boundary is the only surviving path; a subprocess harness (`embed-probe-sherpa/`, `embed-probe-ort/`) validates the architecture end-to-end.
>
> 4. **Perf gate (revised):** Phase 1 measured ~240ms/window on CPU; full 83-min recording projects to ~20min of pyannote inference in the child + ~5-10s re-decode + the existing AHC/refine cost in the parent. Acceptable for a diarization-time-only cost (does not block transcription). The child runs in parallel with nothing else; the parent's `spawn_blocking` closure awaits its stdout.
>
> **Next:** panel this rewritten proposal to convergence, then `/opsx:apply`.
>
> Part A (`diarization-speaker-split-persistence`) is converged and **archived** (2026-07-25) independently.

## Why

The companion change `diarization-speaker-split-persistence` makes the per-speaker split persist correctly — but on `meeting-cde5c264-…` it yields only a **2-way split** of the 26.8 s evidence window, not the per-turn resolution the transcript text implies. The diarization emits boundaries only where the uniform chunk grid (`SPLIT_TARGET_SECS = 3.0`, `FINE_SPLIT_SECS = 2.0`) changes AHC label, so rapid back-and-forth *within* one grid cell is collapsed into a single run. Storage persistence is necessary but not sufficient; the complaint is not resolved until there are more boundaries.

The boundary source already exists, empirically validated. `pyannote-segmentation.onnx` (~5.8 MB, on disk at `~/.meetily-models/`) provides ~16.9 ms-resolution change-points. Phase 2/2b probes confirmed: the 5.7–32.5 s banter window — which the chunk grid collapses to **1 boundary at 21.36 s** — produces **24 turns at pyannote-default smoothing**. The premise that pyannote provides finer boundaries than the chunk grid is empirically verified; the open question was *how to deliver those boundaries given the runtime conflict*, now resolved by the panel.

**Empirical confirmation (2026-07-26/27):** See `openspec/exploration/diarization-pyannote-boundaries-ort-probe.md` for the full probe data, panel transcripts, and the gate-result table.

> **Depends on `diarization-speaker-split-persistence` being archived first.** Finer boundaries produce more N>1 splits, which need the corrected persist path. This change assumes that path is in place.

## What Changes

Ship a standalone Rust binary (the `embed-probe-ort` crate, productionized) that links ONLY `ort` (no sherpa) and runs pyannote-segmentation-3.0 over the meeting's 16kHz mono audio. Package it as a Tauri resource, signed alongside the main app. At diarization time (`commands.rs:413-432`, inside the existing `spawn_blocking` closure), spawn the child with the audio file path + onset/smoothing params, parse its JSON stdout into a `Vec<(f64, f64)>` boundary set, and pass that set as the `transcript_segments` argument to `adapter.process()` (replacing the transcript-timestamp boundaries used today). The existing embedding (sherpa nemo_titanet) + AHC + most-isolated-cluster cap + temporal-coherence smoothing + `refine_pass2` + cross-meeting registry pipeline runs UNCHANGED on the pyannote-bounded sub-segments. The child's cap-shedding (uniform, every k-th by position before emit) replaces `build_chunks`'s own `effective_split` uniform-grid subdivision so `MAX_DIARIZATION_CHUNKS` is enforced once, at the boundary layer.

Two technical points drive the design (both surfaced by the panel and verified against the codebase):

1. **Process isolation is the structurally correct answer to address-space collision, not a workaround.** sherpa-onnx-sys bundles ORT 1.17.1; the `ort` crate brings ORT C-API 27. Both linked in one process → STATUS_ACCESS_VIOLATION (verified twice: crux probe + the structurally-impossible in-process gate). The OS process is the primitive that isolates address spaces. The child links only `ort`; the main app links sherpa (and `ort` for Parakeet, but the child's `ort` is in a separate address space). No porting, no silent-drift risk, no Parakeet regression.
2. **Failure is loud and recoverable, not silent.** The child emits boundaries only — no labels, no embeddings (D4). Meetily's AHC + smoothing + registry remain authoritative. On ANY child failure (non-zero exit, broken pipe, timeout, schema mismatch), the parent logs the failure and falls back to the existing chunk-grid boundaries — the meeting still diarizes at coarse resolution; only finer boundaries are lost. This makes the child a strict enhancement, never a regression vector. (Contrast: all-ort's failure mode is silent embedding drift that corrupts the registry before any test catches it.)

**Out of scope:** porting nemo_titanet to `ort` (panel-rejected); removing or downgrading sherpa-onnx; a segmentation-only FFI fast path through sherpa (none exists, and sherpa crashes on pyannote-3.0 anyway).

## Capabilities

### New Capabilities
(none)

### Modified Capabilities
- `speaker-diarization`: amend the "Diarization segment granularity resolves speaker turns…" requirement so speaker change-point boundaries are sourced from a pyannote-backed **subprocess** (standalone `ort`-only child binary), not the sherpa `OfflineSpeakerDiarization` path (which is empirically non-viable due to the ORT runtime conflict); add the subprocess-fallback requirement (child failure → chunk-grid boundaries, never a crash); add the region-fair decimation requirement so the chunk cap does not preferentially strip turn boundaries on long meetings; add a requirement that the pyannote segmentation model is actually consumed by the child (closing the phantom-dependency state, relocated across the process boundary).

## Impact

- **Code**: `frontend/src-tauri/src/audio/speaker/commands.rs:413-432` (new spawn+parse helper before `adapter.process()`; fallback-to-chunk-grid on child failure). `embed-probe-ort/` crate (productionize: add IPC contract, smoothing already present from the probe, package as Tauri resource). `tauri.conf.json` (resource entry + signing for the sidecar). `sherpa_adapter.rs` `build_chunks` (remove the `effective_split` uniform-grid subdivision now that the cap is enforced at the boundary layer; the pyannote boundary set replaces it). `Cargo.toml` workspace (the `embed-probe-ort` member already exists from the harness).
- **Packaging**: a second signed binary in the Tauri bundle. Recurring per-release signing tax (the panel's most-cited operational concern); mitigated by signing with the same cert as the main app and documenting in the release runbook.
- **Performance**: pyannote inference (~20min on an 83-min meeting, runs in the child) + re-decode (~5-10s, Symphonia) + IPC round-trip (~50ms spawn). Bounded by a perf gate; the cached-AHC bound (already shipped) holds for the clustering half. Open Q1 (temp-file audio passing) can eliminate the re-decode if the cost proves unacceptable.
- **Spec**: one delta against `speaker-diarization` (reworded from the prior sherpa-`OfflineSpeakerDiarization` framing, which is empirically non-viable).
- **Tests**: §4 adversarial — cde5c264 boundary oracle (the Ricardo interjection at ≈46:58 and the 5.7–32.5 s complaint window yield boundaries the chunk-grid baseline misses); end-to-end persistence oracle (strictly more speaker-split rows WITH Part B than the chunk-grid baseline); single-speaker meeting not fragmented; subprocess lifecycle (clean exit, crash → fallback, timeout → fallback); chunk count ≤ `MAX_DIARIZATION_CHUNKS` after the child's shed; region-fair decimation on a long two-speaker rapid-alternation fixture (alternation region not stripped more than a monologue region); silent/empty audio (child emits empty set, parent falls back, no crash); perf gate on cde5c264; phantom-model consumption (swap model for dummy fixture, assert deterministic behavior change); signed-sidecar spawn on a clean Windows machine (manual release gate).
