## Context

The diarization emits speaker boundaries only where the uniform chunk grid (`SPLIT_TARGET_SECS = 3.0`, `FINE_SPLIT_SECS = 2.0`) changes AHC label. Rapid back-and-forth within one grid cell is collapsed into a single run. On `meeting-cde5c264-…`, the fine diarization for the 5.7–32.5 s window contains a single boundary at 21.36 s despite the transcript reading as multi-turn dialogue.

The boundary source is unused. pyannote-segmentation-3.0 provides ~16.9 ms-resolution change-points (verified empirically: 36–42 boundaries in the 5.7–32.5 s banter window vs 1 from the chunk grid; pyannote-default smoothing yields 24 turns and hits both known anchors — see the exploration doc). The question is *how* to deliver those boundaries to Meetily's existing nemo_titanet + AHC + cap + temporal-coherence + `refine_pass2` + registry pipeline without regressing it.

## The runtime constraint (load-bearing — verified by adversarial panel + two probes)

sherpa-onnx-sys 1.13.4 statically bundles ONNX Runtime 1.17.1 (C-API ≤17). The project's existing `ort = "2.0.0-rc.10"` dep (Cargo.toml:113, used for Parakeet transcription) brings C-API 27. The two runtimes **cannot coexist in one process**: the moment sherpa's ORT initializes in a process that has linked `ort`, the C-API 27 DLL collides with sherpa's bundled C-API 17 on the global symbol table and STATUS_ACCESS_VIOLATIONs. This was confirmed twice:

1. **Crux probe (`pyannote_sherpa_load_crux`):** called sherpa's `OfflineSpeakerDiarization::create()` in a test that imports ONLY `sherpa_onnx` (no `use ort::*`). STATUS_ACCESS_VIOLATION — because `ort` is linked for Parakeet regardless of what the diarization code imports.
2. **Cosine gate (`nemo_titanet_ort_cosine_equivalence`):** the in-process gate that compared sherpa's extractor to an `ort`-port crashed identically — the gate itself was structurally impossible.

