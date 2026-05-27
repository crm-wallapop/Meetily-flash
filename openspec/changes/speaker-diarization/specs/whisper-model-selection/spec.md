# Whisper Model Selection — Delta Spec

> Change: `speaker-diarization`
> Modifies: `whisper-model-selection`

---

## ADDED Requirements

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
