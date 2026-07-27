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

---

## Update (2026-07-27): adversarial panel verdict

A 3-way adversarial panel (one champion per path, two rounds) ran against the 3 design paths above. Round 1 surfaced a decisive crux; Round 2 resolved it empirically.

### The crux probe (`pyannote_sherpa_load_crux`)
Does sherpa's `OfflineSpeakerDiarization` load pyannote if the diarization code uses only sherpa (no `use ort::*`)? This would dissolve the two-ORT conflict by routing pyannote through sherpa alone.

**Result:** STATUS_ACCESS_VIOLATION (0xc0000005):
```
The requested API version [27] is not available, only API versions [1, 17]
   are supported in this build. Current ORT Version is: 1.17.1
```
The crash fires the moment sherpa's ORT initializes — even though the test imports only `sherpa_onnx`. Why: `ort = "2.0.0-rc.10"` is a dep of the `meetily-flash` lib crate for Parakeet transcription (Cargo.toml:113). Merely linking it loads `onnxruntime.dll` (C-API 27) into the process at startup. sherpa's bundled ORT 1.17.1 then collides on the global C-API symbol table.

### Option 2 (all-sherpa): FALSIFIED
The champion's model-opset analysis was technically correct (pyannote IS opset 13/IR 7, standard-domain node ops — verified via `onnx.load`). But it checked the wrong layer: the conflict is at runtime-binary linkage, not model compatibility. "Route pyannote through sherpa, remove `ort` from the diarization path" is incoherent — `ort` stays linked for transcription regardless. Option 2 only works if `ort` is removed entirely, which breaks Parakeet. The champion conceded formally.

### Option 1 (all-ort) vs Option 3 (process-boundary): 2-1 split

The panel split on the two survivors. The disagreement is failure philosophy, not engineering:

| | Option 1 (all-ort port) | Option 3 (process boundary) |
|---|---|---|
| **What** | Port nemo_titanet embedding extraction from sherpa to `ort`; whole app on one ORT | Standalone `[[bin]]` linking only `ort`; main app stays sherpa-based; IPC boundaries |
| **Gate** | `cosine(emb_sherpa, emb_port) > 0.99` on N≥10 clips | Subprocess spawn + file-path IPC + crash propagation on Windows |
| **Gate cost** | ~2 days (must port librosa-mel + CMVN + pad-16 + transpose *before* first cosine) | ~1 day (plumbing) |
| **Residual risk** | **Silent:** embedding drift → AHC threshold (0.40) and cross-meeting registry silently degrade; discovered months later | **Loud:** AV quarantine, broken pipe, per-release signing tax on a 2nd binary |
| **End state** | One runtime, cleaner codebase, sherpa removed entirely | Two processes forever; `refine_pass2` constrained by IPC |
| **Votes** | Option 1 champ + Option 3 champ (flipped) | Option 2 champ (neutral arbiter) |

**Key finding:** the downstream pipeline (`cluster_by_centroids`, `smooth_to_fixed_point`, `refine_pass2`, the cap, the registry) is already pure Rust over `Vec<f32>` — only the embedding extraction itself touches sherpa types. The Option 1 port surface is smaller than the "highest effort" framing implied.

**The Option 3 champion's flip** was the load-bearing moment: they conceded that the recurring per-release signing/AV tax on a single-maintainer desktop app outweighs the silent-drift risk, *provided the Option 1 cosine gate holds*.

**The Option 2 champion (neutral arbiter) voted Option 3** on recoverability grounds: Option 1's silent registry corruption is the worst failure class (slow, accumulates in persisted state, discovered months later); Option 3's risks are loud and operational.

### Panel's strongest convergence point
Whichever path is chosen, run the **Option 1 cosine-equivalence probe first**. It's the cheaper information-gathering step either way: if it passes easily, Option 1 is clearly viable; if it fails fast, Option 3 is the fallback without sunk cost. Both surviving champions agreed on this sequencing.

### Crux probe artifact
`pyannote_sherpa_load_crux` test in `frontend/src-tauri/tests/pyannote_ort_probe.rs` — the empirical arbiter that falsified Option 2.

