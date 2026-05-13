# Notifications — Capability Spec

> Status: **initial** — requirements captured, not yet fully implemented.
> Covers Windows system toast notifications for recording lifecycle events.

---

## Requirement: Notification consent gate controls all notifications

The system SHALL NOT show any system notification unless both `consent_given` AND
`system_permission_granted` are `true` in the persisted notification settings.
The UI toggle that enables or disables notifications MUST set both fields, not only
the per-event preference flags.

### Scenario: User enables notifications via the toggle
- **GIVEN** notifications were disabled (all three fields false)
- **WHEN** the user switches the Notifications toggle ON in Preferences
- **THEN** `consent_given`, `system_permission_granted`, `show_recording_started`,
  and `show_recording_stopped` are ALL set to `true` and persisted

### Scenario: User disables notifications via the toggle
- **GIVEN** notifications were enabled (all fields true)
- **WHEN** the user switches the Notifications toggle OFF in Preferences
- **THEN** `consent_given`, `system_permission_granted`, `show_recording_started`,
  and `show_recording_stopped` are ALL set to `false` and persisted

### Scenario: Per-event flags alone cannot enable notifications
- **GIVEN** `consent_given = false`
- **WHEN** `show_recording_started = true` is set without changing `consent_given`
- **THEN** no recording-started notification is shown

---

## Requirement: Clicking a system notification brings the app window to the foreground

When the user clicks a Meetily system notification, the app window SHALL be made
visible, unminimised, and focused so the user can review or cancel an in-progress
recording.

### Scenario: User clicks recording-started toast while app is minimised
- **GIVEN** a recording-started notification was shown AND the app window is minimised
- **WHEN** the user clicks the notification
- **THEN** the app window is shown, unminimised, and brought to the foreground

### Scenario: User clicks toast while app is already visible
- **GIVEN** a recording-started notification was shown AND the app window is already focused
- **WHEN** the user clicks the notification
- **THEN** the app window remains visible and focused (no visible change)

### Scenario: Foreground intent: cancel a mis-triggered auto-recording
- **GIVEN** the auto-detect feature started a recording the user did not intend
- **WHEN** the user clicks the recording-started notification
- **THEN** the app comes to the foreground showing the countdown banner so the user
  can press Cancel before the countdown expires

---

## Requirement: Recording-started notification is shown on auto-detect trigger

> **Status: NOT YET IMPLEMENTED** — to be designed.

When the auto-detect feature starts a recording, the system SHALL show a recording-started
notification that informs the user a recording has begun and gives them a path to cancel it.

### Open questions (must be resolved before implementing)
- What text / title / body should the toast carry?
- Should it include action buttons (e.g. "Cancel recording") directly in the toast, or
  rely on click-to-foreground + the countdown banner UI?
- Windows toast action buttons require `tauri-plugin-notification` action registrations —
  are we using those, or is click-to-foreground sufficient?
- Should the notification be non-dismissible (persistent) for the duration of the countdown?

---

## Requirement: Recording-stopped notification informs the user a meeting was saved

> **Status: NOT YET IMPLEMENTED** — to be designed.

When a recording stops (manually or via auto-stop), the system SHALL show a notification
confirming the meeting was saved with the meeting title.

### Open questions
- Should it include an "Open" action button to navigate to the meeting details view?
- Should cancelled recordings (via `cancel_recording`) suppress this notification?
  (Current intent: yes — `cancel_recording` must not show a "recording saved" notification.)
