# Recording Lifecycle — Delta Spec

> Change: `speaker-diarization`
> Modifies: `recording-lifecycle`

---

## ADDED Requirements

### Requirement: Diarization queue phase does not delay the recording-stop status bar guarantee

The `Diarizing` queue phase SHALL run as a separate job after `background_shutdown` completes. It SHALL NOT block or delay the existing 1-second status-bar-clear guarantee of `stop_recording`: the `RecordingStatusBar` disappears within 1 second of stream release regardless of whether diarization is enabled or still queued.

#### Scenario: Diarization phase does not delay the 1-second status bar clear

- **GIVEN** a recording is active and speaker diarization is enabled
- **WHEN** `stop_recording` is invoked
- **THEN** the status bar still clears within 1 second of stream release
- **AND** the `Diarizing` queue phase runs later as a separate job, after `background_shutdown` completes, and never blocks the status bar UI

---

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
