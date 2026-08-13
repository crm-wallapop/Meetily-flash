## Context

The diarization emits speaker boundaries only where the uniform chunk grid (`SPLIT_TARGET_SECS = 3.0`, `FINE_SPLIT_SECS = 2.0`) changes AHC label. Rapid back-and-forth within one grid cell is collapsed into a single run. On `meeting-cde5c264-…`, the fine diarization for the 5.7–32.5 s window contains a single boundary at 21.36 s despite the transcript reading as multi-turn dialogue.

The boundary source is empirically validated. `pyannote-segmentation.onnx` (~5.8 MB, on disk at `~/.meetily-models/`) provides ~16.9 ms-resolution change-points. Phase 2/2b probes confirmed: the 5.7–32.5 s banter window — which the chunk grid collapses to **1 boundary at 21.36 s** — produces **24 turns at pyannote-default smoothing** (median rad=3, min_on=0.3s, max_off=0.5s), and hits both known anchors (Ricardo join 17:37, interjection 46:58). See `openspec/exploration/diarization-pyannote-boundaries-ort-probe.md` for the full empirical record.

## The runtime constraint — and how this design resolves it at the root

sherpa-onnx-sys 1.13.4 statically bundles ONNX Runtime 1.17.1 (C-API ≤17). The project's `ort = "2.0.0-rc.10"` dep (Cargo.toml:113, used for Parakeet transcription AND — after this change — for pyannote + nemo_titanet) brings C-API 27. The two runtimes **cannot coexist in one process** (STATUS_ACCESS_VIOLATION on the global C-API symbol table; verified by the `pyannote_sherpa_load_crux` probe). An adversarial panel evaluated three paths (all-ort / all-sherpa / process-boundary); all-sherpa was falsified (the `ort` dep stays for Parakeet regardless), process-boundary was rejected as permanent subprocess debt. **This change takes the remaining path: port nemo_titanet embedding extraction from sherpa-onnx to the `ort` crate, remove sherpa-onnx entirely, and run the whole app on one ORT runtime.**

