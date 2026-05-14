# Recording Lifecycle — Capability Spec

> Status: **updated 2026-05-13** — controlled repro confirmed; UX split
> between disabled Stop button and still-active recording state documented.

---

## Requirement: Status bar clears within 1 s of stop command

When the user invokes `stop_recording`, the `RecordingStatusBar` SHALL disappear
(i.e., `isRecording` becomes `false` in the frontend) no later than **1 second**
after the audio streams are released. The remaining shutdown work — transcription
queue drain, Whisper model unload, incremental-saver finalization — runs in the
background and does NOT block the UI update.

**Root cause of current bug (logged 2026-05-13):** `IS_RECORDING` is only set to
`false` at the very end of `stop_recording` in `recording_commands.rs`, after all
10 transcription chunks are drained and the file is saved. In the smoke test this
produced a **2 m 16 s** window where the status bar showed "Recording" even though
the CPAL streams had already been released.

**Controlled repro (2026-05-13, second recording):** Auto-detect started a recording;
user left the Meet call; stop-prompt banner appeared (after the 10 s `meeting-ended`
debounce); user pressed Stop. `metadata.json` confirmed:
- `created_at` → `completed_at` gap: **2 m 15 s** (consistent with first test)
- `duration_seconds: 1.93` — streams released and audio capture halted within
  ~2 s of the Stop press; only the UI signal (`IS_RECORDING`) was delayed.

**UX symptom (confirmed):** After pressing Stop, the frontend enters a split state:
- The sidebar Stop button is immediately **disabled** (the `isStopping` guard in
  `RecordingControls.tsx` fires on first press to prevent double-clicks).
- The `RecordingStatusBar` continues to show "Recording" and a "Processing…" spinner,
  because `isRecording` in `RecordingStateContext` is driven by `IS_RECORDING` — which
  stays `true` for the full ~2-minute shutdown sequence.

The user cannot tell whether the recording is still capturing audio (it is not) or
whether their Stop press was registered (it was). The disabled button strongly implies
"registered" while the status bar implies "still running" — these signals contradict
each other.

### Scenario: UI state is unambiguous immediately after Stop

- **GIVEN** a recording is active
- **WHEN** `stop_recording` is invoked
- **THEN** within 1 second the status bar disappears (or transitions to a clear
  "Saving…" state that does NOT resemble the active-recording state)
- **AND** the Stop button is disabled (already the case via `isStopping`)
- **AND** the disabled Stop button and the status label convey the SAME message:
  recording has ended, background work is in progress

### Scenario: Stop with large transcription backlog

- **GIVEN** a recording is active AND 10 audio chunks are queued for transcription
- **WHEN** `stop_recording` is invoked
- **THEN** the audio streams are released within 1 second
- **AND** the status bar clears within 1 second of stream release
- **AND** transcription continues in the background
- **AND** the recorded file is saved after background work completes

### Scenario: Stop with empty transcription queue

- **GIVEN** a recording is active AND no chunks are queued
- **WHEN** `stop_recording` is invoked
- **THEN** the status bar clears within 1 second
- **AND** the file is saved normally

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
