## MODIFIED Requirements

### Requirement: Diarization segment granularity resolves speaker turns within Whisper segments

Whisper groups transcript segments by sentence/VAD, not by speaker; on multi-speaker meetings these segments routinely span 15–30s and contain two or more speakers. The diarization output SHALL be granular enough that a speaker turn occurring inside a single Whisper transcript segment produces a diarization segment boundary at or near the turn, so that per-word alignment can attribute the words on each side of the turn to the correct speakers rather than collapsing the whole segment to one speaker.

Speaker change-point boundaries SHALL be sourced from a pyannote-backed **subprocess**: a standalone child binary (the productionized `embed-probe-ort` crate) that links ONLY the `ort = "2.0.0-rc.10"` runtime (NOT sherpa-onnx) and runs `pyannote-segmentation-3.0` over the recording's 16 kHz mono audio. The child SHALL apply pyannote-default smoothing (median filter rad=3, min_on=0.3s, max_off=0.5s — the only Phase 2b config that hit BOTH known anchors) at onset 0.5, and emit `Vec<(start_seconds, end_seconds)>` boundaries as JSON over stdout. The parent process (`commands.rs:413-432`) SHALL spawn the child, parse its stdout, and pass the boundary set as the `transcript_segments` argument to `adapter.process()`.

**Why a subprocess, not in-process sherpa:** sherpa-onnx-sys 1.13.4 statically bundles ORT 1.17.1 (C-API ≤17); the project's `ort = "2.0.0-rc.10"` dep (used for Parakeet transcription) brings C-API 27. The two runtimes collide on the global C-API symbol table the moment both are linked into one process → STATUS_ACCESS_VIOLATION. This was verified by the `pyannote_sherpa_load_crux` probe: sherpa's `OfflineSpeakerDiarization::create()` crashes even in a test that imports ONLY `sherpa_onnx` (no `use ort::*`), because `ort` is linked for Parakeet regardless of what the diarization code imports. Process isolation is the OS primitive that resolves the address-space collision without porting working code (all-ort, panel-rejected on silent-drift risk) or removing a working dep (all-sherpa, impossible — Parakeet needs `ort`).

The child's boundary set SUPERSEDES the uniform-chunk-grid boundary source. After this change, `build_chunks` consumes the pyannote-bounded chunk layout and no longer applies its own `effective_split` uniform grid (`sherpa_adapter.rs:331`); the `MAX_DIARIZATION_CHUNKS` cap is enforced once, in the child's shed step (see the next requirement). (A proposal that leaves both the uniform-grid step and the pyannote pre-splitter mandated simultaneously is NON-CONFORMANT — the canonical spec would contradict itself.)

The child SHALL emit boundaries only — no speaker labels, no embeddings (the `ort::Session` is over pyannote-segmentation-3.0 only; nemo_titanet is NOT loaded by the child). This is stronger than "labels discarded": there is nothing to discard. Meetily's AHC clustering, label-quality refinement, most-isolated-cluster cap, temporal-coherence smoothing, and cross-meeting registry matching remain authoritative for labeling, exactly as today.

#### Scenario: Sub-turn interjection is isolated, not swallowed

- **GIVEN** a Whisper transcript segment from 46:58 to 47:21 containing a 2s Ricardo interjection at 46:58–47:00 followed by Cynthia's speech
- **AND** the production diarization previously labeled the entire 46:58–47:30 run as Cynthia
- **WHEN** diarization runs with the pyannote-backed boundary subprocess
- **THEN** the diarization output contains a speaker segment boundary near 47:00 separating Ricardo (≈46:50–47:00) from Cynthia (≈47:00 onward), so the interjection's words are attributed to Ricardo
- **AND** the chunk-grid-only baseline over the same window does not produce that boundary

#### Scenario: Back-and-forth between two speakers is not collapsed to one

- **GIVEN** a region where two speakers alternate in 4–8s turns across a 30s window
- **WHEN** diarization runs with the pyannote-backed boundary subprocess
- **THEN** the output preserves the alternation as multiple segments rather than merging the window into a single speaker's run

#### Scenario: Single-speaker meeting is not fragmented

- **GIVEN** a meeting with exactly one speaker
- **WHEN** diarization runs with the pyannote-backed boundary subprocess
- **THEN** the output is a single speaker (no spurious second cluster introduced by the finer boundary placement)

#### Scenario: Child failure degrades gracefully to chunk-grid boundaries

- **GIVEN** the pyannote child binary fails to spawn, crashes, times out, or emits unparseable output
- **WHEN** the parent's spawn+parse helper detects the failure
- **THEN** the parent logs the failure and proceeds with the existing chunk-grid (transcript-timestamp) boundaries as the `transcript_segments` argument to `adapter.process()`
- **AND** the meeting still diarizes (at coarse resolution); only the finer pyannote boundaries are lost
- **AND** no panic propagates to the user-facing diarization flow

#### Scenario: Uniform shed-to-cap still recovers alternation turns on long meetings

- **GIVEN** a long (≥45 min) meeting with a rapid two-speaker alternation region and a single-speaker monologue region of comparable length
- **WHEN** the child's candidate-boundary count exceeds `MAX_DIARIZATION_CHUNKS` and uniform shedding runs (every k-th by position), followed by Meetily's AHC + temporal-coherence smoothing
- **THEN** the alternation region's turn structure is recovered (a threshold fraction of within-region turns are preserved in the final labeling), because turns are re-derived from the surviving candidate set, not carried by individual shed boundaries
- **AND** the resulting chunk count passed to `adapter.process()` is at or below `MAX_DIARIZATION_CHUNKS`

#### Scenario: Silent or empty audio does not crash the subprocess flow

- **GIVEN** a silent or empty audio fixture
- **WHEN** the child runs and emits an empty boundary set
- **THEN** the parent falls back to chunk-grid boundaries (or empty input) without panicking

## ADDED Requirements

### Requirement: The pyannote segmentation model is actually consumed by the child subprocess

The pyannote-segmentation ONNX model SHALL be loaded and run by the child binary via an `ort::Session`. The child's stdout SHALL contain a deterministic, non-empty boundary set on real multi-speaker audio when the model is present, and SHALL change behavior (construction error or distinct/empty segmentation output) when the model file is swapped for a committed dummy fixture. This closes the prior phantom-dependency state where `segmentation_model_path` was accepted by the adapter constructor, existence-checked, and discarded.

#### Scenario: Child loads and runs the segmentation model

- **GIVEN** the child binary is invoked with a `model_dir` pointing at the on-disk pyannote model
- **WHEN** the child loads the model and runs inference on a real multi-speaker clip
- **THEN** the child's stdout contains a deterministic, non-empty boundary set
- **AND** swapping the model file for a committed dummy fixture changes the child's behavior (construction error or distinct segmentation output — presence-of-path alone is not sufficient evidence of consumption)

### Requirement: The child binary is signed and spawnable on Windows

The child binary SHALL be code-signed with the same certificate as the main application and packaged as a Tauri resource. On a clean Windows machine with Windows Defender enabled, the parent SHALL be able to spawn the child without quarantine or first-launch scanning stalls that exceed a documented budget. This is the panel's most-cited operational risk; it is a release gate, not a unit test.

#### Scenario: Signed child spawns cleanly on a clean Windows machine

- **GIVEN** a release build of the main app with the signed child packaged as a Tauri resource
- **WHEN** the parent spawns the child on a clean Windows machine with Defender enabled
- **THEN** the child spawns without quarantine and completes the IPC round-trip within the documented budget
- **AND** a botched or missing signature is detected at release-gate time (not in production)
