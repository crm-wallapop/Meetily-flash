## ADDED Requirements

### Requirement: Detect active Google Meet calls on Windows
On Windows, the system SHALL detect that the user is in an active Google Meet call by simultaneously observing two signals: (1) a top-level window owned by a process in the browser allowlist (`chrome.exe`, `msedge.exe`, `firefox.exe`) with title matching `* - Google Meet` (suffix-anchored), AND (2) the same browser process has at least one active UDP or TCP socket whose remote IP falls within the hardcoded Google media CIDR list. Detection is triggered only on the transition where the network connection first appears (i.e., it was absent in the previous poll).

#### Scenario: User clicks "Join now" on a Meet call
- **WHEN** a Chrome window with title `Weekly sync - Google Meet` is open AND chrome.exe newly establishes a UDP connection to a Google media-server IP
- **THEN** the detector transitions from `Idle` to `InCall` and emits a `meeting-detected` Tauri event

#### Scenario: Meet tab open but user has not joined
- **WHEN** a Chrome window with title `Weekly sync - Google Meet` is open BUT chrome.exe has no active connection to any Google media IP
- **THEN** the detector remains in `Idle` and emits no event

#### Scenario: User joins muted in a large meeting
- **WHEN** the user joins a Meet call with mic muted from the start AND no participant has yet spoken
- **THEN** the detector still fires because the WebRTC connection is established at join regardless of audio state — recording begins; silent audio is captured and compressed by VAD downstream

#### Scenario: User is the only speaker
- **WHEN** the user joins a Meet call AND speaks before any other participant
- **THEN** the detector fires on the connection signal — render-side audio is not required for detection

#### Scenario: Spotify desktop app is playing music while a Meet tab is open
- **WHEN** Spotify desktop has its own audio session (not in browser) AND a Meet tab is open in Chrome BUT chrome.exe has no Meet connection
- **THEN** the detector remains in `Idle`; Spotify's audio is irrelevant because it belongs to a non-browser process

#### Scenario: Spotify playing in browser, dictation tool active, Meet tab open
- **WHEN** open.spotify.com is playing in a browser tab AND a browser-based dictation tool is using the mic AND a Meet tab is also open BUT chrome.exe has no connection to a Google media IP
- **THEN** the detector remains in `Idle` — the Meet WebRTC signal is the discriminator

#### Scenario: Discord PWA call in the same browser instance, unrelated Meet tab open
- **WHEN** a Discord PWA call is active in the same Chrome process AND a Meet tab is open AND chrome.exe has WebRTC connections to Discord servers BUT none to Google media IPs
- **THEN** the detector remains in `Idle`

### Requirement: Detect when an active call ends
The system SHALL transition from `InCall` to `Idle` when the Meet WebRTC connection signal becomes false and remains false for at least 10 continuous seconds. On the transition, a `meeting-ended` Tauri event is emitted.

#### Scenario: User leaves the call
- **WHEN** the detector is in `InCall` AND chrome.exe's connection to Google media IPs becomes absent AND remains absent for 10 seconds
- **THEN** the detector transitions to `Idle` and emits a `meeting-ended` event

#### Scenario: Transient network drop
- **WHEN** the detector is in `InCall` AND the network connection drops for less than 10 seconds before reappearing
- **THEN** the detector remains in `InCall` and emits no event

### Requirement: Conservative app-start state
The detector SHALL NOT fire `meeting-detected` for connections that were already present at the time the detector was first launched. Only connections appearing during the detector's observation window trigger the event.

#### Scenario: Meetily launches while user is already in a Meet call
- **WHEN** the user is in a Meet call AND launches Meetily AND on the first poll the Meet WebRTC connection is already present
- **THEN** the detector establishes that connection as "pre-existing" and does NOT fire `meeting-detected`; the user must start recording manually for this call

#### Scenario: Connection drops and reappears after app launch
- **WHEN** Meetily launches with no Meet connection present AND later the user joins a Meet call AND the connection appears
- **THEN** the detector fires `meeting-detected` normally

