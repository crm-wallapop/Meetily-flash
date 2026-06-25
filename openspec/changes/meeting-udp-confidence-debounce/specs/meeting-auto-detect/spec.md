## MODIFIED Requirements

### Requirement: Detect when an active call ends

The system SHALL transition from `InCall` to `Idle` when the connection signal becomes false and remains false for the debounce window. The debounce window and connection signal are derived differently by transport path:

- **TCP TURN path** (TURN relay was observed): debounce **4 s**; signal is `has_turn_connection()`. It drops to `false` within ~1 s of the user leaving the call. The lobby page's HTTPS connections do not satisfy the TURN CIDR check, so the debounce starts immediately. Measured exit latency: ~5 s total.
- **UDP path** (no TURN relay observed — the default on typical networks): signal is `has_browser_capture_session()`. The debounce duration is **adaptive**, selected by the pure `step_detector` from the observation's `stable_capture` flag: **4 s** when `stable_capture` is `true`, else **15 s**. `stable_capture` SHALL be `true` only when BOTH hold for the current call: (a) no *recovered* capture drop has occurred — i.e. no `has_browser_capture_session()` `true → false` transition has been followed by a `false → true` recovery during this call; AND (b) the capture session was continuously active for at least `STABLE_CONFIDENCE_WINDOW` (~20 s, chosen to exceed the ~10 s WASAPI transient ceiling with margin) immediately before the current drop. The adapter SHALL latch the transient-prone flag on the *recovery* edge (`false → true`), NOT on the drop edge — a `true → false` drop that is never followed by a recovery is the exit itself and SHALL NOT mark the call transient-prone. The adapter SHALL compute the `stable_capture` value at the drop poll and hold it stable for every subsequent `has_browser_capture_session() == false` poll until capture recovers or `notify_exit()` fires, because `step_detector` recomputes the debounce duration on every poll and the value driving it MUST NOT change mid-debounce. The adapter SHALL reset all per-call capture state (the transient-prone flag, the continuous-active timer, and the latched `stable_capture` decision) to the conservative default in `notify_exit()` on the `InCall → Idle` transition. The 15 s value absorbs WASAPI transients (brief capture-session drops observed during live calls, up to ~10 s); a stable-mic call (the common case) exits in ~5–6 s while a transient-prone setup keeps the safe 15 s. `has_meet_connection()` (broad Google TCP) remains `true` on the "You've left the meeting" lobby page and is not used for exit. `has_browser_capture_session()` checks whether any browser process (`chrome.exe`, `msedge.exe`, `firefox.exe`, `brave.exe`) holds an `AudioSessionStateActive` WASAPI capture session via `IAudioSessionManager2`. Chrome and Edge release the `getUserMedia` capture session within ~1–2 s of the user leaving the call (measured); the session remains `Active` while the user is muted (`track.enabled=false`, not `track.stop()`, so `IAudioClient::Start()` keeps streaming). Only `Expired` means the stream was released. Measured WASAPI lag on leave: ~1 s.

On the `InCall → Idle` transition, `MeetingDetectorPort::notify_exit()` is called first (adapters reset per-call sticky state — including the `stable_capture` latch, the transient-prone flag, and the continuous-active timer — before the frontend sees the event), then `meeting-ended` is emitted.

#### Scenario: User leaves a UDP-transport call on a stable mic
- **WHEN** the detector is in `InCall` AND no TCP TURN connection was ever observed for this call AND `has_browser_capture_session()` has been continuously `true` for at least `STABLE_CONFIDENCE_WINDOW` AND no recovered drop has occurred this call AND the user clicks "Leave call" in Chrome or Edge
- **THEN** the browser's WASAPI audio capture session is released within ~1–2 s AND `has_browser_capture_session()` returns `false` AND the adapter emits `stable_capture == true` on the drop poll AND holds it `true` across the debounce AND after it remains false for **4 s** the detector transitions to `Idle` and emits `meeting-ended` (total exit latency ~5–6 s)

