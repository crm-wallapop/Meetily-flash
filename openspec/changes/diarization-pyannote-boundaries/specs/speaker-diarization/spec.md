## MODIFIED Requirements

### Requirement: Transcript-timestamp-driven speaker diarization runs as a post-processing queue phase

This requirement amends the canonical requirement of the same name. The canonical requirement's item 3 mandates the **effective-split chunk grid**: "Chunk each segment into pieces sized at the **effective split granularity** = `max(SPLIT_TARGET_SECS, speech_seconds / MAX_DIARIZATION_CHUNKS)` … Each piece remains within [`MIN_SPEECH_SECS`, `MAX_CHUNK_SECS`]." With this change the chunk grid is no longer the source of speaker-change-point boundaries on the success path; the in-process pyannote `ort::Session` (see the "Diarization segment granularity resolves speaker turns within Whisper segments" requirement below) supplies intra-region splits that `build_chunks` consumes INSTEAD of the effective-split grid. The canonical item 3's `effective_split` mandate is STRUCK on the success path: when the pyannote model is present, `build_chunks` sub-divides each Whisper speech region by the pyannote boundaries and does NOT apply `effective_split`. The only fallback is pyannote-model-missing: when the segmentation model file is absent, `build_chunks` applies `effective_split` exactly as the canonical item 3 states, so the meeting still diarizes at coarse resolution. (There is no child-failure fallback — pyannote runs in-process via the same `ort` runtime as Parakeet, so there is no subprocess whose spawn/crash/timeout/schema-mismatch failure could fire.)

The canonical "Short meeting is unaffected by the chunk cap" scenario asserts `effective granularity equals SPLIT_TARGET_SECS (3.0 s) — unchanged from before this change`. That assertion is RE-POINTED to the in-process pyannote boundary source: on a short (~10 min) meeting the pyannote model is present (the cap is not hit), and the per-region granularity is set by the pyannote change-points inside each Whisper segment — NOT by a fixed `SPLIT_TARGET_SECS` grid. The "chunk count is identical to a fixed-3 s chunker" clause no longer holds on the success path; the chunk count on the success path equals the count of pyannote change-points (capped). On the pyannote-model-missing fallback path the canonical assertion holds unchanged.

(A delta that leaves the canonical item 3 `effective_split` mandate in place alongside a pyannote pre-splitter requirement would make the canonical spec self-contradict — both cannot be the chunk-layout source simultaneously. This amendment removes that contradiction.)

#### Scenario: Short meeting succeeds via the in-process pyannote boundary source (re-points the canonical "Short meeting" scenario)

- **GIVEN** a meeting with ~10 minutes of speech AND the pyannote segmentation model is present on disk
- **WHEN** diarization runs
- **THEN** the per-region chunk granularity is set by the pyannote change-points inside each Whisper speech region (NOT a fixed `SPLIT_TARGET_SECS` grid)
- **AND** `effective_split` is NOT applied on the success path
- **AND** the chunk count equals the count of pyannote change-points (capped at `MAX_DIARIZATION_CHUNKS`)

#### Scenario: Short meeting falls back to the effective-split grid when the pyannote model is missing

- **GIVEN** a meeting with ~10 minutes of speech AND the pyannote segmentation model file is absent from disk
- **WHEN** diarization runs and the in-process pyannote source is unavailable
- **THEN** `build_chunks` applies the canonical effective-split grid (`SPLIT_TARGET_SECS = 3.0 s` for a short meeting) exactly as the canonical item 3 states
- **AND** the effective granularity equals `SPLIT_TARGET_SECS` (3.0 s) — the canonical assertion holds on the fallback path
- **AND** no panic propagates to the user-facing diarization flow

### Requirement: Diarization segment granularity resolves speaker turns within Whisper segments

Whisper groups transcript segments by sentence/VAD, not by speaker; on multi-speaker meetings these segments routinely span 15–30s and contain two or more speakers. The diarization output SHALL be granular enough that a speaker turn occurring inside a single Whisper transcript segment produces a diarization segment boundary at or near the turn, so that per-word alignment can attribute the words on each side of the turn to the correct speakers rather than collapsing the whole segment to one speaker.

