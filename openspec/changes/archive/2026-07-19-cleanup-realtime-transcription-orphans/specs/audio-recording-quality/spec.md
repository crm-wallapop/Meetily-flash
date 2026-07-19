## MODIFIED Requirements

### Requirement: The recording pipeline does not run VAD or Whisper during capture

The audio pipeline SHALL only encode audio to MP4 during a recording session. No VAD processor SHALL be initialised and no Whisper inference SHALL occur during recording. No transcript-content Tauri events SHALL be emitted while a recording is in progress; transcription runs strictly post-meeting via the retranscription queue or the import path.

#### Scenario: Pipeline skips VAD initialisation

- **WHEN** a recording starts
- **THEN** `ContinuousVadProcessor` is NOT constructed
- **AND** the pipeline's Whisper inference path is NOT entered
- **AND** no transcript-content events are emitted for the lifetime of the recording

#### Scenario: CPU and memory baseline during recording is lower

- **WHEN** a recording is in progress
- **THEN** no Whisper context is held in memory
- **AND** the pipeline consumes no GPU resources for transcription
