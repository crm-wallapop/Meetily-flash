## MODIFIED Requirements

### Requirement: Transcript-timestamp-driven speaker diarization runs as a post-processing queue phase

This requirement amends the canonical requirement of the same name. The canonical requirement's item 3 mandates the **effective-split chunk grid**: "Chunk each segment into pieces sized at the **effective split granularity** = `max(SPLIT_TARGET_SECS, speech_seconds / MAX_DIARIZATION_CHUNKS)` … Each piece remains within [`MIN_SPEECH_SECS`, `MAX_CHUNK_SECS`]." With this change the chunk grid is no longer the source of speaker-change-point boundaries; the pyannote subprocess (see the "Diarization segment granularity resolves speaker turns within Whisper segments" requirement below) supplies intra-region splits that `build_chunks` consumes INSTEAD of the effective-split grid. The canonical item 3's `effective_split` mandate is STRUCK: on the success path, `build_chunks` sub-divides each Whisper speech region by the pyannote boundaries and does NOT apply `effective_split`. On the fallback path (child unavailable), `build_chunks` applies `effective_split` exactly as the canonical item 3 states, so the fallback preserves the status-quo behavior.

The canonical "Short meeting is unaffected by the chunk cap" scenario asserts `effective granularity equals SPLIT_TARGET_SECS (3.0 s) — unchanged from before this change`. That assertion is RE-POINTED to the pyannote boundary source: on a short (~10 min) meeting the pyannote subprocess succeeds (the cap is not hit), and the per-region granularity is set by the pyannote change-points inside each Whisper segment — NOT by a fixed `SPLIT_TARGET_SECS` grid. The "chunk count is identical to a fixed-3 s chunker" clause no longer holds on the success path; the chunk count on the success path equals the count of pyannote change-points (capped). On the fallback path the canonical assertion holds unchanged.

(A delta that leaves the canonical item 3 `effective_split` mandate in place alongside a pyannote pre-splitter requirement would make the canonical spec self-contradict — both cannot be the chunk-layout source simultaneously. This amendment removes that contradiction.)

#### Scenario: Short meeting succeeds via the pyannote subprocess (re-points the canonical "Short meeting" scenario)

- **GIVEN** a meeting with ~10 minutes of speech AND the pyannote subprocess succeeds
- **WHEN** diarization runs
- **THEN** the per-region chunk granularity is set by the pyannote change-points inside each Whisper speech region (NOT a fixed `SPLIT_TARGET_SECS` grid)
- **AND** `effective_split` is NOT applied on the success path
- **AND** the chunk count equals the count of pyannote change-points (capped at `MAX_DIARIZATION_CHUNKS`)

#### Scenario: Short meeting falls back to the effective-split grid on subprocess failure

- **GIVEN** a meeting with ~10 minutes of speech AND the pyannote subprocess fails (crash / timeout / schema mismatch)
- **WHEN** diarization runs and the parent falls back
- **THEN** `build_chunks` applies the canonical effective-split grid (`SPLIT_TARGET_SECS = 3.0 s` for a short meeting) exactly as the canonical item 3 states
- **AND** the effective granularity equals `SPLIT_TARGET_SECS` (3.0 s) — the canonical assertion holds on the fallback path

### Requirement: Diarization segment granularity resolves speaker turns within Whisper segments

Whisper groups transcript segments by sentence/VAD, not by speaker; on multi-speaker meetings these segments routinely span 15–30s and contain two or more speakers. The diarization output SHALL be granular enough that a speaker turn occurring inside a single Whisper transcript segment produces a diarization segment boundary at or near the turn, so that per-word alignment can attribute the words on each side of the turn to the correct speakers rather than collapsing the whole segment to one speaker.