The port is empirically validated. The `embed-probe-ort` crate (built during the panel's diagnostic round) reproduces sherpa's nemo_titanet embeddings via a hand-rolled Rust fbank (knf-equivalent framing, preemph, Hann window, realfft, Slaney mel, log, per-feature CMVN with the double-epsilon floor). After a one-line log-floor fix (`f32::MIN_POSITIVE` → `f32::EPSILON`), cosine similarity vs sherpa's reference on production-relevant clips (clean/overlap, 4–22s, the inputs that actually reach clustering) is **0.9946–0.9989** — well within the AHC operating margin (threshold 0.40, inter-speaker variance 0.6–0.8). See the exploration doc §"ARCHITECTURE LOOP CLOSED" for the full post-fix gate table and the panel's flip-condition reasoning.

Dependency: this change assumes `diarization-speaker-split-persistence` is archived (the corrected persist path is needed for the extra N>1 splits finer boundaries produce).

## Goals / Non-Goals

**Goals:**
- Source speaker change-point boundaries from pyannote's segmentation model (running in-process via `ort`) so rapid turns produce boundaries the chunk grid cannot.
- Remove sherpa-onnx as a dependency. Replace its `SpeakerEmbeddingExtractor` with an `ort::Session` over nemo_titanet (the validated port) and its `SpeakerEmbeddingManager` with a pure-Rust in-memory cosine store. One ORT runtime for the whole app.
- Preserve Meetily's label-quality machinery (AHC, cap, temporal-coherence smoothing, `refine_pass2`, registry) — these already operate on `Vec<f32>` embeddings and are runtime-agnostic; only the extractor and the registry's backing store change.
- Respect the existing `MAX_DIARIZATION_CHUNKS` perf cap without preferentially stripping turn boundaries on long meetings.

**Non-Goals:**
- A subprocess, IPC contract, or second signed binary (the panel-rejected Option 3 path).
- Changing the AHC threshold (0.40), cap logic, or temporal smoothing — these stay tuned to the (now ported) nemo_titanet embeddings; the 0.9946–0.9989 cosine fidelity is within their operating margin.
- Per-word alignment (handled by the companion `diarization-speaker-split-persistence` change).
- Re-training or re-exporting nemo_titanet (the on-disk model is used as-is; only the extraction code changes).

## Decisions

**D1 — Port nemo_titanet embedding extraction from sherpa-onnx to the `ort` crate.**

Replace `SherpaOnnxEmbeddingAdapter` (`sherpa_adapter.rs:17-82`) and the `extractor` field of `SherpaOnnxDiarizationAdapter` (`:85-126`) with an `ort::Session`-backed extractor. The port is the validated `embed-probe-ort/src/main.rs` logic (knf-equivalent fbank + CMVN), lifted into a `NemoEmbeddingExtractor` struct in the speaker module. The I/O contract is the verified nemo_titanet model contract: input `audio_signal float32[N,80,T]` (Slaney mel-filterbank features, 80 bins, after per-feature CMVN) + `length int64[N]` (unpadded frame count) → output `embs float32[N,192]`.

The fbank pipeline (framing 400/160 samples, preemph 0.97, periodic Hann, realfft-512, power spectrum 257 bins, Slaney mel with `high_freq=-400` effective 7600, log with `f32::EPSILON` floor, per-feature CMVN with double-epsilon `variance.max(1e-5)` then `/ (sqrt+1e-5)`, pad-16, transpose `[1,T,80]→[1,80,T]`) is verified bit-faithful to sherpa/knf source. The one known residual (realfft vs kissfft at ~1e-5, ORT 27 vs 1.17.1 kernel differences) produces 0.9946–0.9989 cosine on production-relevant clips — within the AHC margin.

**D2 — Pyannote runs in-process via `ort`, interleaved with the ported nemo_titanet extractor.**

With sherpa gone, the two-ORT conflict is resolved at the root. `pyannote-segmentation-3.0` loads via a second `ort::Session` in the same process — the exact pattern the Phase 1 probe (`pyannote_ort_probe.rs:48-59`) validated. The diarization flow becomes:

1. Load pyannote boundaries: slide a 10s window at 1s step over the 16kHz mono samples, decode per-frame powerset logits to 3-speaker multilabel activity via hysteresis at onset 0.5, apply pyannote-default smoothing (median rad=3, min_on=0.3s, max_off=0.5s — the only Phase 2b config that hit BOTH anchors), emit change-points. (This is the `pyannote_cde5c264_smoothed_precision` test logic, productionized into the adapter.)
2. INTERSECT the pyannote change-points with the Whisper `transcript_segments` (`fetch_transcript_timestamps`): a pyannote change-point inside a Whisper speech region becomes an intra-region split; silence regions between Whisper segments are preserved as silence (NOT embedded). Pyannote AUGMENTS the Whisper boundaries; it does not supersede them.
3. Pass the intersected set as `transcript_segments` to `process()`. `build_chunks` sub-divides each Whisper speech region by the pyannote boundaries INSTEAD OF its own `effective_split` uniform grid (D4 removes `effective_split`).
4. Extract embeddings via the ported nemo_titanet `ort::Session` (D1), then the existing AHC + cap + temporal-coherence smoothing + `refine_pass2` + registry pipeline runs unchanged.

**D3 — Replace `SpeakerEmbeddingManager` with a pure-Rust in-memory cosine store.**

`SherpaOnnxRegistryAdapter` (`sherpa_adapter.rs:1297-1362`) wraps sherpa's `SpeakerEmbeddingManager` for `add`/`search`/`verify`. These are pure cosine operations on `Vec<f32>` — the existing `cosine_similarity` helper (`sherpa_adapter.rs:1191`) already implements the math. The registry becomes an in-memory `HashMap<String, Vec<Vec<f32>>>` + cosine search, ~40 lines. No ONNX model involved; sherpa's manager was a convenience wrapper, not a model. The on-disk registry (`speaker_embeddings` table) schema is unchanged — the stored vectors are nemo_titanet 192-dim either way.

**D4 — Pyannote boundaries replace `build_chunks`'s `effective_split` grid.**

`build_chunks` (`sherpa_adapter.rs:331`) today sub-divides each transcript segment by `effective_split = max(SPLIT_TARGET_SECS, speech_seconds / MAX_DIARIZATION_CHUNKS)`. After this change, it sub-divides by the pyannote intra-region boundaries (from D2's intersect). The `MAX_DIARIZATION_CHUNKS` cap is enforced once, at the pyannote-boundary layer: when the candidate-boundary count exceeds the cap, shed uniformly (every k-th by position) before embedding, then merge sub-`MIN_SPEECH_SECS` survivors into their time-neighbor. `effective_split` is removed. (Same logic as the prior design's D3/D5, now in-process.)

**Consequence:** at this setting, pyannote boundaries are dense candidate splits (~100 ms resolution after smoothing), most of which are within-speaker embedding variation, NOT speaker turns. Individual boundaries are not turns; turns are re-derived downstream by Meetily's AHC + temporal-coherence smoothing + the `MIN_SPEECH_SECS` floor (all unchanged). This is why uniform cap-shedding is acceptable where it would not be if each boundary were a turn.

**D5 — Pyannote labels are structurally absent.**

The pyannote `ort::Session` is over pyannote-segmentation-3.0 only (the powerset segmentation model). It emits per-frame speaker-activity; it does not produce speaker *labels*. Meetily's AHC + cap + smoothing + `refine_pass2` + registry remain authoritative for labeling, exactly as today. This is stronger than "sherpa labels discarded" (the prior design's framing): there is no labeling pass to discard.

