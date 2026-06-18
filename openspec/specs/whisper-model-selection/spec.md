# Whisper Model Selection — Capability Spec

## Purpose

Governs which Whisper models are offered in the selection UI and how they are presented to the user, including tier placement, display labels, and catalog descriptions.
## Requirements
### Requirement: small-q5_1 is visible in the basic model tier
The `small-q5_1` model SHALL appear in the primary (non-accordion) model list alongside other basic models, without requiring the user to expand an "Advanced" section.

#### Scenario: Model list renders small-q5_1 without expanding advanced section
- **WHEN** the WhisperModelManager component mounts
- **THEN** the `small-q5_1` model card is visible in the DOM without any user interaction

#### Scenario: small-q5_1 does not appear in advanced accordion
- **WHEN** the WhisperModelManager component mounts
- **THEN** the `small-q5_1` model is NOT listed inside the advanced models accordion

### Requirement: small-q5_1 displays a human-readable fast-mode label
The `small-q5_1` model SHALL display as "Small (Fast Mode)" in the model selection UI.

#### Scenario: Display name is shown for small-q5_1
- **WHEN** the model card for `small-q5_1` is rendered
- **THEN** the visible label reads "Small (Fast Mode)"

### Requirement: small-q5_1 catalog description communicates measured trade-off
The description shown for `small-q5_1` in the UI and catalog SHALL reference the measured performance gain and accuracy trade-off using concrete numbers.

#### Scenario: Description contains performance context
- **WHEN** the `small-q5_1` entry is read from `WHISPER_MODEL_CATALOG`
- **THEN** the description contains both a speed multiplier (approximately 3.5×) and an accuracy trade-off (~4%)

### Requirement: Whisper provider stores token timestamps in the database

When Whisper is the active transcription provider, the transcription worker SHALL extract per-token timestamps from each Whisper segment (using `set_token_timestamps(true)` which is already enabled). The token timestamps SHALL be serialized as a JSON array of `{word: string, start_ms: i64, end_ms: i64}` objects and stored in the `token_timestamps` column of the `transcripts` table.

The `TranscriptResult` struct SHALL be extended with an optional `token_timestamps: Option<String>` field. The `TranscriptUpdate` Tauri event SHALL include the `token_timestamps` field.

#### Scenario: Whisper provider populates token timestamps

- **WHEN** Whisper transcribes an audio chunk and produces segments with token timestamps
- **THEN** each transcript row in the database has `token_timestamps` populated with a JSON array of word-level timing
- **AND** the `TranscriptUpdate` event carries the same `token_timestamps` data

#### Scenario: Parakeet provider leaves token timestamps null

- **WHEN** Parakeet is the active transcription provider
- **THEN** transcript rows have `token_timestamps = NULL`
- **AND** the `TranscriptUpdate` event has `token_timestamps = null`

