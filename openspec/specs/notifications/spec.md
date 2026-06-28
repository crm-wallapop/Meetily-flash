# Notifications — Capability Spec

> Status: **recording-lifecycle toasts implemented** (notification-actions, 2026-06-23);
> transcription-queue notifications remain an open question.
> Covers Windows system toast notifications for recording lifecycle events.
>
> **Scope note:** This spec governs OS-level toast notifications only. The `recording-state-changed`
> Tauri event (which drives in-app recording UI state) is specified in
> `openspec/specs/recording-lifecycle/spec.md`. The `transcription-queue-changed` event
> (job progress, pause/resume state) is specified in
> `openspec/specs/post-meeting-pipeline/spec.md`.

---

## Purpose

Governs OS-level system toast notifications for recording lifecycle events (recording started, stopped). All notifications are consent-gated and require OS permission. In-app recording UI state is driven by Tauri events specified in recording-lifecycle and post-meeting-pipeline.

## Requirements

### Requirement: Notification consent gate controls all notifications

The system SHALL NOT show any system notification unless both `consent_given` AND
`system_permission_granted` are `true` in the persisted notification settings.
The UI toggle that enables or disables notifications MUST set both fields, not only
the per-event preference flags.

#### Scenario: User enables notifications via the toggle
- **GIVEN** notifications were disabled (all three fields false)
- **WHEN** the user switches the Notifications toggle ON in Preferences
- **THEN** `consent_given`, `system_permission_granted`, `show_recording_started`,
  and `show_recording_stopped` are ALL set to `true` and persisted

#### Scenario: User disables notifications via the toggle
- **GIVEN** notifications were enabled (all fields true)
- **WHEN** the user switches the Notifications toggle OFF in Preferences
- **THEN** `consent_given`, `system_permission_granted`, `show_recording_started`,
  and `show_recording_stopped` are ALL set to `false` and persisted

#### Scenario: Per-event flags alone cannot enable notifications
- **GIVEN** `consent_given = false`
- **WHEN** `show_recording_started = true` is set without changing `consent_given`
- **THEN** no recording-started notification is shown

---

### Requirement: Clicking a system notification brings the app window to the foreground

When the user clicks a Meetily system notification, the app window SHALL be made
visible, unminimised, and focused so the user can review or cancel an in-progress
recording.

#### Scenario: User clicks recording-started toast while app is minimised
- **GIVEN** a recording-started notification was shown AND the app window is minimised
- **WHEN** the user clicks the notification
- **THEN** the app window is shown, unminimised, and brought to the foreground

#### Scenario: User clicks toast while app is already visible
- **GIVEN** a recording-started notification was shown AND the app window is already focused
- **WHEN** the user clicks the notification
- **THEN** the app window remains visible and focused (no visible change)

#### Scenario: Foreground intent: cancel a mis-triggered auto-recording
- **GIVEN** the auto-detect feature started a recording the user did not intend
- **WHEN** the user clicks the recording-started notification
- **THEN** the app comes to the foreground showing the countdown banner so the user
  can press Cancel before the countdown expires

---

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

---

### Requirement: Dev-build AUMID branding is populated at startup

On Windows, when running an uninstalled dev build (`tauri dev`), the app SHALL ensure the AUMID registry key (`HKCU\Software\Classes\AppUserModelId\<identifier>`) has both `DisplayName` and `IconUri` values populated before any system toast is shown. The write SHALL be idempotent (a no-op when both values are already present) and non-fatal (registry failures are logged as warnings and do not block startup).

#### Scenario: First dev-build launch brands the AUMID
- **GIVEN** a dev build launches AND the AUMID registry key exists but lacks `DisplayName` or `IconUri`
- **WHEN** startup completes
- **THEN** `DisplayName` and `IconUri` are written to the AUMID registry key
- **AND** subsequent `recording_started` toasts are displayed rather than silently dropped

#### Scenario: Already-branded AUMID is left untouched
- **GIVEN** a dev build launches AND the AUMID registry key already has both `DisplayName` and `IconUri`
- **WHEN** startup completes
- **THEN** the registry values are NOT rewritten

#### Scenario: Registry write failure does not block startup
- **GIVEN** a dev build launches AND the AUMID registry write fails
- **WHEN** startup completes
- **THEN** a warning is logged
- **AND** the app continues to start and run normally

---

### Requirement: Protocol-scheme reactivation does not allocate a secondary window

The secondary process that forwards a `meetily://` reactivation URI to the running instance SHALL complete without allocating a visible console window or secondary app window, in both debug and release builds. The single-instance forwarding SHALL produce no user-visible window artifact beyond the existing running instance being brought to the foreground.

#### Scenario: Notification action button does not flash a console in dev
- **GIVEN** the app is running via `tauri dev` AND a `recording_started` toast with an action button is shown
- **WHEN** the user clicks the action button, re-launching the app via `meetily://`
- **THEN** the running instance handles the action
- **AND** no console window is allocated or flashed by the forwarding process

#### Scenario: Release-build reactivation remains windowless
- **GIVEN** an installed release build is running AND a notification action button is clicked
- **WHEN** the `meetily://` secondary process forwards the URI
- **THEN** no console or secondary window appears, as a regression guard

---

## Open question: transcription-queue-changed notifications

> **Status: NOT YET DESIGNED** — added 2026-05-18 to track this gap.

The `post-meeting-transcription` change introduced a `transcription-queue-changed` Tauri event
carrying full queue state (job status, progress, `manual_pause_all` flag). Natural follow-on
notifications when jobs complete or fail have not yet been designed.

### Questions to resolve before implementing
- Should job-completion and job-failure notifications reuse the existing consent gate, or have
  their own per-event flags (`show_transcription_completed`, `show_transcription_failed`)?
- What text should the toasts carry (e.g. "Transcription ready — Weekly sync")?
- Should a completion notification carry an "Open" action button to navigate to the meeting?
- Should a failure notification carry a "Retry" action, or is click-to-foreground sufficient?
- Should notifications fire for every completed job, or only for the last job in a batch run?