The prior design (D1–D4 built on sherpa's `OfflineSpeakerDiarization`) is **falsified by this constraint**. A three-way adversarial panel (all-ort / all-sherpa / process-boundary) converged on the answer:

- **All-sherpa** (route pyannote through sherpa, drop `ort` from diarization): FALSIFIED by the crux probe. `ort` stays linked for Parakeet transcription regardless of what the diarization path does. Formal concession logged.
- **All-ort** (port nemo_titanet embedding extraction to `ort`, remove sherpa entirely): GATED OUT. The panel's cosine-equivalence gate (`cosine(emb_sherpa, emb_port) > 0.99` on 10 clips) FAILED — max cosine 0.95 on the best clip (clean 22s monologue). Diagnostic pattern (length-scaled cosines, 2.9× norm ratio on silence) indicates a systematic preprocessing divergence in CMVN or a missing normalization sherpa applies. Pinpointing it requires instrumenting sherpa's C++ internals (sherpa-onnx-sys ships prebuilt static libs, no source), which is unbounded — exactly the multi-week effort the panel flagged as the gate's failure mode.
- **Process-boundary** (this design): the only surviving path. Process isolation is the OS primitive that resolves address-space collision. A standalone child binary linking ONLY `ort` runs pyannote; the sherpa-based main app spawns it and consumes its boundaries.

Dependency: this change assumes `diarization-speaker-split-persistence` is archived (the corrected persist path is needed for the extra N>1 splits finer boundaries produce).

## Goals / Non-Goals

**Goals:**
- Source speaker change-point boundaries from pyannote's segmentation model (via an `ort`-only child process) so rapid turns produce boundaries the chunk grid cannot.
- Preserve Meetily's label-quality machinery (sherpa nemo_titanet embeddings, AHC, cap, temporal-coherence smoothing, `refine_pass2`, registry) **bit-for-bit** — pyannote supplies boundaries only, never labels, never embeddings.
- Respect the existing `MAX_DIARIZATION_CHUNKS` perf cap without preferentially stripping turn boundaries on long meetings.
- Close the runtime conflict at the OS level (process boundary), not by porting working code (all-ort's silent-registry-corruption risk) or by removing a working dep (all-sherpa's Parakeet regression).

**Non-Goals:**
- Porting nemo_titanet to `ort` (panel-rejected: silent-drift risk, gate failed).
- Removing or downgrading sherpa-onnx (it remains the embedding + clustering source for label quality).
- A segmentation-only FFI fast path through sherpa (none exists; sherpa's `process()` always runs segmentation + embedding + clustering, and crashes on pyannote-3.0 anyway).
- Per-word alignment (handled by the companion `diarization-speaker-split-persistence` change).

## Decisions

**D1 — Pyannote runs in a standalone child binary linking only `ort`; the main app spawns it and consumes boundaries via JSON-over-stdio.**

The child binary is a thin CLI shell around the validated probe logic (Phase 2b in `pyannote_ort_probe.rs`): load pyannote-segmentation-3.0 via `ort::Session::commit_from_file`, slide a 10s window at 1s step over the 16kHz mono audio, decode per-frame powerset logits to 3-speaker multilabel activity via hysteresis at onset 0.5, apply pyannote-default smoothing (median filter rad=3, min_on=0.3s, max_off=0.5s — the only config that hit BOTH known anchors in Phase 2b), emit change-points as `Vec<(start_s, end_s)>`.

The child is invoked from `commands.rs:413-432` (the `spawn_blocking` closure) BEFORE `adapter.process()`. Its stdout is parsed into the `transcript_segments` argument that `process()` already accepts. The existing `embed-probe-ort` crate is ~80% of this binary — productionizing it means adding the IPC contract (D2), Windows lifecycle handling (D3), and packaging as a Tauri resource.

**Why process isolation is structurally correct, not a workaround:** the constraint is address-space collision between two ORT C-API runtimes. The OS process is the primitive that exists precisely to enforce address-space isolation. All-ort removes one colliding party (valid, but panel-rejected on silent-drift risk); all-sherpa removes the other (impossible — Parakeet needs `ort`). Process-boundary keeps both parties intact and isolates them at the boundary the OS provides. sherpa's verified nemo_titanet extractor is preserved bit-for-bit; `ort`'s pyannote-3.0 path (validated in isolation by Phase 1/2/2b) runs in its own address space.

**D2 — IPC contract: JSON-over-stdio, file-path audio passing, spawn-per-meeting.**

- **stdin (one line, JSON):** `{"audio_path": "...", "model_dir": "~/.meetily-models", "onset": 0.5, "median_rad": 3, "min_on_secs": 0.3, "max_off_secs": 0.5}`.
- **stdout (one JSON blob):** `{"boundaries": [{"start": 0.0, "end": 4.21}, ...], "duration_secs": 4980.0, "windows_inferred": 4963, "change_points": 120}`.
- **stderr:** human-readable progress (gated by an env var or `--verbose` flag for production).
- **exit codes:** 0 = success; 1 = invalid args; 2 = model missing; 3 = decode failure; 4 = inference failure.

**Why file-path audio, not piped samples:** the decoded samples live in the parent's memory (`commands.rs:369` `decoded.to_whisper_format()`); for an 80-min meeting that's ~307 MB of f32. Piping through stdin is dead on arrival. Passing the file path and re-decoding in the child costs ~5–10s (Symphonia on MP4/AAC) added to a ~20-min job — acceptable. The re-decode produces the same 16kHz mono buffer because both paths use the same Symphonia pipeline. (Rubato resampling is NOT needed — the production decoder already resamples to 16kHz; the child reuses that.)

**Why spawn-per-meeting, not a persistent worker:** the diarization flow runs once per meeting inside `spawn_blocking`. A persistent worker would add IPC statefulness, reconnection logic, and lifecycle bugs for zero gain on a batch job. Windows process spawn is ~50ms — noise against the ~20-min inference.

**D3 — Integration seam: replace the `transcript_segments` source at `commands.rs:414`, not `process()` internals.**

`adapter.process(&samples, DIARIZATION_SAMPLE_RATE, &transcript_segments)` takes transcript-segment boundaries as its third arg. Option 3 produces a pyannote boundary set that SUPERSEDES `transcript_segments` as the chunk-layout source. The change is: invoke the child binary before the `spawn_blocking` closure, parse its stdout into `Vec<(f64, f64)>`, pass that as the third arg. `process()`, `build_chunks`, `cluster_by_centroids`, `smooth_to_fixed_point`, the cap, `refine_pass2`, the registry — **all untouched**. This is the smallest possible blast radius: one call-site change + one new binary + one spawn+parse helper.

The fallback on child failure (non-zero exit, broken pipe, timeout) is graceful: log the failure, fall back to the existing `transcript_segments` (chunk-grid boundaries). The meeting still diarizes at coarse resolution; only the finer pyannote boundaries are lost. This makes the child a **strict enhancement**, never a regression vector — if it breaks, behavior degrades to the status quo, not to a crash.

**D4 — Pyannote labels are structurally absent; the child emits boundaries only.**

The child runs pyannote SEGMENTATION only (the `ort::Session` over pyannote-segmentation-3.0). It does not load nemo_titanet, does not cluster, does not emit speaker labels. Its output schema (`boundaries: [{start, end}]`) has no label field by construction. This is stronger than the prior design's "sherpa labels SHALL be discarded" — there is nothing to discard. Meetily's AHC + cap + smoothing + `refine_pass2` + registry remain authoritative for labeling, exactly as today.

**D5 (new) — Cap enforcement moves to the boundary layer, before `build_chunks`.**

Pyannote at pyannote-default smoothing emits ~120 change-points on cde5c264's ROI (Phase 2b). On a full 60-min meeting the candidate count can exceed `MAX_DIARIZATION_CHUNKS = 600`. The child binary SHALL shed uniformly (every k-th by position) down to a configurable cap (default: `MAX_DIARIZATION_CHUNKS`) BEFORE emitting, so the parent embeds only the capped set. Sub-`MIN_SPEECH_SECS` survivors are merged into their time-neighbor. This replaces `build_chunks`'s own `effective_split` uniform-grid subdivision (`sherpa_adapter.rs:331`) — the cap is now enforced once, at the pyannote-boundary layer. (Same logic as the prior design's D3, relocated across the process boundary.)

**Consequence:** at this setting, pyannote boundaries are dense candidate splits, most of which are within-speaker embedding variation, NOT speaker turns. Individual boundaries are not turns; turns are re-derived downstream by Meetily's AHC + temporal-coherence smoothing + the `MIN_SPEECH_SECS` floor (all unchanged). This is why uniform cap-shedding is acceptable where it would not be if each boundary were a turn.

## Alternatives (rejected, with evidence)

- **All-sherpa (route pyannote through `OfflineSpeakerDiarization`, drop `ort` from diarization):** FALSIFIED by the crux probe. The Option 2 champion's model-opset analysis was correct (pyannote IS opset 13/IR 7, standard-domain ops) but checked the wrong layer — the conflict is runtime-binary linkage, not model compatibility. `ort` is linked for Parakeet transcription regardless of what the diarization path does; the C-API 27 DLL loads at process startup and collides with sherpa's bundled C-API 17 the moment sherpa's ORT initializes. Formal panel concession logged. (See exploration doc §"Option 2 (all-sherpa): FALSIFIED".)
- **All-ort (port nemo_titanet embedding extraction to `ort`, remove sherpa):** GATED OUT. The panel's cosine-equivalence gate (`cosine(emb_sherpa, emb_port) > 0.99`) FAILED — max cosine 0.95 on the best clip. Diagnostic pattern indicates systematic preprocessing divergence (CMVN or missing normalization) that would require unbounded effort to pinpoint against sherpa's prebuilt C++ static libs. The port's silent-drift risk (AHC threshold 0.40 + registry corruption discovered months later) is unacceptable for production. (See exploration doc §"Gate result (FAILED)".)
- **Reduce `FINE_SPLIT_SECS` (2 s → finer):** Window-reduction disproven (project memory): between-speaker cosine collapses ~0.6→~0.8 at finer windows, defeating `SMOOTH_SELF_WEIGHT=0.6`. Rejected.
- **`pyannote-rs` crate (crates.io v0.1.2):** Unnecessary dependency — the standalone child binary already loads pyannote via `ort` cleanly (Phase 1). v0.1.2 carries no semver guarantees and adds a third-party supply-chain surface for no benefit.
- **Acoustic change-point detection (BIC/KL2):** Re-implements, weaker, what pyannote already does.

## Risks / Trade-offs

- **[Windows code signing / antivirus on a second binary]** A standalone `.exe` in the Tauri bundle must be re-signed every release (the project already pulls `tauri-plugin-updater`). Defender first-launch scanning may stall newly-installed sidecars for seconds. **Mitigation:** sign the child with the same cert as the main app; document the sidecar in release packaging. The Option 3 champion flagged this as the single most-likely operational sinker; it is recurring but loud (a botched signing bricks diarization loudly on that build — never silent). Test gate: ship a signed build to a clean Windows machine and confirm the child spawns.
- **[Updater-induced version skew]** An interrupted Tauri update could leave a mismatched main app (sherpa/ORT 1.17.1) and sidecar (`ort`/C-API 27). **Mitigation:** the IPC contract is one-shot JSON with no version field; the parent validates the output schema and falls back to chunk-grid boundaries on any mismatch. The child is versioned with the main app in the same release artifact.
- **[Doubled decode cost]** The parent decodes for sherpa embeddings; the child re-decodes for pyannote. ~5–10s added to a ~20-min job. **Mitigation:** acceptable; both paths use the same Symphonia pipeline so the buffers agree. Open Q1 — could be eliminated by writing the decoded 16kHz mono to a temp `.wav` and passing that path to the child, but the complexity isn't justified at the current cost.
- **[Subprocess crash propagation]** A panic in the child (model-missing, OOM, malformed audio) becomes a non-zero exit / broken pipe in the parent. **Mitigation:** the parent treats ANY child failure as "pyannote unavailable, fall back to chunk-grid boundaries." The meeting still diarizes; only finer boundaries are lost. This is the structural advantage over all-ort (whose failure mode is silent registry corruption).
- **[Long-meeting resolution loss]** Uniform shed-to-cap lowers per-region resolution on long meetings; acceptable because turns are re-derived downstream. The long-meeting adversarial test gates that resolution loss does not collapse the alternation region's turn structure.
- **[IPC schema drift]** The JSON contract between parent and child could drift across versions. **Mitigation:** the schema is tiny and versioned with the binary pair; the parent's parser is defensive (unknown fields ignored, missing fields → fallback).

## Migration Plan

Code + packaging only. No schema migration. Rollback = revert code + remove the sidecar from the Tauri resources. Existing meetings keep prior (coarse) boundaries until re-diarized. The spec delta records the pyannote-via-subprocess boundary source + the consumption requirement (replaces the prior "sherpa OfflineSpeakerDiarization" wording, which is empirically false).

## Security Model

The child binary receives an audio file path (from the parent's trusted `folder_path` resolution) and a model directory (from the parent's `~/.meetily-models` resolution). No untrusted input crosses the IPC boundary — the audio path is already trusted by the parent, and the model directory is a fixed project location. The child does not write anywhere except stdout/stderr. No new untrusted input surface vs the status quo; the parent's existing path validation (`find_audio_in_folder`) is unchanged.

## Adversarial Tests (§4)

cde5c264 boundary oracle — **two windows**: (a) the Ricardo interjection at ≈46:58 and (b) the actual complaint window 5.7–32.5 s; in each, a boundary the chunk-grid-only baseline misses is present in the child's emitted boundary set (assert the SPECIFIC boundary exists in child output and is absent in baseline, not a window-wide count delta — a count delta could coincidentally align with the 2 s grid). End-to-end persistence oracle — the pipeline WITH Part B (on top of Part A) persists strictly more speaker-split rows for the complaint window than the chunk-grid baseline. Single-speaker meeting not fragmented. **Subprocess lifecycle** — child spawn + JSON parse + clean exit on success; child crash (simulate via `--crash-test` flag in dev builds) → parent falls back to chunk-grid boundaries and logs the failure (NOT a hard error); child timeout (configurable, default 2× the cde5c264 budget) → parent kills the child and falls back. Chunk count ≤ `MAX_DIARIZATION_CHUNKS` after the child's shed (including at-cap off-by-one: exactly 600 vs 601 candidate boundaries). **Uniform-shed turn-recovery** — the load-bearing hypothesis test: a long (≥45 min) two-speaker rapid-alternation fixture sheds to the cap, and Meetily's AHC+smoothing still recovers the alternation turn structure (assert ≥ a threshold fraction of within-region turns are recovered; if this fails, the fallback is embedding-delta-weighted shedding). **adversarial audio** — noise/music that triggers many false change-points does not collapse the meeting to one speaker (AHC+smoothing rejects the noise fragments). Stress (candidate boundaries ≈10× cap) sheds without OOM. Silent/empty audio → child emits empty boundary set → parent falls back to chunk-grid (no crash). Perf gate on cde5c264 (sub-budget wall-clock for the parent's `spawn_blocking`, including the child spawn + decode + inference + IPC round-trip). **Phantom-model consumption** — swap the segmentation model in the child's model dir for a committed dummy fixture and assert a specific, deterministic behavior change (construction error or distinct segmentation output — not a flaky filesystem mutation; mark `#[ignore]` if it needs the real model). **Signed-sidecar spawn** — on a clean Windows machine with Defender, the signed child spawns without quarantine (manual release gate; document in release runbook).

> **Smoke carve-out.** Part B touches zero frontend code (Rust adapter + a new sidecar binary). Per CLAUDE.md §3 smoke is mandated for user-visible *frontend* behavior; more speaker badges are a downstream consequence but flow through Part A's already-smoke-tested persist path. No separate smoke spec for Part B.

## Open Questions

1. **Temp-file audio passing vs re-decode.** Is the ~5–10s re-decode cost acceptable on a 60-min meeting, or should the parent write the decoded 16kHz mono to a temp `.wav` and pass that path to the child? Resolve at D2 implementation; the perf gate is the decision point. Current lean: re-decode (simpler, cost is acceptable).
2. **Child binary packaging.** Ship as a Tauri resource (`resources` in `tauri.conf.json`) resolved relative to the main app binary, or as a separately-installed sidecar? Tauri resource is simpler and co-versioned; investigate at D3 implementation.
3. **Two-pass smart decimation.** Would a second pyannote pass at a meaningful threshold (for labels only) enable within-run-first decimation that preserves turn boundaries better than region-fair? Cost vs. fairness tradeoff; defer unless the region-fair adversarial test reveals unsatisfactory preservation on real long meetings.