## Alternatives (rejected, with evidence)

- **All-sherpa (route pyannote through `OfflineSpeakerDiarization`, drop `ort`):** FALSIFIED by the `pyannote_sherpa_load_crux` probe. `ort` is linked for Parakeet regardless of what the diarization path does; the C-API 27 DLL loads at process startup and collides with sherpa's bundled C-API 17. Formal panel concession.
- **Process-boundary (standalone `ort`-only child binary, IPC boundaries):** Panel-rejected as permanent subprocess debt. The panel's re-framing (Loop 1, after the diagnostic validated the port): Option 3's recurring per-release signing tax, updater version-skew, IPC schema maintenance, and Windows Defender stall → silent fallback are all permanent costs that vanish if the port is taken. The diagnostic proved the port is viable (one logic bug, now fixed), so the debt is unjustified.
- **Reduce `FINE_SPLIT_SECS` (2 s → finer):** Window-reduction disproven (project memory): between-speaker cosine collapses ~0.6→~0.8 at finer windows, defeating `SMOOTH_SELF_WEIGHT=0.6`. Rejected.
- **`pyannote-rs` crate:** Unnecessary — the in-process `ort::Session` path is validated and adds no dependency.
- **Acoustic change-point detection (BIC/KL2):** Re-implements, weaker, what pyannote already does.

## Risks / Trade-offs

