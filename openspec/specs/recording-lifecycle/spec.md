# Recording Lifecycle — Capability Spec

> Status: **updated 2026-05-18** — corrected background-work description to match
> post-meeting-transcription implementation; added cross-reference to queue enqueue step.

---

## Requirement: Status bar clears within 1 s of stop command

When the user invokes `stop_recording`, the `RecordingStatusBar` SHALL disappear
(i.e., `isRecording` becomes `false` in the frontend) no later than **1 second**
after the audio streams are released. The remaining shutdown work — MP4 flush and
finalization, SQLite row creation, phase reset, scheduler gate release — runs in the
background (`background_shutdown` task) and does NOT block the UI update.

### Scenario: UI state is unambiguous immediately after Stop

- **GIVEN** a recording is active
- **WHEN** `stop_recording` is invoked
- **THEN** within 1 second the status bar disappears (or transitions to a clear
  "Saving…" state that does NOT resemble the active-recording state)
- **AND** the Stop button is disabled (already the case via `isStopping`)
- **AND** the disabled Stop button and the status label convey the SAME message:
  recording has ended, background work is in progress

### Scenario: Stop with a background_shutdown task in flight

- **GIVEN** a recording is active
- **WHEN** `stop_recording` is invoked
- **THEN** the audio streams are released within 1 second
- **AND** the status bar clears within 1 second of stream release
- **AND** `background_shutdown` completes: MP4 flush → SQLite save → PhaseGuard reset (Saving → Idle) → scheduler gate release (`set_recording_gate false`) → `queue.resume_all()`
- **AND** after `background_shutdown` the frontend (`useRecordingStop.ts`) enqueues a transcription job via `enqueue_transcription_job(meetingId, audioPath)` — this step occurs outside the Tauri phase boundary, after the meeting row UUID is returned by `saveMeeting()`

> **Cross-reference:** The transcription job enqueue step is part of the stop lifecycle but is not governed by `RecordingPhase`. See `openspec/specs/post-meeting-pipeline/spec.md` for the queue contract.

---

## Requirement: Stop command is idempotent

A second invocation of `stop_recording` while the first is still in progress
SHALL be a no-op: the audio streams, transcription task, and file saver are owned
by exactly one shutdown sequence; a concurrent second call finds them already
released.

### Scenario: User double-presses the Stop button

- **WHEN** the user presses Stop AND immediately presses Stop again before the
  status bar has cleared
- **THEN** the second press is silently ignored (frontend `isStopping` guard OR
  backend `IS_RECORDING` check)
- **AND** the recording is stopped exactly once with no partial cleanup

---

## Requirement: Audio capture halts within 1 second of stop command

No audio samples recorded after the CPAL streams are released SHALL appear in
the saved file. The incremental saver flushes its in-memory buffer before
finalizing, but the flush boundary is the moment of stream release.

### Scenario: User speaks immediately after pressing Stop

- **GIVEN** the user presses Stop at time T
- **WHEN** the user speaks at time T + 2 s (after streams are released)
- **THEN** that speech is NOT present in the saved audio file

### Scenario: User speaks in the 1-second window while streams are closing

- **GIVEN** the user presses Stop at time T
- **WHEN** the user speaks at time T + 0.5 s (streams may still be draining)
- **THEN** whether this audio is captured is implementation-defined, but the
  duration of the capture window SHALL NOT exceed 1 second from the stop command
