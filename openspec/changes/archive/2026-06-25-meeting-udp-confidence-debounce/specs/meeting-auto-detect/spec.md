## MODIFIED Requirements

### Requirement: Detect when an active call ends

The system SHALL transition from `InCall` to `Idle` when the connection signal becomes false and remains false for the debounce window. The debounce window and connection signal are derived differently by transport path:

- **TCP TURN path** (TURN relay was observed): debounce **4 s**; signal is `has_turn_connection()`. It drops to `false` within ~1 s of the user leaving the call. The lobby page's HTTPS connections do not satisfy the TURN CIDR check, so the debounce starts immediately. Measured exit latency: ~5 s total.
- **UDP path** (no TURN relay observed — the default on typical networks): signal is `has_browser_capture_session()`. The debounce duration is **adaptive**, selected by the pure `step_detector` from the observation's `stable_capture` flag: **4 s** when `stable_capture` is `true`, else **15 s**. `stable_capture` SHALL be decided **once per call**, at the **first** `has_browser_capture_session()` `true → false` drop, from the length of the unbroken `true` run immediately preceding that drop: `stable_capture == true` iff that run was ≥ `STABLE_CONFIDENCE_WINDOW` (~20 s, a `const` chosen to exceed the ~10 s WASAPI transient ceiling with margin); otherwise `false`. A genuinely flaky UDP session drops frequently, so the run before its first drop is short (< window) → 15 s; a stable session holds capture for minutes → ≥ window → 4 s. The decision is stored in a per-call `exit_stable_latch: Option<bool>` which, once set to `Some(v)`, SHALL be held **immutable** for the rest of the call — it SHALL NOT be cleared or recomputed on any subsequent `false → true` recovery or later drop — until `notify_exit()` resets it. This immutability is mandatory because `step_detector` recomputes the debounce duration on every poll and the value driving it MUST NOT change mid-debounce: the prior recovery-based draft cleared the latch on a `false → true` edge and recreated the `detector-turn-latch` self-heal trap of commit `693ff90`, where a single WASAPI-flicker poll mid-debounce flipped a running 4 s exit to 15 s. (Consequence, decided 2026-06-25: the earlier "a recovered transient ⟹ 15 s for the rest of the call" rule is relaxed — see the scenario below.) The adapter SHALL reset all per-call capture state (`exit_stable_latch` and the continuous-active timer `bc_true_since`) to the conservative default in `notify_exit()` on the `InCall → Idle` transition; a mid-process crash before `notify_exit()` is covered by `WindowsMeetingDetector` being reconstructed fresh on next start (the fields start `None`). The 15 s value absorbs WASAPI transients (brief capture-session drops observed during live calls, up to ~10 s); a stable-mic call (the common case) exits in ~5–6 s while a flaky setup keeps the safe 15 s. `has_meet_connection()` (broad Google TCP) remains `true` on the "You've left the meeting" lobby page and is not used for exit. `has_browser_capture_session()` checks whether any browser process (`chrome.exe`, `msedge.exe`, `firefox.exe`, `brave.exe`) holds an `AudioSessionStateActive` WASAPI capture session via `IAudioSessionManager2`. Chrome and Edge release the `getUserMedia` capture session within ~1–2 s of the user leaving the call (measured); the session remains `Active` while the user is muted (`track.enabled=false`, not `track.stop()`, so `IAudioClient::Start()` keeps streaming). Only `Expired` means the stream was released. Measured WASAPI lag on leave: ~1 s.

On the `InCall → Idle` transition, `MeetingDetectorPort::notify_exit()` is called first (adapters reset per-call sticky state — `exit_stable_latch` and `bc_true_since` — before the frontend sees the event), then `meeting-ended` is emitted.

#### Scenario: Stable-mic UDP call exits on the short debounce
- **WHEN** the detector is in `InCall` AND no TCP TURN connection was ever observed for this call AND `has_browser_capture_session()` has been continuously `true` for at least `STABLE_CONFIDENCE_WINDOW` AND the user clicks "Leave call" in Chrome or Edge
- **THEN** the browser's WASAPI audio capture session is released within ~1–2 s AND `has_browser_capture_session()` returns `false` AND the adapter sets `exit_stable_latch = Some(true)` on that first drop poll (the preceding run ≥ window) AND holds it immutable AND after it remains false for **4 s** the detector transitions to `Idle` and emits `meeting-ended` (total exit latency ~5–6 s)