Speaker change-point boundaries SHALL be sourced from a pyannote `ort::Session` running **in-process** — a second `ort::Session` (the first serves Parakeet transcription; after this change a third serves the ported nemo_titanet extractor) over `pyannote-segmentation-3.0`, the exact pattern the Phase 1 probe (`pyannote_ort_probe.rs:48-59`) validated. The segmentation + sliding-window + powerset-decode + smoothing + boundary-emission logic is the productionized form of the Phase 2b probe (`pyannote_ort_probe.rs`): slide a 10s window at 1s step over the recording's 16 kHz mono samples, decode per-frame powerset logits to 3-speaker multilabel activity via hysteresis at onset 0.5, apply pyannote-default smoothing (median filter rad=3, min_on=0.3s, max_off=0.5s — the only Phase 2b config that hit BOTH known anchors), and emit `Vec<(start_seconds, end_seconds)>` change-points. The diarization flow (`commands.rs:413-432`) SHALL INTERSECT the pyannote change-points with the Whisper `transcript_segments` (`fetch_transcript_timestamps`) — a pyannote change-point inside a Whisper speech region is kept as an intra-region split; the Whisper silence regions are preserved as silence (not embedded). The intersected set is passed as the `transcript_segments` argument to `adapter.process()`.

**Why in-process on one runtime, not a subprocess:** sherpa-onnx-sys 1.13.4 statically bundles ORT 1.17.1 (C-API ≤17); the project's `ort = "2.0.0-rc.10"` dep (used for Parakeet transcription) brings C-API 27. The two runtimes collide on the global C-API symbol table the moment both are linked into one process → STATUS_ACCESS_VIOLATION. This was verified by the `pyannote_sherpa_load_crux` probe. This change resolves the conflict at the root by **removing sherpa-onnx entirely** and porting nemo_titanet embedding extraction to the `ort` crate (see design.md D1); with sherpa gone, Parakeet + nemo_titanet + pyannote all share one ORT runtime — no conflict by construction. The port is empirically validated: the `embed-probe-ort` crate reproduces sherpa's nemo_titanet embeddings at cosine **0.9946–0.9989** on production-relevant clips (clean/overlap ≥1.5s, non-silent) after a one-line log-floor fix (`f32::MIN_POSITIVE` → `f32::EPSILON`), well within the AHC operating margin. See `openspec/exploration/diarization-pyannote-boundaries-ort-probe.md` §"ARCHITECTURE LOOP CLOSED". A subprocess/IPC/second-binary path (Option 3) was panel-rejected as permanent subprocess debt once the port proved viable.

The pyannote boundary set AUGMENTS the Whisper transcript segments with intra-region splits; it does NOT supersede or replace the Whisper boundaries (which remain the speech-vs-silence mask). After this change, `build_chunks` sub-divides each Whisper speech region by the pyannote boundaries inside it and no longer applies its own `effective_split` uniform grid (`sherpa_adapter.rs:331`); the `MAX_DIARIZATION_CHUNKS` cap is enforced once, at the pyannote-boundary layer (see the uniform-shed scenario below). (A proposal that leaves both the uniform-grid step and the pyannote pre-splitter mandated simultaneously is NON-CONFORMANT — the canonical spec would contradict itself.)

The pyannote `ort::Session` emits boundaries only — no speaker labels, no embeddings (the session is over pyannote-segmentation-3.0 only). This is stronger than "labels discarded": there is nothing to discard. Meetily's AHC clustering, label-quality refinement, most-isolated-cluster cap, temporal-coherence smoothing, and cross-meeting registry matching remain authoritative for labeling, exactly as today.

#### Scenario: Sub-turn interjection is isolated, not swallowed

- **GIVEN** a Whisper transcript segment from 46:58 to 47:21 containing a 2s Ricardo interjection at 46:58–47:00 followed by Cynthia's speech
- **AND** the production diarization previously labeled the entire 46:58–47:30 run as Cynthia
- **WHEN** diarization runs with the in-process pyannote boundary source
- **THEN** the diarization output contains a speaker segment boundary near 47:00 separating Ricardo (≈46:50–47:00) from Cynthia (≈47:00 onward), so the interjection's words are attributed to Ricardo
- **AND** the chunk-grid-only baseline over the same window does not produce that boundary

#### Scenario: Back-and-forth between two speakers is not collapsed to one

- **GIVEN** a region where two speakers alternate in 4–8s turns across a 30s window
- **WHEN** diarization runs with the in-process pyannote boundary source
- **THEN** the output preserves the alternation as multiple segments rather than merging the window into a single speaker's run

#### Scenario: Single-speaker meeting is not fragmented

- **GIVEN** a meeting with exactly one speaker
- **WHEN** diarization runs with the in-process pyannote boundary source
- **THEN** the output is a single speaker (no spurious second cluster introduced by the finer boundary placement)

#### Scenario: Pyannote-model-missing falls back to the effective-split grid

- **GIVEN** the pyannote segmentation model file is absent from disk (not downloaded, or deleted)
- **WHEN** diarization runs and the in-process pyannote session cannot be constructed
- **THEN** the diarization proceeds with the canonical effective-split (`SPLIT_TARGET_SECS`) grid as the `transcript_segments` subdivision source
- **AND** the meeting still diarizes (at coarse resolution); only the finer pyannote boundaries are lost
- **AND** no panic propagates to the user-facing diarization flow