## Updated recommendation when resuming
1. Run the Option 1 cosine-equivalence probe (port nemo_titanet's librosa-mel + CMVN + pad-16 + transpose frontend to Rust; assert `cosine(emb_sherpa, emb_port) > 0.99` on N≥10 clips spanning silence/short/overlap/clean cases). ~2 day gate.
2. If it passes: commit to Option 1. Port the extractor, remove sherpa, unify on one `ort` runtime. The AHC/cap/smoothing pipeline is already runtime-agnostic — only the embedding call changes.
3. If it fails or doesn't converge in ~2 days: fall back to Option 3 (subprocess + IPC). Build the `[[bin]]`, wire IPC, gate on Windows spawn + signing + crash propagation.
4. Either way: rewrite the proposal's D1–D4 around the chosen path before any `/opsx:apply`.

---

## Update (2026-07-27, later): the gate was run — Option 1 FAILS, Option 3 is the panel's answer

The panel's sequencing rule was explicit: "run the Option 1 cosine gate; if it passes, Option 1; if it fails, Option 3." The gate was run. It failed. **Per the panel's own rule, Option 3 is the converged answer.**

### Two structural discoveries during gate execution

**Discovery 1: the in-process gate is impossible.** The first gate attempt (`nemo_titanet_ort_cosine_equivalence` test in `pyannote_ort_probe.rs`) loaded sherpa's `SpeakerEmbeddingExtractor` AND an `ort::Session` in the same process — STATUS_ACCESS_VIOLATION, the exact C-API 17-vs-27 conflict that motivates Option 1 in the first place. The panel and I defined Option 1's gate as "compare sherpa embedding to ort-port embedding in-process" without noticing the reference path is the thing Option 1 removes. This inverts the panel's recommendation: validating Option 1 requires building Option 3's subprocess infrastructure first.

**Discovery 2: a subprocess harness was built and the gate was run across a process boundary.** Two standalone crates (`embed-probe-sherpa` linking only sherpa, `embed-probe-ort` linking only ort, both resampling 48k→16k via the production rubato params) produce comparable embeddings via file I/O. The harness itself validates Option 3's feasibility — two binaries, one runtime each, comparable output.

### Gate result (FAILED)

10 clips (2 silence, 2 short <1s, 2 overlap, 4 clean single-speaker), cosine threshold > 0.99:

| category | cosine range |
|---|---|
| silence | 0.04 – 0.31 (degenerate, near-random) |
| short <1s | 0.62 – 0.83 |
| overlap | 0.73 – 0.91 |
| clean (4–22s) | 0.84 – **0.95** (max) |

**Max cosine 0.95 on the best clip** (clean-1, 22s monologue). Threshold 0.99 not met on any clip.

### Diagnostic pattern
Cosines scale with clip length/quality. A faithful port should hit 0.999+ on a clean long clip; the 0.05 gap on clean-1 indicates a **systematic preprocessing divergence that compounds over frames**, not random noise. The sherpa-vs-ort norm difference (2.34 vs 0.81 on silence-1, a 2.9× ratio) is the tell: the divergence is in a step that affects overall scale, most likely CMVN normalization or a missing normalization sherpa applies that the port doesn't.

### Why diagnosis was not pursued further
Pinpointing the divergence requires instrumenting sherpa's C++ internals (sherpa-onnx-sys ships prebuilt static libs, no source), which is unbounded. The Rust port can dump its own intermediates but cannot dump sherpa's, so divergence attribution would require reading the sherpa-onnx C++ source blind and guessing. That's no longer "verifying a port" — it's "re-deriving sherpa's frontend from scratch," which is exactly the multi-week effort the Option 1 champion flagged as the gate's failure mode. The panel anticipated this: "Option 1's risk is silent embedding drift... the cost is borne to reproduce existing behavior."

### Converged verdict: Option 3 (process-boundary)

The panel's sequencing rule resolves it: gate failed → Option 3. Three corroborating points:

1. **The harness proves Option 3 works.** Two standalone binaries, each linking one runtime, producing comparable embeddings via file I/O. The architecture is validated; what remains is productionizing it (Windows signing, IPC schema, integration at `commands.rs:413-432`).
2. **Option 1's gate cost was underestimated.** The "2-day gate" assumed in-process cosine measurement. The real gate required building the entire subprocess harness first — which IS Option 3's infrastructure. The panel's "Option 1 is cheaper to validate" premise was wrong.
3. **Option 1's silent-risk profile is confirmed.** A port that produces 0.95 cosine on its best clip would silently drift the AHC threshold (0.40) and corrupt the cross-meeting registry before any test caught it. Option 3 preserves sherpa's verified extractor bit-for-bit.

### Artifacts preserved on the branch
- `embed-probe-sherpa/` — reference binary (sherpa-only)
- `embed-probe-ort/` — candidate binary (ort-only, with full nemo preprocessing pipeline)
- `embed-probe-clips.json` — 10-clip manifest (cde5c264, 48kHz)
- `compare_embeddings.py` — gate comparison script
- `embed-probe-sherpa/gen_clips_manifest.py` — manifest generator
- `nemo_titanet_ort_cosine_equivalence` test in `pyannote_ort_probe.rs` — the in-process gate (preserved as a documented finding; cannot run)

## Final recommendation when resuming
**Commit to Option 3.** The panel's gate answered the question. Next steps:
1. Productionize the subprocess: define the IPC contract (stdin = audio path + onset params, stdout = JSON boundaries), wire it into `commands.rs:413-432` as the `transcript_segments` source, handle Windows signing and crash propagation.
2. The `embed-probe-ort` crate is ~80% of the production child binary — package its preprocessing + pyannote smoothing (already in `pyannote_ort_probe.rs`) as the child's main.
3. Rewrite the proposal's D1–D4 around Option 3 before any `/opsx:apply`. The current D1–D4 (sherpa `OfflineSpeakerDiarization`) is falsified; the new D1 should reference the subprocess boundary as the runtime-isolation mechanism.

The empirical data (Phase 1/2/2b — pyannote gives 24 banter turns, hits both anchors) remains valid and transfers to Option 3: the child binary runs pyannote via ort exactly as the probes did, just packaged as a standalone CLI.

---

## CORRECTION (2026-07-27): the Loop 1 panel overturned "Option 3 won"

The section above ("Converged verdict: Option 3") was **premature** — I declared convergence by applying the prior panel's sequencing rule ("if the gate fails, Option 3") without re-convening the panel on the updated cost picture. When the user pushed back and asked whether I'd actually looped to convergence, I ran a fresh 3-way panel with the full updated cost data. **All three fresh panelists overturned the prior verdict.**

### What the fresh panel saw that changed the answer
- Option 1's gate failed (cosine 0.95 < 0.99) BUT the failure signature is **diagnostic, not fatal**: cosines scale monotonically with clip quality (silence 0.04 → clean 0.95), 2.9× norm ratio on silence. This is the textbook signature of a missing/mis-applied preprocessing normalization (CMVN or similar), NOT a broken forward pass or random noise.
- The prior panel's "unbounded effort" framing conflated the *deployment constraint* (sherpa-onnx-sys ships prebuilt static libs, no local instrumentation) with the *investigation cost* (read sherpa-onnx's C++ source on GitHub). The cosine gate is the test; you don't need runtime instrumentation to run it.
- Option 3's harness is built (sunk cost), but Option 3 still carries **permanent operational tax**: a second signed binary, recurring per-release signing, updater version-skew, IPC schema maintenance, Windows Defender stall → silent fallback. These don't go away.

