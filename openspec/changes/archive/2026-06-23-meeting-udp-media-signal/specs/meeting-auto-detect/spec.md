## MODIFIED Requirements

### Requirement: Detect when an active call ends

The system SHALL transition from `InCall` to `Idle` when the connection signal becomes false and remains false for the debounce window. The debounce window and connection signal are derived by transport path, and the UDP path's debounce is **adaptive** to the call's observed mic-capture stability:

- **TCP TURN path** (TURN relay was observed): debounce **4 s**; signal is `has_turn_connection()`. It drops to `false` within ~1 s of the user leaving the call. The lobby page's HTTPS connections do not satisfy the TURN CIDR check, so the debounce starts immediately. Measured exit latency: ~5 s total.
- **UDP path** (no TURN relay observed — the default on typical networks): signal is `has_browser_capture_session()`. The debounce duration is **adaptive**, selected by the pure `step_detector` from the observation's `stable_capture` flag: **4 s** when `stable_capture` is `true`, else **15 s**. `stable_capture` SHALL be `true` only while no `has_browser_capture_session()` drop (a `true → false` transition) has been observed during the current call; the adapter SHALL latch it to `false` on the first such drop and SHALL reset it to the conservative default (`false`) in `notify_exit()` on the `InCall → Idle` transition. The 15 s value absorbs WASAPI transients (brief capture-session drops observed during live calls, up to ~10 s); once any drop is observed the call is treated as transient-prone and uses 15 s for the rest of the call, so a stable-mic call (the common case) exits in ~5–6 s while a transient-prone setup keeps the safe 15 s. `has_meet_connection()` (broad Google TCP) remains `true` on the "You've left the meeting" lobby page and is not used for exit. `has_browser_capture_session()` checks whether any browser process (`chrome.exe`, `msedge.exe`, `firefox.exe`, `brave.exe`) holds an `AudioSessionStateActive` WASAPI capture session via `IAudioSessionManager2`. Chrome and Edge release the `getUserMedia` capture session within ~1–2 s of the user leaving the call (measured); the session remains `Active` while the user is muted (`track.enabled=false`, not `track.stop()`, so `IAudioClient::Start()` keeps streaming). Only `Expired` means the stream was released. Measured WASAPI lag on leave: ~1 s.

On the `InCall → Idle` transition, `MeetingDetectorPort::notify_exit()` is called first (adapters reset per-call sticky state — including the `stable_capture` latch — before the frontend sees the event), then `meeting-ended` is emitted.

#### Scenario: User leaves a UDP-transport call on a stable mic
- **WHEN** the detector is in `InCall` AND no TCP TURN connection was ever observed for this call AND no `has_browser_capture_session()` drop has been observed during this call (`stable_capture == true`) AND the user clicks "Leave call" in Chrome or Edge
- **THEN** the browser's WASAPI audio capture session is released within ~1–2 s AND `has_browser_capture_session()` returns `false` AND `stable_capture` remains `true` AND after it remains false for **4 s** the detector transitions to `Idle` and emits `meeting-ended` (total exit latency ~5–6 s)

#### Scenario: Stable-mic UDP call exits on the short debounce
- **WHEN** a UDP call has been in `InCall` with `has_browser_capture_session()` continuously `true` (no drop observed) AND the user leaves the call
- **THEN** the UDP debounce duration applied by `step_detector` SHALL be 4 s, not 15 s

#### Scenario: UDP call with an observed bc drop uses the long debounce
- **WHEN** a UDP call is in `InCall` AND `has_browser_capture_session()` drops and later returns (a transient, e.g. device switch) AND the user subsequently leaves the call
- **THEN** `stable_capture` SHALL be `false` (latched by the observed drop) AND the UDP debounce duration applied by `step_detector` SHALL be 15 s

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
- **THEN** the detector remains in `InCall` — the transient latches `stable_capture` to `false`, so the 15 s debounce window applies and absorbs the transient absence, and any subsequent exit for this call also uses 15 s

#### Scenario: WASAPI enumeration fails
- **WHEN** `has_browser_capture_session()` fails to initialise COM or enumerate sessions
- **THEN** it returns `false` AND the conjunction becomes `false` AND the debounce starts — this is the conservative default (may fire `meeting-ended` early rather than never)

### Requirement: Conservative app-start state
The detector SHALL NOT fire `meeting-detected` for connections that were already present at the time the detector was first launched. Only connections appearing during the detector's observation window trigger the event.

#### Scenario: Meetily launches while user is already in a Meet call
- **WHEN** the user is in a Meet call AND launches Meetily AND on the first poll the Meet WebRTC connection is already present
- **THEN** the detector establishes that connection as "pre-existing" and does NOT fire `meeting-detected`; the user must start recording manually for this call

#### Scenario: Connection drops and reappears after app launch
- **WHEN** Meetily launches with no Meet connection present AND later the user joins a Meet call AND the connection appears
- **THEN** the detector fires `meeting-detected` normally

> **Known limitation:** The UDP entry signal (`has_meet_connection() AND has_browser_capture_session()`) is satisfied by the Meet lobby page as well as an active call (lobby HTTPS connections + `getUserMedia` camera/mic preview both satisfy the signals). If Meetily launches while the user has the Meet lobby open and then joins the call without navigating away, `connection_first_seen_at` may be set at detector start and the `not_preexisting` guard may suppress `meeting-detected`. In practice the user would start recording manually for that call.
>
> The previously-speculated future remedy — a `GetExtendedUdpTable` check for UDP media flows to Google IPs — is **not viable**: `GetExtendedUdpTable` exposes only the local address, local port, and owning PID (UDP is connectionless; there is no remote-address field), so a WebRTC media flow cannot be distinguished from QUIC, which Chrome uses over UDP for many Google services. The clean signals that would discriminate an active call's media flow (ETW / Windows Filtering Platform flow events; Chrome DevTools Protocol `RTCPeerConnection` state) are blocked for a user app by their respective prerequisites (admin privileges; launching the browser with `--remote-debugging-port`). Exit latency is instead addressed by the adaptive UDP debounce in the "Detect when an active call ends" requirement; entry lobby-discrimination remains a known limitation.

> **Implementation note — dual-stack hosts:** On Windows hosts with dual-stack network adapters, the kernel may report established TCP connections using IPv4-mapped IPv6 notation (`::ffff:x.x.x.x`). The TCP6 scanner unwraps these to IPv4 before CIDR matching so Google range checks work correctly on dual-stack configurations.
