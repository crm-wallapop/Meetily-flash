## ADDED Requirements

### Requirement: Recording start is synchronous — no pre-flight model checks

The recording-start path SHALL NOT perform any asynchronous Tauri IPC calls before invoking `start_recording`. In particular, the Parakeet model-readiness checks (`parakeet_init`, `parakeet_has_available_models`, `parakeet_get_available_models`) SHALL NOT be called at recording-start time. Transcription model readiness is irrelevant to the recording-start path because transcription happens post-recording via the Whisper retranscription pipeline, not via Parakeet.

#### Scenario: Start button responds immediately on click

- **WHEN** the user clicks "Start recording"
- **THEN** the recording-start path calls `start_recording` without any preceding Tauri IPC round-trips
- **AND** the button responds within one render frame (no async latency before the UI reflects `STARTING` status)

---

### Requirement: The recording pipeline does not run VAD or Whisper during capture

The audio pipeline SHALL only encode audio to MP4 during a recording session. No VAD processor SHALL be initialised and no Whisper inference SHALL occur during recording. No `transcript-update` events SHALL be emitted while a recording is in progress.

#### Scenario: Pipeline skips VAD initialisation

- **WHEN** a recording starts
- **THEN** `ContinuousVadProcessor` is NOT constructed
- **AND** the pipeline's Whisper inference path is NOT entered
- **AND** no `transcript-update` events are emitted for the lifetime of the recording

#### Scenario: CPU and memory baseline during recording is lower

- **WHEN** a recording is in progress
- **THEN** no Whisper context is held in memory
- **AND** the pipeline consumes no GPU resources for transcription

---

### Requirement: The recording pipeline does not write per-chunk audio checkpoint files

The incremental saver SHALL stream audio directly to the in-progress `audio.mp4` file without producing intermediate `.ckpt` files. The MP4 file is the sole audio artefact written during a session.

#### Scenario: No checkpoint files in the meeting folder during recording

- **WHEN** a recording is in progress
- **THEN** the meeting folder contains `audio.mp4` (in progress) and `metadata.json` only
- **AND** no files with the extension `.ckpt` are written

#### Scenario: Migration cleanup removes legacy checkpoints

- **GIVEN** a meeting folder created before this change contains `.ckpt` files alongside `audio.mp4`
- **WHEN** the startup GC pass runs
- **THEN** the legacy `.ckpt` files are deleted
- **AND** the `audio.mp4` and `metadata.json` are preserved
