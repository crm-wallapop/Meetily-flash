## MODIFIED Requirements

### Requirement: Recording-started notification is shown on auto-detect trigger

When the auto-detect feature leads to a recording, the system SHALL show exactly **one** actionable toast notification at **record-start time** (not at detection time). The toast SHALL carry title "Meetily" and body "Meeting detected — recording: \<resolved title\>", and SHALL render two action buttons: `[Stop recording]` and `[Continue]`. The detector's detection-time event SHALL NOT show a toast — detection surfaces only the in-app banner; the previous premature `recording_started` notification fired from `emit_detected` is removed. A manually-started recording SHALL fire an actionable toast at record-start time with body "Recording started: \<title\>" and the same two buttons. Both detector-started and manual starts converge on a single notification at record-start, so no suppression flag is required.

The buttons activate via the `meetily://` protocol scheme specified in "Toast action buttons activate via a validated meetily:// protocol scheme". The notification consent gate (consent_given AND system_permission_granted) applies as usual.

#### Scenario: Auto-detected recording surfaces an actionable started toast

- **WHEN** the auto-detect feature starts a recording
- **THEN** exactly one toast is shown with title "Meetily", body naming the resolved title, and two buttons `[Stop recording]` and `[Continue]`
- **AND** the toast provides a path to stop the recording without leaving the meeting

#### Scenario: Stop button stops and saves the recording

- **GIVEN** the actionable started toast is visible and a recording is active
- **WHEN** the user taps `[Stop recording]`
- **THEN** the recording is stopped and saved via the normal `stop_recording` path (the meeting is retained, not discarded)

#### Scenario: Continue button keeps the recording running

- **GIVEN** the actionable started toast is visible and a recording is active
- **WHEN** the user taps `[Continue]`
- **THEN** the toast is dismissed AND the recording continues uninterrupted

#### Scenario: Detection shows no toast

- **GIVEN** the detector has detected a meeting but recording has not started
- **WHEN** the detection event fires
- **THEN** no toast is shown — only the in-app banner

#### Scenario: Record-start shows exactly one toast for detector-started recordings

- **GIVEN** a detector-started recording is beginning
- **WHEN** the record-start code path runs
- **THEN** exactly one actionable toast is shown (no duplicate)

#### Scenario: Manual start fires the actionable toast at record-start

- **GIVEN** no detector-started toast is active
- **WHEN** the user manually starts a recording
- **THEN** the actionable started toast (with `[Stop recording]` and `[Continue]`) is shown once at record-start time

#### Scenario: Cancelled detector recording shows no started toast aftermath

- **GIVEN** the detector started a recording and showed the actionable started toast
- **WHEN** the recording is cancelled via `cancel_recording`
- **THEN** no recording-stopped/saved toast is shown (see the recording-stopped requirement)

---

### Requirement: Recording-stopped notification informs the user a meeting was saved

When a recording stops and is saved (manually or via auto-stop), the system SHALL show a toast confirming the meeting was saved, with title "Meetily" and body "Recording saved: \<meeting title\>", and SHALL render two action buttons: `[Continue recording]` and `[Dismiss]`. A recording cancelled via `cancel_recording` SHALL suppress this toast entirely (no "recording saved" notification for a discarded recording). The buttons activate via the `meetily://` protocol scheme. The consent gate applies.

`[Continue recording]` undoes a false stop by starting a **fresh recording** with the same title — the pipeline has no append-after-save path (see design Resolved Q1), so this is a new session, not a continuation of the saved audio. True cross-session merge of the two recordings into one meeting is a follow-up issue. `[Dismiss]` accepts the stop; the meeting stays saved and no further action is taken.

#### Scenario: Stopped recording surfaces a saved toast with buttons

- **WHEN** a recording stops and is saved
- **THEN** a toast is shown with title "Meetily", body naming the meeting title, and buttons `[Continue recording]` and `[Dismiss]`

#### Scenario: Continue recording starts a fresh capture

- **GIVEN** the stopped toast is visible
- **WHEN** the user taps `[Continue recording]`
- **THEN** a new recording starts with the same title (the stopped meeting stays saved; the two are separate sessions pending a future merge feature)

#### Scenario: Dismiss accepts the stop

- **GIVEN** the stopped toast is visible
- **WHEN** the user taps `[Dismiss]`
- **THEN** the toast is dismissed AND the meeting remains saved AND no further action is taken

#### Scenario: Cancelled recording suppresses the stopped toast

- **WHEN** a recording is cancelled via `cancel_recording` (e.g. from the auto-detect countdown)
- **THEN** no recording-stopped/saved toast is shown

---

## ADDED Requirements

### Requirement: Toast action buttons activate via a validated meetily:// protocol scheme

Action buttons on recording-lifecycle toasts SHALL use `activationType="protocol"` with a `meetily://recording/<action>` URI. A custom `meetily://` scheme SHALL be registered with the OS (via `tauri-plugin-deep-link`) and a re-activation SHALL be delivered to the running single instance via `tauri-plugin-single-instance` — a toast-button re-launch passes the URI as argv, single-instance forwards it to the already-running instance, and the launcher process exits without spawning a second instance or window. A dispatch use case SHALL accept only URIs whose scheme is `meetily`, host is `recording`, and path action is `stop` or `continue`; every other host, action, query parameter, or malformed URI SHALL be rejected (logged, no command invoked). No untrusted URI component SHALL reach SQL, the filesystem, or an LLM.

Activation SHALL be safe under abnormal conditions: a button tapped while the app is not running (cold start) SHALL launch the app but perform no recording action and log the event; a repeated `stop` when no recording is active SHALL be an idempotent no-op; a `continue` when a recording is already active SHALL be a no-op.

#### Scenario: Valid stop URI stops and saves

- **GIVEN** a recording is active
- **WHEN** the deep-link event delivers `meetily://recording/stop`
- **THEN** the recording is stopped and saved via the normal stop path

#### Scenario: Valid continue URI dismisses / resumes

- **WHEN** the deep-link event delivers `meetily://recording/continue`
- **THEN** the corresponding continue behavior runs (dismiss for an active toast, start a fresh recording for a stopped toast)

#### Scenario: Unknown action is rejected

- **WHEN** the deep-link event delivers `meetily://recording/pause` (or any action other than `stop`/`continue`)
- **THEN** the URI is rejected AND no command is invoked AND the rejection is logged

#### Scenario: Wrong scheme or host is rejected

- **WHEN** the deep-link event delivers `https://recording/stop` or `meetily://malicious/stop`
- **THEN** the URI is rejected AND no command is invoked

#### Scenario: Unknown query parameters are ignored

- **WHEN** the deep-link event delivers `meetily://recording/stop?extra=evil`
- **THEN** the stop action runs AND the unknown parameter is ignored (no untrusted value reaches a command)

#### Scenario: Cold-start activation is a no-op

- **GIVEN** the app is not running
- **WHEN** the user taps a toast button and the app is cold-started with a `meetily://recording/*` URI
- **THEN** the app launches AND performs no recording action AND logs the cold-start event

#### Scenario: Double-tap is idempotent

- **GIVEN** a recording is not active
- **WHEN** the deep-link event delivers `meetily://recording/stop` twice in rapid succession
- **THEN** no error is raised AND no spurious stop occurs (idempotent no-op)

#### Scenario: Warm activation forwards to the running instance

- **GIVEN** the app is already running (a recording may or may not be active)
- **WHEN** a toast button re-activates the app via a `meetily://recording/*` URI
- **THEN** the URI is delivered to the *already-running* instance AND no second app instance or window is spawned (the launcher forwards its argv and exits) AND the running instance logs the dispatch
