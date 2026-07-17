## MODIFIED Requirements

### Requirement: Whisper provider stores token timestamps in the database

When Whisper is the active transcription provider, per-token timestamps SHALL be extracted from each Whisper segment (using `set_token_timestamps(true)`, which is already enabled) and serialized as a JSON array of `{word: string, start_ms: i64, end_ms: i64}` objects into the `token_timestamps` column of the `transcripts` table. This SHALL hold for every transcription save path that actually runs: the post-meeting retranscription queue path (`start_retranscription`) and the import path. The transcription result type SHALL carry an optional `token_timestamps: Option<String>` field through these live save paths.

Realtime transcription during recording is not supported — the `audio-recording-quality` requirement forbids Whisper inference and `transcript-update` emission while a recording is in progress. No `TranscriptUpdate` Tauri event is emitted; token timestamps reach the database solely via the post-meeting and import save paths.

#### Scenario: Whisper provider populates token timestamps

- **WHEN** a recording is transcribed post-meeting, or an audio file is imported, with Whisper as the active provider
- **THEN** each transcript row in the database has `token_timestamps` populated with a JSON array of word-level timing

#### Scenario: Parakeet provider leaves token timestamps null

- **WHEN** Parakeet is the active transcription provider
- **THEN** transcript rows have `token_timestamps = NULL`
