## MODIFIED Requirements

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
- **THEN** the detector SHALL NOT transition to `Idle` while the browser still holds an active capture session — `has_meet_connection()` is `true` AND `has_browser_capture_session()` is `true` → the conjunction remains `true`

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

### Requirement: Meeting detection gates the transcription queue

> **Status: implemented 2026-05-18** — wired in `lib.rs` as part of `post-meeting-transcription`.

On `meeting-detected`, the system SHALL set `scheduler.meeting_busy = true` and `SHOULD_YIELD = true` so that any in-flight transcription chunk is interrupted at the next yield point and no new jobs are dispatched while a call is active.

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