- **[Embedding fidelity residual]** The port reproduces sherpa's embeddings at 0.9946–0.9989 cosine on production-relevant clips (clean/overlap ≥1.5s, non-silent). The residual is realfft-vs-kissfft rounding (~1e-5) + ORT 27-vs-1.17.1 kernel differences, irreducible and concentrated on near-zero-energy / very-short inputs that the pipeline drops before clustering. **Mitigation:** the residual is 100× below the AHC operating margin (threshold 0.40, inter-speaker variance 0.6–0.8); the revised acceptance gate (Adversarial Tests) includes an end-to-end AHC parity test that catches any regression that somehow compounds. The existing `cde5c264` oracle (label-quality, temporal-coherence, refine_pass2) is the regression net.
- **[Registry continuity]** Stored `speaker_embeddings` rows are nemo_titanet 192-dim vectors. The port produces the same dim from the same model; cross-meeting matching is unaffected at the 0.9946+ fidelity level. **Mitigation:** the registry schema is unchanged; no migration. A drift regression would surface as degraded cross-meeting matches, caught by the registry parity test.
- **[Sherpa removal blast radius]** sherpa-onnx is used in 3 files (`sherpa_adapter.rs`, `model_download.rs`, `commands.rs`). The extractor (`SpeakerEmbeddingExtractor`), the manager (`SpeakerEmbeddingManager`), and the diarization-config plumbing are removed; the AHC/smoothing/cap/refine code stays. The `model_download.rs` pyannote URL stays (pyannote is still downloaded); the nemo_titanet download URL stays (the model file is unchanged, only the loader changes). **Mitigation:** the port is already built and validated in `embed-probe-ort`; the productionization is mechanical (lift into the speaker module, wire into the adapter struct).
- **[Long-meeting resolution loss]** Uniform shed-to-cap lowers per-region resolution on long meetings; acceptable because turns are re-derived downstream. The long-meeting adversarial test gates that resolution loss does not collapse the alternation region's turn structure.
- **[Parakeet coexistence]** Parakeet transcription also uses `ort 2.0.0-rc.10`. After this change, Parakeet + nemo_titanet + pyannote all share one ORT runtime — no conflict by construction (the conflict was only ever sherpa's bundled ORT vs the `ort` crate). Verified: the `embed-probe-ort` crate links only `ort` and runs nemo_titanet without crash.

## Migration Plan

Code-only. No schema migration, no subprocess, no second binary, no signing change. Rollback = revert code (sherpa-onnx returns as a dep). Existing meetings keep prior (coarse) boundaries until re-diarized. The spec delta records the pyannote-via-ort boundary source + the in-process consumption requirement.

## Security Model

Pyannote boundaries are derived from the audio signal, not from any untrusted text field. No new untrusted input surface. The ported extractor reads the same on-disk nemo_titanet model sherpa did; the registry's `sqlx` `?` binding is unchanged. No IPC boundary (contrast Option 3), so no IPC-injection surface.

## Adversarial Tests (§4)

cde5c264 boundary oracle — **two windows**: (a) the Ricardo interjection at ≈46:58 and (b) the actual complaint window 5.7–32.5 s; in each, a boundary the chunk-grid-only baseline misses is present in the pyannote boundary set (assert the SPECIFIC boundary exists and is absent in baseline, not a window-wide count delta). End-to-end persistence oracle — the pipeline WITH Part B (on top of Part A) persists strictly more speaker-split rows for the complaint window than the chunk-grid baseline. Single-speaker meeting not fragmented. **Embedding fidelity gate (revised, from the architecture-loop close):** (1) cosine ≥ 0.99 on every clip ≥ 1.5s passing `is_effectively_silent`, over the fixed 10-clip set plus production-representative additions; (2) filter parity — the port drops the same clips sherpa drops (silence, <1.5s); (3) end-to-end AHC parity on ≥10 labeled multi-speaker recordings — identical cluster counts and ≥95% speaker-attributed segment overlap vs the pre-change sherpa reference (this is the gate cosine was always a proxy for). **Uniform-shed turn-recovery** — a long (≥45 min) two-speaker rapid-alternation fixture sheds to the cap, and Meetily's AHC+smoothing still recovers the alternation turn structure (≥80% within-region turns). **Adversarial audio** — noise/music that triggers many false change-points does not collapse the meeting to one speaker. Stress (candidate boundaries ≈10× cap) sheds without OOM. Silent/empty audio → empty boundary set → no crash. Perf gate on cde5c264 (sub-budget wall-clock; the in-process path is faster than the subprocess alternative). **Phantom-model consumption** — swap the segmentation model for a committed dummy fixture and assert a deterministic behavior change. **Sherpa removal verification** — `cargo tree` confirms `sherpa-onnx` and `sherpa-onnx-sys` are no longer in the dependency graph; a grep for `sherpa_onnx::` in `src/` returns zero hits.

> **Smoke carve-out.** Part B touches zero frontend code (Rust adapter). Per CLAUDE.md §3 smoke is mandated for user-visible *frontend* behavior; more speaker badges flow through Part A's already-smoke-tested persist path. No separate smoke spec.

## Open Questions

1. **Realfft vs kissfft.** The 0.005 residual on production clips traces partly to realfft-vs-kissfft rounding. Swapping the port to a Rust kissfft binding would close part of the gap but adds a dep and the residual is already within margin. Defer unless the end-to-end AHC parity test reveals a regression.
2. **CMVN-on-padded-frames semantics.** The port applies CMVN before pad-16 (matching sherpa). The padded zero-frames are post-CMVN, so they are literally zero in the model input — matching sherpa. Verified, but worth a unit test asserting the padded region is exactly zero.
3. **Two-pass smart decimation.** Would a second pyannote pass at a meaningful threshold enable within-run-first decimation that preserves turn boundaries better than region-fair? Defer unless the region-fair adversarial test reveals unsatisfactory preservation on real long meetings.
