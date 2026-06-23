# Meeting Auto-Detect — Capability Spec

## Purpose

Governs automatic detection of active Google Meet calls on Windows (window-title, TCP, and WASAPI signals), auto-start and auto-stop of recordings, smart title resolution, per-call cancel suppression, transcription-queue gating during calls, and the startup GC pass.

## Requirements

### Requirement: Detect active Google Meet calls on Windows
On Windows, the system SHALL detect that the user is in an active Google Meet call by polling three signals: (1) a top-level window owned by a process in the browser allowlist (`chrome.exe`, `msedge.exe`, `firefox.exe`, `brave.exe`) with title matching the Meet pattern, (2) that browser process has an active TCP connection to a Google media/signalling IP (`has_meet_connection()` — TCP CIDR check via `GetExtendedTcpTable`), AND (3) that browser process holds an `AudioSessionStateActive` WASAPI capture session (`has_browser_capture_session()`). For calls using a TCP TURN relay, signal (2) is replaced by `has_turn_connection()` (TCP TURN CIDR check) which is sufficient alone. Detection fires only for connections that first appear after the detector starts (pre-existing connections are ignored — see Conservative app-start state).

#### Scenario: User clicks "Join now" on a Meet call
- **WHEN** a Chrome window with title `Weekly sync - Google Meet` is open AND chrome.exe newly establishes a TCP connection to a Google media-server IP AND holds an active WASAPI capture session
- **THEN** the detector transitions from `Idle` to `InCall` and emits a `meeting-detected` Tauri event

#### Scenario: Meet tab open but user has not joined
- **WHEN** a Chrome window with title `Weekly sync - Google Meet` is open BUT chrome.exe has no active connection to any Google media IP
- **THEN** the detector remains in `Idle` and emits no event

#### Scenario: User joins muted in a large meeting
- **WHEN** the user joins a Meet call with mic muted from the start AND no participant has yet spoken
- **THEN** the detector still fires because the WebRTC connection is established at join regardless of audio state — recording begins; silent audio is captured and written to the MP4. Post-meeting retranscription will process the file; VAD operates only during retranscription.

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

The system SHALL transition from `InCall` to `Idle` when the connection signal becomes false and remains false for the debounce window. The debounce window and connection signal are derived differently by transport path:

- **TCP TURN path** (TURN relay was observed): debounce **4 s**; signal is `has_turn_connection()`. It drops to `false` within ~1 s of the user leaving the call. The lobby page's HTTPS connections do not satisfy the TURN CIDR check, so the debounce starts immediately. Measured exit latency: ~5 s total.
- **UDP path** (no TURN relay observed — the default on typical networks): debounce **15 s**; signal is `has_browser_capture_session()`. The longer debounce absorbs WASAPI transients (brief capture-session drops observed during live calls, up to ~10 s). `has_meet_connection()` (broad Google TCP) remains `true` on the "You've left the meeting" lobby page and is not used for exit. `has_browser_capture_session()` checks whether any browser process (`chrome.exe`, `msedge.exe`, `firefox.exe`, `brave.exe`) holds an `AudioSessionStateActive` WASAPI capture session via `IAudioSessionManager2`. Chrome and Edge release the `getUserMedia` capture session within ~1–2 s of the user leaving the call (measured); the session remains `Active` while the user is muted (`track.enabled=false`, not `track.stop()`, so `IAudioClient::Start()` keeps streaming). Only `Expired` means the stream was released. Measured WASAPI lag on leave: ~1 s.

On the `InCall → Idle` transition, `MeetingDetectorPort::notify_exit()` is called first (adapters reset per-call sticky state before the frontend sees the event), then `meeting-ended` is emitted.

#### Scenario: User leaves a UDP-transport call
- **WHEN** the detector is in `InCall` AND no TCP TURN connection was ever observed for this call AND the user clicks "Leave call" in Chrome or Edge
- **THEN** the browser's WASAPI audio capture session is released within ~1–2 s AND `has_browser_capture_session()` returns `false` AND after it remains false for 15 s the detector transitions to `Idle` and emits `meeting-ended`