Speaker change-point boundaries SHALL be sourced from a pyannote-backed **subprocess**: a standalone child binary (a new crate modeled on the `embed-probe-ort` harness, which is a TEMPLATE for the `ort` session-builder pattern and IPC scaffolding only — its `main.rs` emits nemo embeddings, not pyannote boundaries; the segmentation + smoothing + boundary-emission logic is ported from `frontend/src-tauri/tests/pyannote_ort_probe.rs` Phase 2b) that links ONLY the `ort = "2.0.0-rc.10"` runtime (NOT sherpa-onnx) and runs `pyannote-segmentation-3.0` over the recording's 16 kHz mono audio. The child SHALL apply pyannote-default smoothing (median filter rad=3, min_on=0.3s, max_off=0.5s — the only Phase 2b config that hit BOTH known anchors) at onset 0.5, and emit `Vec<(start_seconds, end_seconds)>` boundaries as JSON over stdout (with a `schema_version: u32` field the parent pins and validates). The parent process (`commands.rs:413-432`) SHALL spawn the child via `tokio::process::Command` (NOT `std::process::Command` inside `spawn_blocking`, so the inference wait does not pin a blocking-pool thread), parse its stdout, and INTERSECT the pyannote boundaries with the Whisper `transcript_segments` (`fetch_transcript_timestamps`) — a pyannote boundary inside a Whisper speech region is kept as an intra-region split; the Whisper silence regions are preserved as silence (not embedded). The intersected set is passed as the `transcript_segments` argument to `adapter.process()`.

**Why a subprocess, not in-process sherpa:** sherpa-onnx-sys 1.13.4 statically bundles ORT 1.17.1 (C-API ≤17); the project's `ort = "2.0.0-rc.10"` dep (used for Parakeet transcription) brings C-API 27. The two runtimes collide on the global C-API symbol table the moment both are linked into one process → STATUS_ACCESS_VIOLATION. This was verified by the `pyannote_sherpa_load_crux` probe: sherpa's `OfflineSpeakerDiarization::create()` crashes even in a test that imports ONLY `sherpa_onnx` (no `use ort::*`), because `ort` is linked for Parakeet regardless of what the diarization path imports. Process isolation is the OS primitive that resolves the address-space collision without porting working code (all-ort, panel-rejected on silent-drift risk) or removing a working dep (all-sherpa, impossible — Parakeet needs `ort`).

The pyannote boundary set AUGMENTS the Whisper transcript segments with intra-region splits; it does NOT supersede or replace the Whisper boundaries (which remain the speech-vs-silence mask). After this change, `build_chunks` sub-divides each Whisper speech region by the pyannote boundaries inside it and no longer applies its own `effective_split` uniform grid (`sherpa_adapter.rs:331`); the `MAX_DIARIZATION_CHUNKS` cap is enforced once, in the child's shed step (see the next requirement). (A proposal that leaves both the uniform-grid step and the pyannote pre-splitter mandated simultaneously is NON-CONFORMANT — the canonical spec would contradict itself.)

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

The parent SHALL emit a structured `boundary_source` log marker on EVERY diarization run: `boundary_source = "pyannote"` when the child succeeded and the intersected set was consumed, or `boundary_source = "chunk-grid-fallback"` when ANY fallback fired (child failed to spawn / crashed / timed out / emitted a schema mismatch / emitted NaN-bearing output). This marker is what makes the "subprocess-sourced" requirement falsifiable even when the fallback fires — without it, a silent fallback would be observationally indistinguishable from a success at the diarization output level. The marker is emitted exactly once per diarization run, before `adapter.process()` is invoked on the chosen boundary set.

#### Scenario: Child failure degrades gracefully to chunk-grid boundaries

- **GIVEN** the pyannote child binary fails to spawn, crashes, times out, or emits unparseable output (or NaN-bearing output from a corrupted model)
- **WHEN** the parent's spawn+parse helper detects the failure
- **THEN** the parent logs the failure and proceeds with the existing chunk-grid (transcript-timestamp) boundaries as the `transcript_segments` argument to `adapter.process()`
- **AND** the meeting still diarizes (at coarse resolution); only the finer pyannote boundaries are lost
- **AND** no panic propagates to the user-facing diarization flow
- **AND** the parent emits a `boundary_source = "chunk-grid-fallback"` log marker on this run

