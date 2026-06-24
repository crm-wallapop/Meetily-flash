## MODIFIED Requirements

### Requirement: Status bar clears within 1 s of stop command

When the user invokes `stop_recording`, the `RecordingStatusBar` SHALL stop
showing the "Recording" state (i.e., `isRecording` becomes `false` in the
frontend) no later than **1 second** after the audio streams are released. The
remaining shutdown work — transcription queue drain, Whisper model unload,
incremental-saver finalization — runs in the background as a `Saving` phase
and does NOT block the UI update.

The recording lifecycle is modeled as three phases: `Idle`, `Recording`,
`Saving`. The transition from `Recording` to `Saving` happens on the same
synchronous path as stream release; the transition from `Saving` to `Idle`
happens after background shutdown completes.

#### Scenario: Stop with large transcription backlog

- **GIVEN** a recording is active AND 10 audio chunks are queued for transcription
- **WHEN** `stop_recording` is invoked
- **THEN** the audio streams are released within 1 second
- **AND** the phase transitions from `Recording` to `Saving` within 1 second
- **AND** `isRecording` in the frontend becomes `false` within 1 second
- **AND** transcription continues in the background under the `Saving` phase
- **AND** the recorded file is saved after background work completes
- **AND** the phase transitions from `Saving` to `Idle` once background work completes

#### Scenario: Stop with empty transcription queue

- **GIVEN** a recording is active AND no chunks are queued
- **WHEN** `stop_recording` is invoked
- **THEN** the phase transitions `Recording` → `Saving` → `Idle` within 1 second
- **AND** the file is saved normally

#### Scenario: UI state is unambiguous immediately after Stop

- **GIVEN** a recording is active
- **WHEN** `stop_recording` is invoked
- **THEN** within 1 second the status bar leaves the "Recording" appearance
- **AND** the Stop button is no longer rendered (hidden, not just disabled)
- **AND** if background shutdown is still in progress, a visually distinct
  "Saving…" indicator is shown (gray spinner, no recording-red dot)
- **AND** the UI never simultaneously presents "Stop is disabled / processing"
  and "Recording is active"

## ADDED Requirements

### Requirement: Stop command is idempotent

A duplicate `stop_recording` while the first is still in progress SHALL be a no-op.
The audio streams, transcription task, and file saver are owned by exactly
one shutdown sequence; a concurrent second call observes the phase has
already transitioned out of `Recording` and returns early.

#### Scenario: User double-presses the Stop button

- **WHEN** the user presses Stop AND immediately presses Stop again before
  the status bar has cleared
- **THEN** the second press is silently ignored (frontend button is hidden
  after the first press; backend phase check rejects a duplicate command)
- **AND** the recording is stopped exactly once with no partial cleanup

#### Scenario: stop_recording invoked while phase is Saving

- **GIVEN** the phase is `Saving` (background shutdown running)
- **WHEN** `stop_recording` is invoked again (e.g., via a stale code path)
- **THEN** the command returns Ok without side effects
- **AND** the running background task is not disturbed

#### Scenario: stop_recording invoked while phase is Idle

- **GIVEN** the phase is `Idle`
- **WHEN** `stop_recording` is invoked
- **THEN** the command returns Ok without side effects

### Requirement: Audio capture halts within 1 second of stop command

No audio samples recorded after the CPAL streams are released SHALL appear in
the saved file. The incremental saver flushes its in-memory buffer before
finalizing, but the flush boundary is the moment of stream release — which
happens on the synchronous path of `stop_recording` before the phase
transitions to `Saving`.

#### Scenario: User speaks immediately after pressing Stop

- **GIVEN** the user presses Stop at time T
- **WHEN** the user speaks at time T + 2 s (after streams are released)
- **THEN** that speech is NOT present in the saved audio file

#### Scenario: User speaks in the 1-second window while streams are closing

- **GIVEN** the user presses Stop at time T
- **WHEN** the user speaks at time T + 0.5 s (streams may still be draining)
- **THEN** whether this audio is captured is implementation-defined, but the
  duration of the capture window SHALL NOT exceed 1 second from the stop command

### Requirement: Lifecycle phase is observable from the frontend

The backend SHALL expose the current recording phase (`Idle`, `Recording`,
`Saving`) to the frontend via:
- a Tauri `recording-state-changed` event emitted on every phase transition,
  carrying `{ phase: "Idle" | "Recording" | "Saving" }`;
- the existing `get_recording_state` Tauri command, augmented to return the
  phase alongside its current fields.

The frontend `RecordingStateContext` SHALL expose `isRecording` (true only
when phase is `Recording`) and `isSaving` (true only when phase is `Saving`).

#### Scenario: Phase transitions emit events in order

- **GIVEN** the phase is `Idle`
- **WHEN** a recording starts and later stops with a non-empty transcription queue
- **THEN** the frontend receives `recording-state-changed` events in this
  order: `Recording`, `Saving`, `Idle`
- **AND** no transition emits more than once for a given recording

#### Scenario: Polling fallback agrees with events

- **WHEN** `get_recording_state` is invoked at any moment
- **THEN** the returned phase matches the phase observed by the last
  `recording-state-changed` event (or the next one, if a transition is
  happening concurrently)

### Requirement: Saving phase rejects new recordings politely

While the phase is `Saving`, a call to `start_recording` SHALL return an
error indicating that a previous recording is finalizing. The user is shown
a clear message; no partial recording is created.

#### Scenario: User tries to start a new recording during Saving

- **GIVEN** the phase is `Saving`
- **WHEN** `start_recording` is invoked
- **THEN** the command returns an error `"a previous recording is still
  finalizing"` (or equivalent localized text)
- **AND** no streams are opened
- **AND** no meeting row is written

### Requirement: Background shutdown failures surface as toasts, not stuck UI

If background shutdown work fails after the phase has reached `Saving`, the system SHALL still reach `Idle` and surface the error — it MUST never leave the UI stuck in `Saving`.
- still transition the phase to `Idle` (never leave the UI stuck in `Saving`);
- emit a `recording-save-failed` Tauri event carrying an error message;
- log the failure with sufficient context for the startup GC pass to reconcile
  any orphan state.

#### Scenario: Whisper unload fails during background shutdown

- **GIVEN** the phase is `Saving`
- **WHEN** the Whisper model unload step errors
- **THEN** the remaining shutdown steps still run
- **AND** the phase transitions to `Idle`
- **AND** a `recording-save-failed` event is emitted
- **AND** the frontend shows a toast describing the failure
- **AND** the user can start a new recording

#### Scenario: File save fails during background shutdown

- **GIVEN** the phase is `Saving`
- **WHEN** the final audio file save step errors (e.g., disk full)
- **THEN** the phase transitions to `Idle`
- **AND** a `recording-save-failed` event is emitted with the underlying error
- **AND** the partial state is reconciled by the startup GC pass on next launch