#### Scenario: Lobby page does not trigger exit for UDP call
- **WHEN** the detector is in `InCall` for a UDP call AND the user is on the `meet.google.com/<code>` lobby page with the title still showing "Meet - xxx" AND HTTPS connections to Google IPs are still open
- **THEN** the detector SHALL NOT transition to `Idle` while the browser still holds an active capture session — `has_browser_capture_session()` is `true` (the lobby page has the Meet tab open with an active getUserMedia session), so the debounce timer is cleared on every poll and no exit event fires

#### Scenario: User leaves a TCP TURN call (behaviour unchanged)
- **WHEN** the detector is in `InCall` AND a TCP TURN connection was observed during the call AND the user leaves the call
- **THEN** `has_turn_connection()` drops to `false` (TURN relay disconnects on hang-up) AND after 4 s the detector transitions to `Idle` and emits `meeting-ended` — the WASAPI check is NOT applied on this path

#### Scenario: Transient network drop (TURN path)
- **WHEN** the detector is in `InCall` on the TURN path AND `has_turn_connection()` drops for less than 4 s before reappearing
- **THEN** the detector remains in `InCall` and emits no event

#### Scenario: Browser capture session transiently drops during call (UDP path)
- **WHEN** the detector is in `InCall` for a UDP call AND the WASAPI capture session is briefly released and re-acquired within 15 s (e.g., device switch)
- **THEN** the detector remains in `InCall` — the 15 s debounce window absorbs the transient absence

#### Scenario: WASAPI enumeration fails
- **WHEN** `has_browser_capture_session()` fails to initialise COM or enumerate sessions
- **THEN** it returns `false` AND the conjunction becomes `false` AND the debounce starts — this is the conservative default (may fire `meeting-ended` early rather than never)

### Requirement: TURN-relay latch is scoped and non-blocking

The adapter's per-call `turn_established` latch SHALL be scoped to genuine in-call observations and SHALL NEVER suppress the entry signal.

- **Entry is unconditional.** The `has_meet_connection` observation returned by `current_state()` SHALL equal `has_turn_connection() || (has_meet_connection() && has_browser_capture_session())` regardless of the value of `turn_established`. A stale latch SHALL NOT force the entry signal false.
- **Latch set is gated on an in-call discriminator.** `turn_established` SHALL be set to `true` only on a poll where a TURN relay is observed AND the browser holds an active capture session (`has_turn_connection() && has_browser_capture_session()`). Browser TCP to a Google/GCP IP without an active capture session SHALL NOT set the latch.
- **Latch drives only `is_turn_exit`.** The latch exists solely to select the 4 s TURN exit debounce (`is_turn_exit = !has_turn_connection() && turn_established`) for calls that used a TURN relay. It SHALL be reset to `false` by `notify_exit()` on the `InCall → Idle` transition so back-to-back calls remain detectable.

#### Scenario: Stale latch does not block detection of a later call

- **WHEN** `turn_established` is `true` (latched by any prior observation) AND on a subsequent poll `has_turn_connection()` is `false` BUT `has_meet_connection()` and `has_browser_capture_session()` are both `true`
- **THEN** the observation's `has_meet_connection` SHALL be `true` (entry is not suppressed) AND the detector can transition `Idle → InCall`

#### Scenario: Background GCP traffic does not set the latch

- **WHEN** `has_turn_connection()` is `true` (browser has a TCP connection to a GCP/Google IP) BUT `has_browser_capture_session()` is `false` (no active call — e.g. an ordinary Google service in a background tab)
- **THEN** `turn_established` SHALL remain `false` AND a later UDP call's exit SHALL use the 15 s UDP debounce (not the 4 s TURN debounce), because `is_turn_exit` is `false`

#### Scenario: Genuine TURN call still gets the fast exit debounce

- **WHEN** during a detected call both `has_turn_connection()` and `has_browser_capture_session()` are `true` on at least one poll
- **THEN** `turn_established` SHALL be set to `true` AND when the TURN relay subsequently drops (`has_turn_connection()` → `false`) `is_turn_exit` SHALL be `true` AND the 4 s TURN debounce applies (behaviour preserved)

