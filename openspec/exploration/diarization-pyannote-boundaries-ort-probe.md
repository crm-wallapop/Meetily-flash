# Exploration: pyannote-ort probe findings (2026-07-26)

**Branch:** `diarize/pyannote-threshold-probe`
**Status:** Part B (`diarization-pyannote-boundaries`) empirically unblocked, architecturally blocked. Parked pending design re-scope.
**Probe artifact:** `frontend/src-tauri/tests/pyannote_ort_probe.rs` (three `#[ignore]` tests)

## TL;DR

Part B's goal — use pyannote to produce finer speaker boundaries than the chunk grid — is **empirically validated**: pyannote + pyannote-default smoothing resolves the merged-speakers bug (24 banter turns vs 1 today, both known anchors hit). But the natural implementation ("ort-pyannote for boundaries + sherpa-nemo_titanet for embeddings + Meetily's AHC") is **architecturally impossible**: sherpa-onnx and the `ort` crate cannot coexist in one process. The proposal's D1–D4 design is built on a path that crashes; its STATUS block credits an unblock (ort-direct) the design body never actually adopts.

## What the probes proved

### Phase 1 — ORT unblock (SOLVED)
`ort = 2.0.0-rc.10` loads and runs `pyannote-segmentation-3.0.onnx` without STATUS_ACCESS_VIOLATION. ~240ms/window forward pass on CPU. No new dependency (already in `Cargo.toml:113` for Parakeet). Test: `pyannote_loads_and_runs_via_ort_rc10`.

### Phase 2 — Raw boundary sweep (cde5c264, ROI-only)
At onset 0.3/0.5/0.7 over 3 known-turn regions (~165 ROI windows, 9.9s inference):

| region | current pipeline | pyannote (raw) |
|---|---|---|
| banter 5.7–32.5s | 1 boundary @ 21.36s | 36–42 change-points |
| Ricardo join 17:37 | sparse | 35–43 |
| interjection 46:58 | collapsed | 22–27 |

Threshold-insensitive (±15% across 0.3→0.7). Test: `pyannote_cde5c264_threshold_sweep`. Report: `cde5c264_pyannote_threshold_sweep.txt`.

**Caveat:** raw output is ~1 change-point/sec with 46% sub-500ms jitter — detection, not turns.

### Phase 2b — Smoothed precision (the decisive empirical data)
Standard pyannote post-processing (median filter rad=3 + min_on=0.3s / max_off=0.5s duration gates — pyannote's own defaults):

| config | banter | join | interj | total | anchors hit (±2s) |
|---|---|---|---|---|---|
| current pipeline | **1** | — | — | — | — |
| raw (onset 0.5) | 36 | 37 | 24 | 210 | — |
| light (rad=1) | 31 | 27 | 21 | 175 | 2/2 |
| **medium (pyannote defaults)** | **24** | **16** | **19** | **120** | **2/2** |
| aggressive (rad=5) | 15 | 10 | 4 | 70 | 1/2 (interjection MISS) |

Medium config is the sweet spot: 24 banter turns (24× current resolution), both anchors hit (Ricardo join 4 hits, interjection 2 hits). Aggressive over-smooths and loses the brief interjection. Test: `pyannote_cde5c264_smoothed_precision`. Report: `cde5c264_pyannote_smoothed_precision.txt`.

## The architectural blocker (the reason Part B is parked)

### The constraint
sherpa-onnx-sys 1.13.4 (resolved from manifest `sherpa-onnx = "1.13"`) **statically bundles its own ONNX Runtime 1.17.1** (C-API ≤17). The `ort = 2.0.0-rc.10` crate brings **C-API 27**. Loading both in one process produces:

```
The requested API version [27] is not available, only API versions [1, 17]
   are supported in this build. Current ORT Version is: 1.17.1
... STATUS_ACCESS_VIOLATION (0xc0000005)
```

The two ORT C-API runtimes collide on the global symbol table. **This is not fixable in code; it's a linker/runtime-level incompatibility.**

### Why this kills the natural design
Meetily's label-quality machinery (AHC, most-isolated-cluster cap, temporal-coherence smoothing, `refine_pass2`, cross-meeting registry) is all bolted to sherpa's `SpeakerEmbeddingExtractor` (nemo_titanet). The AHC re-clustering probe (`pyannote_cde5c264_ahc_reclustering`) attempted to feed pyannote-ort boundaries into `adapter.process()` — which internally uses sherpa's extractor — and crashed on the second ORT load:

```
AHC: 238 transcript segments fetched from DB
[sherpa adapter builds, loads nemo_titanet via ORT 1.17.1]
[probe loads pyannote via ort 2.0.0-rc.10] → STATUS_ACCESS_VIOLATION
```

Phase 1/2b worked only because they loaded `ort` **without sherpa**. The moment both stacks are needed, the process dies.

### What this means for the proposal
`openspec/changes/diarization-pyannote-boundaries/`:
- The **STATUS block** (proposal.md:1-15) credits "unblock lever (b)" — ort-direct — as resolving the hard block.
- But **design.md D1, D2, D4** are written entirely around sherpa's `OfflineSpeakerDiarization::process()` — the path that crashes on model load. The credited unblock and the actual design are disconnected.
- The proposal never addresses the two-ORT conflict because it was written assuming sherpa could load pyannote. It can't.

## Viable design paths (need your decision)

1. **All-ort stack** — port nemo_titanet embedding extraction from sherpa to the `ort` crate (sherpa's `SpeakerEmbeddingExtractor` → an `ort` session). One ORT runtime, no conflict. **Cost:** touches the embedding extraction, registry, AHC, cap, smoothing — all currently bolted to sherpa's extractor API. Highest effort, cleanest result, unblocks Part B fully.
2. **All-sherpa stack** — find a pyannote ONNX export compatible with sherpa's bundled ORT 1.17.1 (e.g. pyannote-segmentation-2.0, an older revision, or a re-exported model). No `ort` dep. **Cost:** research + a load test; unknown if a compatible model exists. If yes, lowest-effort path.
3. **Process boundary** — run pyannote-ort in a separate subprocess, IPC the boundaries back to the sherpa-based pipeline. **Cost:** architecturally invasive (subprocess management, IPC schema, error handling), but decouples the runtimes without porting anything.

## What's preserved on the branch

- `frontend/src-tauri/tests/pyannote_ort_probe.rs` — three `#[ignore]` tests (Phase 1 load, Phase 2 sweep, Phase 2b smoothed precision) + the AHC probe that surfaces the conflict. All compile clean against `ort 2.0.0-rc.10`.
- Temp-dir reports from the runs (reproducible by re-running the tests).
- This doc.

## Recommendation when resuming

Don't re-convene a shark-tank on the current proposal — its design premise (sherpa loads pyannote) is empirically false. Instead:
1. Pick one of the 3 design paths above (your call — they have very different cost/risk profiles).
2. Rewrite the proposal's D1–D4 around the chosen path.
3. *Then* convene a panel on the rewritten design.

The empirical data (Phase 1/2/2b) transfers to any of the 3 paths — it answers "does pyannote give usable boundaries?" (yes) regardless of which runtime delivers them.
