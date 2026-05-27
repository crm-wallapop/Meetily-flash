# Recording Lifecycle — Delta Spec

> Change: `speaker-diarization`
> Modifies: `recording-lifecycle`

---

## MODIFIED Requirements

### Requirement: Status bar clears within 1 s of stop command

When the user invokes `stop_recording`, the `RecordingStatusBar` SHALL disappear (i.e., `isRecording` becomes `false` in the frontend) no later than **1 second** after the audio streams are released. The remaining shutdown work — MP4 flush and finalization, SQLite row creation, phase reset, scheduler gate release — runs in the background (`background_shutdown` task) and does NOT block the UI update.

*(No behavior change — the existing requirement text is unchanged. The diarization phase runs as a separate queue job after `background_shutdown` completes and does not affect the 1-second status bar guarantee.)*

---

## ADDED Requirements

### Requirement: TranscriptSegment and TranscriptUpdate carry an optional speaker field

`TranscriptSegment` (Rust) and `TranscriptUpdate` (Tauri event) SHALL include an optional `speaker: Option<String>` field. The field SHALL be `None` during recording (no speaker labels available in offline-only mode) and SHALL be populated after the `Diarizing` queue phase completes.

`TranscriptUpdate` SHALL also include an optional `token_timestamps: Option<String>` field containing a JSON array of `{word: string, start_ms: i64, end_ms: i64}` objects, populated when the transcription provider supports token-level timestamps (Whisper). The field SHALL be `None` for providers that do not support token timestamps (Parakeet).

#### Scenario: TranscriptUpdate during recording has no speaker

- **WHEN** a `TranscriptUpdate` event is emitted during recording
- **THEN** `speaker` is `None`
- **AND** `token_timestamps` is populated if the Whisper provider is active

#### Scenario: TranscriptUpdate speaker populated after diarization

- **WHEN** the `Diarizing` phase completes for a meeting
- **THEN** the transcript rows in the database have `speaker` set to the assigned label
- **AND** the frontend receives a `diarization-complete` event with `{meeting_id, speakers: [{label, name, color}]}`

---

### Requirement: `diarization-complete` event updates frontend speaker state

After the `Diarizing` phase completes, the Rust side SHALL emit a `diarization-complete` Tauri event with the meeting ID and an array of speaker assignments (cluster label, resolved name, color). The frontend SHALL update the transcript view with speaker badges for the meeting.

#### Scenario: Frontend receives diarization-complete

- **WHEN** the `Diarizing` phase completes for meeting `M`
- **THEN** a `diarization-complete` event is emitted with `{meeting_id: "M", speakers: [{label: "Speaker 1", name: "Alice", color: "#0EA5E9"}, {label: "Speaker 2", name: null, color: "#F97316"}]}`
- **AND** the frontend updates the meeting's transcript view with speaker badges