#### Scenario: notify_exit resets the latch for back-to-back calls

- **WHEN** a TURN call ends AND `notify_exit()` is called on the `InCall → Idle` transition
- **THEN** `turn_established` SHALL be reset to `false` AND a subsequent UDP call (no TURN relay) SHALL be detected normally with `is_turn_exit = false`

### Requirement: Meeting detection gates the transcription queue


On `meeting-detected`, the system SHALL set `scheduler.meeting_busy = true` and `SHOULD_YIELD = true` so that any in-flight transcription chunk is interrupted at the next yield point and no new jobs are dispatched while a call is active.

> **Status: implemented 2026-05-18** — wired in `lib.rs` as part of `post-meeting-transcription`.

On `meeting-ended`, the system SHALL clear `scheduler.meeting_busy = false`. If `manual_pause_all` is not set, the system SHALL call `queue.resume_all()` (in a spawned async task) to allow queued jobs to continue. If `manual_pause_all` is set (user deliberately paused all background work), the worker SHALL NOT be resumed — only the `meeting_busy` gate is cleared.

> **Fixed 2026-05-18:** `resume_all()` previously called unconditionally, clearing `manual_pause_all` and silently lifting a user-initiated global pause when a meeting ended. Fixed by guarding the `resume_all()` call with `if !scheduler.manual_pause_all.load(SeqCst)`.

#### Scenario: Transcription pauses when meeting is detected
- **WHEN** a `meeting-detected` event fires AND a transcription job is in progress
- **THEN** `scheduler.meeting_busy` is set to `true` AND `SHOULD_YIELD` is set to `true` AND the running job yields at the next chunk boundary

#### Scenario: Transcription resumes when meeting ends and no manual pause is active
- **WHEN** a `meeting-ended` event fires AND `manual_pause_all` is `false`
- **THEN** `scheduler.meeting_busy` is cleared AND `resume_all()` is called AND any paused jobs transition to `Pending` and the worker loop is notified

#### Scenario: Manual pause survives meeting end
- **WHEN** a `meeting-ended` event fires AND `manual_pause_all` is `true`
- **THEN** `scheduler.meeting_busy` is cleared AND `resume_all()` is NOT called AND `manual_pause_all` remains `true` AND `can_run()` returns `false`

### Requirement: Conservative app-start state
The detector SHALL NOT fire `meeting-detected` for connections that were already present at the time the detector was first launched. Only connections appearing during the detector's observation window trigger the event.

#### Scenario: Meetily launches while user is already in a Meet call
- **WHEN** the user is in a Meet call AND launches Meetily AND on the first poll the Meet WebRTC connection is already present
- **THEN** the detector establishes that connection as "pre-existing" and does NOT fire `meeting-detected`; the user must start recording manually for this call

#### Scenario: Connection drops and reappears after app launch
- **WHEN** Meetily launches with no Meet connection present AND later the user joins a Meet call AND the connection appears
- **THEN** the detector fires `meeting-detected` normally

> **Known limitation:** The UDP entry signal (`has_meet_connection() AND has_browser_capture_session()`) is satisfied by the Meet lobby page as well as an active call (lobby HTTPS connections + `getUserMedia` camera/mic preview both satisfy the signals). If Meetily launches while the user has the Meet lobby open and then joins the call without navigating away, `connection_first_seen_at` may be set at detector start and the `not_preexisting` guard may suppress `meeting-detected`. In practice the user would start recording manually for that call. A future change should add a UDP socket check (`GetExtendedUdpTable` to Google media IPs — the WebRTC media signal absent from the lobby) to provide a discriminating entry signal.