#### Scenario: The exit drop alone does not mark the call transient-prone
- **WHEN** a UDP call has been in `InCall` with `has_browser_capture_session()` continuously `true` past `STABLE_CONFIDENCE_WINDOW` AND the user leaves, producing a single `true → false` drop that is never followed by a recovery
- **THEN** the transient-prone flag SHALL remain `false` (it latches only on a `false → true` recovery, which does not occur) AND `stable_capture` SHALL be `true` AND the UDP debounce duration applied by `step_detector` SHALL be 4 s, not 15 s

#### Scenario: UDP call with a recovered capture drop uses the long debounce
- **WHEN** a UDP call is in `InCall` AND `has_browser_capture_session()` drops and later returns (a transient, e.g. device switch — a `true → false → true` sequence) AND the user subsequently leaves the call
- **THEN** the transient-prone flag SHALL be `true` (latched on the recovery edge) AND `stable_capture` SHALL be `false` AND the UDP debounce duration applied by `step_detector` SHALL be 15 s

#### Scenario: UDP call leaves after only a brief stable run uses the long debounce
- **WHEN** a UDP call enters `InCall` AND `has_browser_capture_session()` has been continuously `true` for less than `STABLE_CONFIDENCE_WINDOW` AND a `true → false` drop occurs
- **THEN** `stable_capture` SHALL be `false` (the minimum stable-run guard is not satisfied) AND the UDP debounce duration applied by `step_detector` SHALL be 15 s — a short stable run is treated conservatively because an early non-recovering drop may be the leading edge of a transient

#### Scenario: The stable_capture decision is stable across the debounce window
- **WHEN** the adapter emits `stable_capture == true` on the drop poll for a stable-mic UDP exit AND subsequent polls continue to observe `has_browser_capture_session() == false` while the 4 s debounce elapses
- **THEN** every such poll SHALL report the same latched `stable_capture == true` value (it SHALL NOT flip to `false` mid-debounce) so the debounce duration `step_detector` recomputes each poll stays 4 s until the threshold is reached

#### Scenario: Lobby page does not trigger exit for UDP call
- **WHEN** the detector is in `InCall` for a UDP call AND the user is on the `meet.google.com/<code>` lobby page with the title still showing "Meet - xxx" AND HTTPS connections to Google IPs are still open
- **THEN** the detector SHALL NOT transition to `Idle` while the browser still holds an active capture session — `has_browser_capture_session()` is `true` (the lobby page has the Meet tab open with an active getUserMedia session), so the debounce timer is cleared on every poll and no exit event fires regardless of the adaptive debounce value

#### Scenario: User leaves a TCP TURN call (behaviour unchanged)
- **WHEN** the detector is in `InCall` AND a TCP TURN connection was observed during the call AND the user leaves the call
- **THEN** `has_turn_connection()` drops to `false` (TURN relay disconnects on hang-up) AND after 4 s the detector transitions to `Idle` and emits `meeting-ended` — the WASAPI check is NOT applied on this path

#### Scenario: Transient network drop (TURN path)
- **WHEN** the detector is in `InCall` on the TURN path AND `has_turn_connection()` drops for less than 4 s before reappearing
- **THEN** the detector remains in `InCall` and emits no event

#### Scenario: Browser capture session transiently drops during call (UDP path)
- **WHEN** the detector is in `InCall` for a UDP call AND the WASAPI capture session is briefly released and re-acquired within 15 s (e.g., device switch)
- **THEN** the detector remains in `InCall` — when capture recovers (`false → true`) the transient-prone flag latches `true`, the continuous-active timer restarts from the recovery, the 15 s debounce window applies and absorbs the transient absence, and any subsequent exit for this call also uses 15 s

#### Scenario: WASAPI enumeration fails
- **WHEN** `has_browser_capture_session()` fails to initialise COM or enumerate sessions
- **THEN** it returns `false` AND the conjunction becomes `false` AND the debounce starts — this is the conservative default (may fire `meeting-ended` early rather than never)