### Requirement: Auto-start recording on call detection
On receiving `meeting-detected`, the frontend SHALL immediately start a recording AND display a countdown banner with an editable title field, a dropdown of all currently-enumerated Meet windows, and a 10-second cancel window.

#### Scenario: Detection fires, user accepts default
- **WHEN** a `meeting-detected` event is received AND no recording is currently active
- **THEN** the frontend invokes `start_recording_with_devices_and_meeting` with the resolved default title
- **AND** displays a banner reading "Google Meet call detected — recording in 10s" with the editable title field
- **AND** after 10 seconds the banner dismisses, the title is committed, and the recording continues normally

#### Scenario: User edits the title during countdown
- **WHEN** the countdown banner is showing AND the user types in the title field or selects from the dropdown
- **THEN** the displayed title updates; on confirm or timeout, the edited title is written to the meeting row

#### Scenario: User confirms immediately
- **WHEN** the countdown banner is showing AND the user clicks "Start now"
- **THEN** the banner dismisses immediately, the current title (default or edited) is committed, and the recording continues

#### Scenario: User cancels during countdown
- **WHEN** the countdown banner is showing AND the user clicks "Cancel"
- **THEN** the frontend invokes `cancel_recording` AND the audio file is deleted AND the meeting database row is deleted AND no "recording saved" notification is shown

#### Scenario: Recording already active when detection fires
- **WHEN** a `meeting-detected` event is received AND a recording is already active
- **THEN** the event is ignored; no banner is shown; no new recording is started

### Requirement: Title resolution provides a smart default
The default title shown in the auto-start banner SHALL be resolved in this priority order: (1) foreground window at detection-transition moment if it matches the Meet pattern; (2) the most recently focused Meet window from the focus tracker (last 10 minutes); (3) the first Meet-titled window returned by `EnumWindows`; (4) a generic timestamp `Meeting <YYYY-MM-DD HH:MM>`.

#### Scenario: User clicks Join with Meet tab focused
- **WHEN** the detection transition fires AND `GetForegroundWindow()` returns a window whose title matches the Meet pattern
- **THEN** that window's title is used as the default

#### Scenario: User joins Meet, immediately switches to another window
- **WHEN** the detection transition fires AND foreground is no longer Meet BUT the focus tracker has a Meet window focused within the last 10 minutes
- **THEN** that recent Meet title is used as the default

#### Scenario: No Meet window has been focused recently
- **WHEN** no foreground or recent-focus Meet match exists AND `EnumWindows` returns at least one Meet-titled window
- **THEN** the first such window's title is used

#### Scenario: No Meet windows enumerable
- **WHEN** no Meet-titled window can be found at all
- **THEN** the default is `Meeting <YYYY-MM-DD HH:MM>` using the current local time

#### Scenario: PWA window is the source
- **WHEN** the user is using the Meet PWA AND the PWA's window matches the Meet title pattern
- **THEN** the PWA window participates in title resolution identically to a browser tab window

### Requirement: Cancel-suppression scope is per-call
After a user cancels an auto-start banner, the system SHALL NOT re-prompt for the same call. The suppression flag resets when the state machine transitions back to `Idle`.

#### Scenario: User cancels, network blips briefly, connection returns
- **WHEN** the user has cancelled the current call's auto-start AND the network connection drops for fewer than 10s and reappears (no Idle transition occurred)
- **THEN** the detector does NOT re-fire `meeting-detected` for this call

#### Scenario: User cancels, call truly ends, new call begins
- **WHEN** the user cancelled an earlier call AND the connection dropped for >10s (Idle transition) AND a new connection appears
- **THEN** the detector fires `meeting-detected` for the new call; the cancel-suppression flag has been reset

### Requirement: Auto-stop recording on call end
On receiving `meeting-ended` while a detector-started recording is active, the frontend SHALL display a stop-prompt banner with a 10-second confirmation window. If the signals re-engage during the prompt, the prompt is dismissed silently.