#### Scenario: Success path emits the pyannote marker

- **GIVEN** the pyannote child binary succeeds and emits a parseable, schema-matching, NaN-free boundary set
- **WHEN** the parent intersects the boundaries with the Whisper segments and passes the intersected set to `adapter.process()`
- **THEN** the parent emits a `boundary_source = "pyannote"` log marker on this run

#### Scenario: Out-of-order boundaries are sorted before processing

- **GIVEN** the child emits a non-monotonic boundary set (e.g. a boundary at 30s followed by one at 10s)
- **WHEN** the parent receives the set
- **THEN** the parent sorts the boundaries into monotonic order before intersecting with the Whisper segments and passing to `adapter.process()`
- **AND** no out-of-order boundary reaches `build_chunks`

#### Scenario: Boundaries beyond duration_secs are clamped

- **GIVEN** the child emits boundaries with `start` or `end` values outside `[0, duration_secs]` (e.g. a boundary at `duration_secs + 5.0`)
- **WHEN** the parent receives the set
- **THEN** the parent clamps each boundary's `start`/`end` to `[0, duration_secs]` before intersecting and processing
- **AND** no out-of-range boundary reaches `build_chunks`

#### Scenario: Corrupted model emits NaN and triggers fallback (not silent garbage)

- **GIVEN** the child's segmentation model file is corrupted (NOT absent — present but damaged) such that the child emits NaN/garbage boundary values at exit 0
- **WHEN** the parent parses the stdout
- **THEN** the parent detects the NaN/Inf values via strict parsing and routes to fallback (no NaN value reaches `adapter.process()`)
- **AND** the parent emits a `boundary_source = "chunk-grid-fallback"` marker

#### Scenario: Parent killed mid-inference takes the child with it

- **GIVEN** the child is mid-inference and the parent process is killed (e.g. app closed, OS kill)
- **WHEN** the parent's `tokio::process::Child` handle is dropped
- **THEN** the child is killed via `Command::kill` on drop (tokio `Child` kills on drop by default) — no orphaned child process continues consuming CPU after the parent is gone

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

The child binary SHALL be code-signed via Tauri's `bundle.externalBin` mechanism (declared as `binaries/pyannote-boundaries` in `tauri.conf.json`, built with the `-$TARGET_TRIPLE` suffix in CI copying the `build.yml:508-561` `llama-helper` pattern), which applies the existing `windows.signCommand` (`sign-windows.ps1` with DigiCert `smctl`) automatically — the same signing path as `llama-helper`. The child SHALL be resolvable at runtime by reusing the `resolve_helper_binary` pattern at `sidecar.rs:108-227` verbatim (env-var override → relative-to-exe → `RESOURCE_DIR` fuzzy match). On a CI Windows runner (`windows-latest`), the signed child SHALL spawn via the real NSIS-installed path and complete the IPC round-trip (exit-0 + valid JSON) within the documented budget. Defender first-launch reputation is non-deterministic and cannot be CI-gated; a scan/quarantine stall produces a SILENT FALLBACK (the child fails to spawn or stalls past timeout, the parent falls back to chunk-grid and emits `boundary_source = "chunk-grid-fallback"`), which is acceptable because the fallback is correct. The "no quarantine on a clean machine" property is a release note, not a test assertion.

#### Scenario: Signed child spawns and completes the IPC round-trip on a CI Windows runner

- **GIVEN** a release build of the main app with the signed child packaged as an `externalBin` entry (NSIS-installed alongside the main app)
- **WHEN** the parent spawns the child on a CI Windows runner (`windows-latest`) via the real NSIS-installed path
- **THEN** the child spawns and completes the IPC round-trip (exit-0 + valid JSON matching the pinned `schema_version`) within the documented budget
- **AND** a botched or missing signature is detected at release-gate time (not in production)
- **AND** a Defender scan/quarantine stall — if it occurs on an end-user machine — routes to the silent fallback (parent emits `boundary_source = "chunk-grid-fallback"`), NOT a crash