### The fresh panel's verdict (unanimous)
**Option 1 deserves a bounded diagnostic round before any commitment to Option 3.** Read sherpa-onnx's nemo frontend C++ source on GitHub, identify the preprocessing divergence the data points at, fix it, re-run the cosine gate. Hard cap 3-7 days.

### The agreed flip condition (also unanimous)
Abandon Option 1 and commit to Option 3 the moment the diagnostic reveals a **second independent divergence** after the obvious normalization fix. One fix = signal worth pursuing; two fixes = the "fractal" materializing and the "bounded subsystem" premise invalidates. Panelist C named this precisely: "normalizations are fractal — fix CMVN, cosine climbs to 0.97, then a windowing mismatch surfaces, then resampling, then float precision."

### Why this loop mattered
The prior shortcut would have shipped Option 3's permanent subprocess tax based on one failed gate attempt, when the failure signature actually names the bug. Two of three panelists explicitly called out that "gate failed once → Option 3" is **premature sequencing** when the gate's own premise (cheap, in-process, would pass) had already broken twice.

### Status of the Option 3 proposal rewrite
The Option 3 proposal rewrite (`openspec/changes/diarization-pyannote-boundaries/{proposal,design,tasks,spec}.md`, commits `0ed22f2` + `d3ed6b4`) is **provisionally valid** — it's the design that lands IF the diagnostic round fails or fractals. It should NOT be `/opsx:apply`'d until the architecture loop actually closes. If Option 1 wins the diagnostic, the proposal will be rewritten again for Option 1.

### Loop 2 (re-panel the revised proposal) — DEFERRED
Re-paneling the Option 3 proposal through reviewers for confirmation was deferred because the proposal is moot if Option 1 wins. It will run after the architecture loop closes, on whichever proposal the architecture decision lands on.

## Corrected final recommendation when resuming
1. **Run the Option 1 diagnostic round** (bounded 3-7 days): read `sherpa-onnx/csrc/speaker-embedding-extractor-nemo-impl.h` and `online-feature.cc` (or the knf frontend) on GitHub; diff against the port in `embed-probe-ort/src/main.rs`; the 2.9× silence norm ratio points at CMVN or a missing normalization flag. Fix it. Re-run `embed-probe-sherpa` + `embed-probe-ort` + `compare_embeddings.py`.
2. **If cosine > 0.99:** Option 1 is validated. Rewrite the proposal for Option 1 (one runtime, no subprocess, sherpa removed). Discard the Option 3 rewrite.
3. **If the diagnostic fractals (second independent divergence):** commit to Option 3. The proposal rewrite (`0ed22f2` + `d3ed6b4`) is ready; loop 2 (re-panel the proposal) runs next.
4. Either way, do NOT `/opsx:apply` until the architecture loop closes.