#### Scenario: The first exit drop after a long stable run is stable
- **WHEN** a UDP call has been in `InCall` with `has_browser_capture_session()` continuously `true` past `STABLE_CONFIDENCE_WINDOW` AND the user leaves, producing the call's first `true → false` drop
- **THEN** the adapter SHALL set `exit_stable_latch = Some(true)` (the first drop's run ≥ window) AND `stable_capture` SHALL be `true` AND the UDP debounce duration applied by `step_detector` SHALL be 4 s, not 15 s

#### Scenario: A WASAPI flicker during the 4 s debounce does not flip the decision (self-heal guard)
- **WHEN** a stable-mic UDP call has set `exit_stable_latch = Some(true)` on the exit drop AND a subsequent poll reads `has_browser_capture_session() == true` for a single poll (WASAPI flicker/release lag) during the 4 s debounce AND then reads `false` again
- **THEN** `exit_stable_latch` SHALL remain `Some(true)` (immutable — NOT cleared by the `false → true` flicker) AND `stable_capture` SHALL be `true` on every post-drop poll AND the debounce duration `step_detector` recomputes each poll SHALL stay 4 s — this is the inverse of the reverted `detector-turn-latch` self-heal (commit `693ff90`) and is the load-bearing reason the latch is immutable

#### Scenario: A recovered transient after a long stable run still exits at 4 s (decision locked at the first drop)
- **WHEN** a UDP call is in `InCall` AND `has_browser_capture_session()` has been continuously `true` past `STABLE_CONFIDENCE_WINDOW` AND it drops and later returns (a transient: `true → false → true`) AND the user subsequently leaves the call
- **THEN** the adapter SHALL have set `exit_stable_latch = Some(true)` at the transient's drop (the first drop, preceded by a ≥ window run) AND SHALL hold it immutable across the recovery AND `stable_capture` SHALL be `true` AND the UDP debounce duration SHALL be 4 s — this is the decided (2026-06-25) relaxation of the prior "transient ⟹ 15 s" rule; a call that proved ≥ window of stable capture before its first drop is treated as stable

#### Scenario: UDP call leaves after only a brief stable run uses the long debounce
- **WHEN** a UDP call enters `InCall` AND `has_browser_capture_session()` has been continuously `true` for less than `STABLE_CONFIDENCE_WINDOW` AND the call's first `true → false` drop occurs
- **THEN** the adapter SHALL set `exit_stable_latch = Some(false)` (the first drop's run < window) AND `stable_capture` SHALL be `false` AND the UDP debounce duration applied by `step_detector` SHALL be 15 s — a short stable run is treated conservatively because an early non-recovering drop may be the leading edge of a transient

#### Scenario: The stable_capture decision is immutable across the debounce window
- **WHEN** the adapter sets `exit_stable_latch = Some(true)` on the first drop poll for a stable-mic UDP exit AND subsequent polls continue to observe `has_browser_capture_session() == false` while the 4 s debounce elapses
- **THEN** every such poll SHALL report the same latched `stable_capture == true` value (it SHALL NOT flip to `false` mid-debounce) so the debounce duration `step_detector` recomputes each poll stays 4 s until the threshold is reached

#### Scenario: A mid-call WASAPI transient restarts the run-length timer but not a locked latch
- **WHEN** the detector is in `InCall` for a UDP call AND the WASAPI capture session is briefly released and re-acquired mid-call (e.g. device switch: `true → false → true`) before any exit decision is locked
- **THEN** the detector remains in `InCall` (capture recovered, so `step_detector` clears the debounce timer on the `false → true` edge) AND the continuous-active timer `bc_true_since` restarts from the recovery edge AND no `exit_stable_latch` has been set yet (no exit drop occurred); the run-length that will classify a later exit is measured from this recovery

#### Scenario: Detector constructed mid-call exits conservatively
- **WHEN** the app starts while a meeting is already in progress AND `WindowsMeetingDetector` is constructed AND the first poll reads `has_browser_capture_session() == true` AND the user leaves shortly after
- **THEN** `bc_true_since` is stamped to the detector's start instant (the unknowable pre-start capture history cannot be recovered) AND the first drop's `run_len` is short (< window) AND `exit_stable_latch = Some(false)` AND the UDP debounce SHALL be 15 s — the safe direction

#### Scenario: Lobby page does not trigger exit for UDP call
- **WHEN** the detector is in `InCall` for a UDP call AND the user is on the `meet.google.com/<code>` lobby page with the title still showing "Meet - xxx" AND HTTPS connections to Google IPs are still open
- **THEN** the detector SHALL NOT transition to `Idle` while the browser still holds an active capture session — `has_browser_capture_session()` is `true` (the lobby page has the Meet tab open with an active getUserMedia session), so the debounce timer is cleared on every poll and no exit event fires regardless of the adaptive debounce value

#### Scenario: User leaves a TCP TURN call (behaviour unchanged)
- **WHEN** the detector is in `InCall` AND a TCP TURN connection was observed during the call AND the user leaves the call
- **THEN** `has_turn_connection()` drops to `false` (TURN relay disconnects on hang-up) AND after 4 s the detector transitions to `Idle` and emits `meeting-ended` — the WASAPI check is NOT applied on this path

#### Scenario: Transient network drop (TURN path)
- **WHEN** the detector is in `InCall` on the TURN path AND `has_turn_connection()` drops for less than 4 s before reappearing
- **THEN** the detector remains in `InCall` and emits no event

#### Scenario: WASAPI enumeration fails
- **WHEN** `has_browser_capture_session()` fails to initialise COM or enumerate sessions
- **THEN** it returns `false` AND the debounce starts with no prior stable run (`exit_stable_latch = None → stable_capture == false`) AND the 15 s debounce applies — this is the conservative default (may fire `meeting-ended` early rather than never)