> **Implementation note — dual-stack hosts:** On Windows hosts with dual-stack network adapters, the kernel may report established TCP connections using IPv4-mapped IPv6 notation (`::ffff:x.x.x.x`). The TCP6 scanner unwraps these to IPv4 before CIDR matching so Google range checks work correctly on dual-stack configurations.

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
- **WHEN** the user has cancelled the current call's auto-start AND the connection signal drops for less than the debounce window (15 s for UDP, 4 s for TURN) and reappears (no Idle transition occurred)
- **THEN** the detector does NOT re-fire `meeting-detected` for this call

#### Scenario: User cancels, call truly ends, new call begins
- **WHEN** the user cancelled an earlier call AND the connection signal was absent for longer than the debounce window (Idle transition occurred) AND a new connection appears
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

### Requirement: Debug-only detector simulation seam
The system SHALL provide a detector simulation seam, compiled only when the off-by-default `dev-detector` Cargo feature is enabled, that drives the production detection state machine (`spawn_detector` / `step_detector`) with a synthetic `DetectorObservation` so a developer can trigger `meeting-detected` and `meeting-ended` without joining a real Google Meet call. When the `dev-detector` feature is disabled (the default), the seam — the fake adapter, its controller, and the `__dev_simulate_meeting` Tauri command — SHALL NOT be compiled into the binary, and production detection SHALL be identical to a build without this change.

The seam SHALL expose a `__dev_simulate_meeting(state, title?)` Tauri command (registered only under the feature) where `state = "joined"` sets the observation to a fresh in-call signal — a Meet-titled window, `has_meet_connection = true`, `has_browser_capture_session = true`, and `connection_first_seen_at` equal to the current instant so the conservative app-start guard does not suppress it — and `state = "left"` sets the observation to the **full idle state** — all six fields cleared, matching `DetectorObservation::default()` and the real adapter's idle output (`meet_windows = []`, `has_meet_connection = false`, `has_browser_capture_session = false`, `connection_first_seen_at = None`, `default_title = ""`, `is_turn_exit = false`) — after which the real 15 s UDP debounce applies before `meeting-ended` fires. The `title` argument, when provided, SHALL set the resolved default title and the synthetic window title.

#### Scenario: Seam is absent from a default build
- **GIVEN** the `dev-detector` Cargo feature is disabled (the default)
- **WHEN** the app is compiled and started
- **THEN** the `__dev_simulate_meeting` Tauri command is not registered
- **AND** no fake detector adapter is compiled into the binary
- **AND** production detection behaves identically to a build without this change

#### Scenario: Simulating a join fires meeting-detected and auto-starts a real recording
- **GIVEN** the app is built with the `dev-detector` feature and the detector polling loop is running with the fake adapter
- **WHEN** the developer invokes `__dev_simulate_meeting("joined", Some("Weekly sync"))`
- **THEN** within one poll interval the state machine transitions `Idle → InCall` and emits `meeting-detected` with `default_title = "Weekly sync"`
- **AND** the frontend auto-start banner appears and `start_recording` begins capturing real audio exactly as it would for a genuine call

#### Scenario: Simulating a leave fires meeting-ended after the real debounce
- **GIVEN** the fake detector is in the `joined` state and a detector-started recording is active
- **WHEN** the developer invokes `__dev_simulate_meeting("left", None)`
- **THEN** the state machine starts the real 15 s UDP debounce
- **AND** after the debounce elapses it emits `meeting-ended`
- **AND** the frontend shows the stop-prompt banner for the detector-started recording

#### Scenario: The real state-machine semantics are exercised unchanged
- **GIVEN** the app is built with the `dev-detector` feature
- **WHEN** the fake observation is driven through a join → leave script
- **THEN** the cancel-suppression, pre-existing-connection, and debounce behaviours are governed by the unmodified `step_detector`
- **AND** no parallel mock event path bypasses the state machine

#### Scenario: Unknown state value is rejected
- **GIVEN** the app is built with the `dev-detector` feature and the fake detector is running
- **WHEN** the developer invokes `__dev_simulate_meeting("paused", None)` (or any value other than `"joined"` / `"left"`)
- **THEN** the command returns an error before the shared observation is mutated
- **AND** no `meeting-detected` or `meeting-ended` event is emitted