#### Scenario: Uniform shed-to-cap still recovers alternation turns on long meetings

- **GIVEN** a long (≥45 min) meeting with a rapid two-speaker alternation region and a single-speaker monologue region of comparable length
- **WHEN** the candidate-boundary count exceeds `MAX_DIARIZATION_CHUNKS` and uniform shedding runs (every k-th by position), followed by Meetily's AHC + temporal-coherence smoothing
- **THEN** the alternation region's turn structure is recovered (a threshold fraction of within-region turns are preserved in the final labeling), because turns are re-derived from the surviving candidate set, not carried by individual shed boundaries
- **AND** the resulting chunk count passed to `adapter.process()` is at or below `MAX_DIARIZATION_CHUNKS`

#### Scenario: Silent or empty audio does not crash the in-process flow

- **GIVEN** a silent or empty audio fixture
- **WHEN** the in-process pyannote session runs and yields an empty boundary set
- **THEN** the diarization proceeds (with an empty intersected set or the effective-split fallback) without panicking

## ADDED Requirements

### Requirement: The pyannote segmentation model is actually consumed by the in-process ort::Session

The pyannote-segmentation ONNX model SHALL be loaded and run by an in-process `ort::Session` (the second `ort` session in the process, alongside the Parakeet and nemo_titanet sessions) — NOT by a child binary or sherpa's `OfflineSpeakerDiarization` (which is non-viable due to the ORT runtime conflict). The session SHALL emit a deterministic, non-empty boundary set on real multi-speaker audio when the model is present, and SHALL change behavior (construction error or distinct/empty segmentation output) when the model file is swapped for a committed dummy fixture. This closes the prior phantom-dependency state where `segmentation_model_path` was accepted by the adapter constructor, existence-checked, and discarded.

#### Scenario: The in-process session loads and runs the segmentation model

- **GIVEN** the in-process pyannote `ort::Session` is constructed with a `model_dir` pointing at the on-disk pyannote model
- **WHEN** the session runs inference on a real multi-speaker clip
- **THEN** the emitted boundary set is deterministic and non-empty
- **AND** swapping the model file for a committed dummy fixture changes the session's behavior (construction error or distinct segmentation output — presence-of-path alone is not sufficient evidence of consumption)

### Requirement: nemo_titanet embedding extraction is ported to ort and sherpa-onnx is removed

The nemo_titanet embedding extraction SHALL be performed by an in-process `ort::Session` (the ported `NemoEmbeddingExtractor`, lifting the validated `embed-probe-ort` fbank + CMVN + pad-16 + transpose + session-builder pipeline) — NOT by sherpa-onnx's `SpeakerEmbeddingExtractor`. The `SpeakerEmbeddingManager` SHALL be replaced by a pure-Rust in-memory cosine store (`HashMap<String, Vec<Vec<f32>>>` + cosine search; sherpa's manager was a convenience wrapper, not a model). sherpa-onnx and sherpa-onnx-sys SHALL be removed from `Cargo.toml`, so the whole app links exactly one ORT runtime (the `ort` crate) and the C-API 17-vs-27 collision that motivated this change cannot occur by construction. The stored `speaker_embeddings` vectors remain nemo_titanet 192-dim — no schema migration.

#### Scenario: sherpa-onnx is no longer in the dependency graph

- **GIVEN** the port is complete and `Cargo.toml` no longer declares `sherpa-onnx`
- **WHEN** `cargo tree` is run on the workspace
- **THEN** neither `sherpa-onnx` nor `sherpa-onnx-sys` appears in the dependency graph
- **AND** a grep for `sherpa_onnx::` in `frontend/src-tauri/src/` returns zero hits (the port replaced every sherpa reference in the speaker module)

#### Scenario: The port reproduces sherpa's embeddings within the AHC operating margin

- **GIVEN** the fixed 10-clip gate set plus production-representative additions, each clip ≥ 1.5s and passing `is_effectively_silent`
- **WHEN** the ported `NemoEmbeddingExtractor` and the sherpa reference extract embeddings from the same 16kHz mono clip
- **THEN** the cosine similarity between the two embeddings is ≥ 0.99 on every such clip
- **AND** filter parity holds — the port drops (via `is_effectively_silent` and `MIN_SPEECH_SECS`) exactly the clips sherpa drops

#### Scenario: End-to-end AHC parity vs the sherpa reference

- **GIVEN** ≥10 labeled multi-speaker recordings
- **WHEN** the full diarization pipeline (ported nemo_titanet extractor + in-process pyannote boundaries + the unchanged AHC + cap + smoothing + refine_pass2 + registry) runs on each
- **THEN** the resulting cluster counts are identical to the pre-change sherpa reference
- **AND** speaker-attributed segment overlap vs the reference is ≥ 95% (this is the gate cosine was always a proxy for)
