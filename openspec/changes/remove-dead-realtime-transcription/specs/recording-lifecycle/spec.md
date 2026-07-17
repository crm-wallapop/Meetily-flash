## REMOVED Requirements

### Requirement: TranscriptSegment and TranscriptUpdate carry an optional speaker field

Removed because the `TranscriptUpdate` realtime Tauri event is part of the unsupported realtime transcription path and is never emitted (transcription runs post-meeting). The live parts — `TranscriptSegment.speaker` populated after the `Diarizing` phase, and the `diarization-complete` event — are preserved under the replacement requirement below. The `token_timestamps`-on-`TranscriptUpdate` clause is dropped; token storage is governed by `whisper-model-selection`.

## ADDED Requirements

### Requirement: TranscriptSegment carries an optional speaker field

`TranscriptSegment` SHALL include an optional `speaker: Option<String>` field. The field SHALL be `None` until the `Diarizing` queue phase completes (no speaker labels are available before diarization runs), and SHALL be populated on the transcript rows after diarization. Speaker labels are not delivered via any realtime event during recording; transcription and diarization both run post-meeting (see `audio-recording-quality`).

#### Scenario: Transcript speaker populated after diarization

- **WHEN** the `Diarizing` phase completes for a meeting
- **THEN** the transcript rows in the database have `speaker` set to the assigned label
- **AND** the frontend receives a `diarization-complete` event with `{meeting_id, speakers: [{label, name, color}]}`