#### Scenario: Call ends, user does nothing
- **WHEN** a `meeting-ended` event is received AND a detector-started recording is active
- **THEN** the frontend displays a banner reading "Call ended — stop recording in 10s [Stop now] [Keep recording]"
- **AND** after 10 seconds the recording is stopped via the normal `stop_recording` path

#### Scenario: User stops immediately
- **WHEN** the stop-prompt banner is showing AND the user clicks "Stop now"
- **THEN** `stop_recording` is invoked immediately

#### Scenario: User extends the recording
- **WHEN** the stop-prompt banner is showing AND the user clicks "Keep recording"
- **THEN** the banner dismisses AND the recording continues until manually stopped AND no further auto-stop prompts fire for this recording

#### Scenario: Connection reappears during stop-prompt
- **WHEN** the stop-prompt banner is showing AND a `meeting-detected` event fires (signals re-engaged within 10s of meeting-ended)
- **THEN** the stop-prompt dismisses silently AND the recording continues without interruption

### Requirement: Concurrent-action precedence
Manual user actions SHALL take precedence over automated detector actions.

#### Scenario: Manual Start during auto-start countdown
- **WHEN** the auto-start countdown banner is showing AND the user clicks the manual "Start Recording" button
- **THEN** the auto-recording is cancelled silently (its audio file and DB row are deleted via `cancel_recording`) AND a fresh manual recording is started

#### Scenario: Auto-detect fires while manual recording is in progress
- **WHEN** a `meeting-detected` event is received AND a manual recording is already active
- **THEN** the event is ignored; the manual recording continues; no banner is shown

### Requirement: cancel_recording Tauri command performs atomic cleanup
The system SHALL expose a `cancel_recording(meeting_id)` Tauri command that stops the audio capture, deletes the audio file from disk, and removes the meeting database row.

#### Scenario: Successful cancel
- **WHEN** `cancel_recording` is invoked with a valid in-progress `meeting_id`
- **THEN** the audio capture is stopped AND the audio file is removed AND any persisted meeting row is removed (note: in the auto-detect countdown flow the row has not yet been written, so the DB step is a no-op) AND the command returns Ok

#### Scenario: Cleanup partial failure
- **WHEN** `cancel_recording` is invoked AND the audio capture stops successfully BUT file deletion fails
- **THEN** the failure is logged with the meeting_id and file path AND the command returns an error AND any partial state is left for the startup GC pass to reconcile

### Requirement: Startup GC pass reconciles orphan state
On app startup, before the detector is spawned, the system SHALL run a synchronous garbage-collection pass that removes orphan DB rows and orphan audio files.

#### Scenario: DB row references missing audio file
- **WHEN** a meeting row's audio file path is set AND that file does not exist on disk
- **THEN** the GC pass deletes the meeting row AND logs the deletion with the meeting_id and the missing path

#### Scenario: Audio file is not referenced by any meeting row
- **WHEN** a file in the recordings directory matches the expected audio extension AND no meeting row references its absolute path
- **THEN** the GC pass deletes the file AND logs the deletion with the file path

#### Scenario: Valid meeting with valid file
- **WHEN** a meeting row points to an audio file that exists on disk
- **THEN** the GC pass touches neither

### Requirement: Auto-detection setting controls detector lifecycle
The user SHALL be able to enable or disable auto-detection via a single setting `auto_detect_meetings` (default: `true`). The setting takes effect after an app restart in v1.

#### Scenario: Setting is enabled on startup
- **WHEN** the app launches AND `auto_detect_meetings` is `true`
- **THEN** the meeting detector polling loop is started

#### Scenario: Setting is disabled on startup
- **WHEN** the app launches AND `auto_detect_meetings` is `false`
- **THEN** no polling loop is started AND no detection events are emitted regardless of system state

#### Scenario: User toggles the setting at runtime
- **WHEN** the user changes `auto_detect_meetings` while the app is running
- **THEN** the setting is persisted AND the user is informed inline that the change takes effect after restart
