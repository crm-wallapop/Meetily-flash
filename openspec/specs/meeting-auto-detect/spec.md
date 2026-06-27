# Meeting Auto-Detect — Capability Spec

## Purpose

Governs automatic detection of active Google Meet calls on Windows (window-title, TCP, and WASAPI signals), auto-start and auto-stop of recordings, smart title resolution, per-call cancel suppression, transcription-queue gating during calls, and the startup GC pass.
## Requirements
### Requirement: Detect active Google Meet calls on Windows

On Windows, the system SHALL detect that the user is in an active call by polling a
**vendor-neutral gate** (heading retained for canonical-spec continuity; renamed to "Detect
active meeting calls on Windows" when the second vendor adapter lands): (1) a wired
`CallSignalingPort` adapter reports an active
call-signaling connection (v1: the Meet adapter's Google-CIDR `GetExtendedTcpTable` check,
`has_meet_connection()`), AND (2) a browser process (`chrome.exe`, `msedge.exe`,
`firefox.exe`, `brave.exe`) holds an `AudioSessionStateActive` WASAPI capture session
(`has_browser_capture_session()`). For calls using a TCP TURN relay, the TURN CIDR check
(`has_turn_connection()`) is sufficient alone and replaces the signaling+bc conjunction.
Detection fires only for connections that first appear after the detector starts
(pre-existing connections are ignored — see Conservative app-start state).

**The window title is NOT consulted for entry.** The third gate signal of the prior design
— a top-level window whose title matched the Meet regex (`has_title`) — is **removed** from
the entry decision and decoupled to `MeetingTitleExtractorPort` (best-effort decoration,
resolved post-transition). A call SHALL be detected even when no recognizable titled window
exists, provided the signaling+bc (or TURN) conjunction and `not_preexisting` hold. This
decouples detection from title-pattern fragility (the EN-dash bug that disabled all PWA
detection cannot recur as a detection outage).

The `CallSignalingPort` result feeding signal (1) is the OR of all wired adapters; v1 wires
exactly one (Meet). The state machine's entry term is `has_conn && not_preexisting` where
`has_conn = has_turn_connection || (signaling_active && has_browser_capture_session)`.

#### Scenario: User clicks "Join now" on a Meet call
- **WHEN** chrome.exe newly establishes a TCP connection to a Google media/signalling IP
  (Meet signaling adapter reports active) AND holds an active WASAPI capture session
- **THEN** the detector transitions from `Idle` to `InCall` and emits a `meeting-detected`
  Tauri event — regardless of whether a Meet-titled window is present at the transition
  instant (title is resolved post-transition by `MeetingTitleExtractorPort`)

#### Scenario: Meet PWA in-call — detection no longer blocked by the title regex
- **WHEN** the user is in a Meet call via the PWA (`msedge.exe`) AND the Meet signaling
  adapter reports active (HTTPS TCP to Google IPs) AND `has_browser_capture_session()` is
  true
- **THEN** the detector transitions `Idle → InCall` and emits `meeting-detected`. The PWA's
  EN-dash title (`Google Meet - Meet – <code>`) is irrelevant to entry; it is consumed only
  by the post-detection title extractor. (Prior to this change, the EM-dash regex bug
  disabled all PWA detection because `has_title` gated entry.)

#### Scenario: Entry fires without any recognizable titled window
- **WHEN** the signaling adapter reports active AND `has_browser_capture_session()` is true
  AND `not_preexisting` holds AND no enumerated window matches any vendor's title pattern
- **THEN** the detector transitions `Idle → InCall` AND `meeting-detected` fires AND the
  recording's default title falls through to the generic-timestamp default
  (`Meeting <YYYY-MM-DD HH:MM>`) — detection succeeds, title is merely unnamed

#### Scenario: Signaling false blocks entry (no title gate to fall back on)
- **WHEN** no `CallSignalingPort` adapter reports active AND no TURN relay is observed
  AND `has_browser_capture_session()` is true (e.g. a browser dictation tool with the mic
  open) AND `browser_windows` happens to be empty
- **THEN** the detector remains in `Idle` — the signaling+bc conjunction is the sole
  discriminator now that `has_title` is removed

#### Scenario: Meet home page open (no meeting joined) does not fire
- **WHEN** a Chrome window is open to the Meet home page with title `Google Meet` AND no
  meeting has been joined AND `has_browser_capture_session()` is false (no `getUserMedia`)
- **THEN** the detector remains in `Idle` — incidental Google TCP without an active capture
  session does not satisfy the bc term

#### Scenario: Meet green-room then Join — benign early start (KNOWN, universal)
- **WHEN** Meetily is already running AND the user navigates to a Meet green-room /
  pre-join screen (camera preview visible, `Join now` not yet clicked) AND the Meet
  signaling adapter reports active (HTTPS signaling to Google IPs) AND
  `has_browser_capture_session()` is true (the `getUserMedia` camera/mic preview) AND
  `connection_first_seen_at` is fresh AND the user then clicks `Join now`
- **THEN** the detector fires `meeting-detected` a few seconds early (the signaling+bc
  conjunction and `not_preexisting` all hold in the green room — **title no longer
  suppresses this** for any vendor; it previously suppressed it for the PWA only, by
  accident of the EM-dash regex bug). Because `getUserMedia` persists from the green room
  into the call, the early-start recording continues seamlessly into the real call — a
  benign few-seconds-early start (it captures a few seconds of pre-join mic/system audio;
  Meetily records audio, not camera frames), not a spurious recording.

#### Scenario: Meet green-room then abandon — bc-drop exit fires (KNOWN universal FP)
- **WHEN** the user navigates to a Meet green-room AND the detector fires `meeting-detected`
  (as above) AND the user then leaves **without** joining AND `has_browser_capture_session()`
  subsequently drops (Meet released `getUserMedia` after the green room closed)
- **THEN** `meeting-ended` SHALL fire via the bc-drop exit within the configured debounce,
  ending the spurious recording.

#### Scenario: Meet green-room then abandon — F6 holds getUserMedia (KNOWN limitation)
- **WHEN** the user abandons a Meet green-room (as above) AND Meet holds `getUserMedia` open
  after abandonment (the idle-eviction behavior in `meeting-lobby-discrimination` exploration
  F6, untested)
- **THEN** `has_browser_capture_session()` does **not** drop, the bc-drop exit does not fire,
  and the spurious recording runs until the user manually stops it. `meeting-ended` is not
  emitted on this path. (The bc-drop timing on the abandon path is unmeasured; F6 is the
  known escape from the bc-drop bound.)
- **NOTE** This FP is a known limitation, broadened by this change from PWA-only to
  universal. A lobby discriminator is deferred to `meeting-lobby-discrimination`. The
  exploration ranked eRender-in-green-room as the highest-leverage discriminating signal but
  never sampled it; tasks §8 captures it opportunistically so the discriminator opens with
  data. "No clean OS-level signal" is the current untested conclusion, not an established
  result — eRender Active=0 in the green room (with capture Active=1) would be a viable
  discriminator.

#### Scenario: Cross-vendor CIDR+bc imprecision — KNOWN FALSE POSITIVE (new, unmeasured)
- **WHEN** a browser process has an open TCP connection to a Google IP for unrelated
  reasons (Gmail, Drive, a Google search) AND a non-Meet browser call is active in the same
  or another tab (so `has_browser_capture_session()` is true) AND the non-Meet vendor has
  no signaling adapter wired (v1: only Meet is wired)
- **THEN** the detector MAY fire a spurious `meeting-detected` because the Meet signaling
  adapter reports active (the coincidental Google TCP) and bc is true. The recording's
  default title falls through to the generic timestamp (the extractor finds no Meet window)
  — the call is at worst generically named, never mis-attributed to Meet by title.
- **NOTE** This imprecision is a known limitation of CIDR+bc gating without a vendor
  confirmation signal. Its severity is **unmeasured** — the coincidence (pinned Gmail + any
  browser WebRTC call) is common, not rare. It does not self-correct until a vendor-
  confirmation gate ships; "narrows as adapters proliferate" is a future cost, not a v1
  remedy. v1 ships with the imprecision; tasks §8 includes an observation run to measure it;
  revisit if it fires routinely in practice.

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

### Requirement: TURN-relay latch is scoped and non-blocking

The adapter's per-call `turn_established` latch SHALL be scoped to genuine in-call observations and SHALL NEVER suppress the entry signal.

> **Modified by this change:** the raw `has_meet_connection()` free function is extracted behind `CallSignalingPort`. Every function-call reference below becomes `signaling_active` — the OR of wired `CallSignalingPort` adapters (v1: Meet). The `has_meet_connection` **observation field** (the folded entry value `current_state()` returns) is unchanged, so sibling-requirement prose that writes `has_meet_connection()` (the "Detect when an active call ends" body and the "Conservative app-start state" known-limitation) refers to that retained field, not the extracted free function — the identifier resolves unchanged in the merged spec. Latch behavior is otherwise identical.

- **Entry is unconditional.** The `has_meet_connection` observation returned by `current_state()` SHALL equal `has_turn_connection() || (signaling_active && has_browser_capture_session())` — where `signaling_active` is the OR of wired `CallSignalingPort` adapters (v1: Meet) — regardless of the value of `turn_established`. A stale latch SHALL NOT force the entry signal false.
- **Latch set is gated on an in-call discriminator.** `turn_established` SHALL be set to `true` only on a poll where a TURN relay is observed AND the browser holds an active capture session (`has_turn_connection() && has_browser_capture_session()`). Browser TCP to a Google/GCP IP without an active capture session SHALL NOT set the latch.
- **Latch drives only `is_turn_exit`.** The latch exists solely to select the 4 s TURN exit debounce (`is_turn_exit = !has_turn_connection() && turn_established`) for calls that used a TURN relay. It SHALL be reset to `false` by `notify_exit()` on the `InCall → Idle` transition so back-to-back calls remain detectable.

#### Scenario: Stale latch does not block detection of a later call

- **WHEN** `turn_established` is `true` (latched by any prior observation) AND on a subsequent poll `has_turn_connection()` is `false` BUT `signaling_active` and `has_browser_capture_session()` are both `true`
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

> **Known limitation:** The UDP entry signal (`has_meet_connection() AND has_browser_capture_session()`) is satisfied by the Meet lobby page as well as an active call (lobby HTTPS connections + `getUserMedia` camera/mic preview both satisfy the signals). If Meetily launches while the user has the Meet lobby open and then joins the call without navigating away, `connection_first_seen_at` may be set at detector start and the `not_preexisting` guard may suppress `meeting-detected`. In practice the user would start recording manually for that call.
>
> The previously-speculated future remedy — a `GetExtendedUdpTable` check for UDP media flows to Google IPs — is **not viable**: `GetExtendedUdpTable` exposes only the local address, local port, and owning PID (UDP is connectionless; there is no remote-address field), so a WebRTC media flow cannot be distinguished from QUIC, which Chrome uses over UDP for many Google services. The clean signals that would discriminate an active call's media flow (ETW / Windows Filtering Platform flow events; Chrome DevTools Protocol `RTCPeerConnection` state) are blocked for a user app by their respective prerequisites (admin privileges; launching the browser with `--remote-debugging-port`). Exit latency is instead addressed by the adaptive UDP debounce in the "Detect when an active call ends" requirement; entry lobby-discrimination remains a known limitation.

> **Implementation note — dual-stack hosts:** On Windows hosts with dual-stack network adapters, the kernel may report established TCP connections using IPv4-mapped IPv6 notation (`::ffff:x.x.x.x`). The TCP6 scanner unwraps these to IPv4 before CIDR matching so Google range checks work correctly on dual-stack configurations.

### Requirement: Auto-start recording on call detection

On receiving `meeting-detected`, the frontend SHALL immediately start a recording AND display a countdown banner with an editable title field, a dropdown of all currently-enumerated browser windows for which the wired `MeetingTitleExtractorPort` returns a match (the `candidate_titles` — v1: Meet-titled windows), and a 10-second cancel window.

> **Modified by this change:** the canonical phrase "a dropdown of all currently-enumerated Meet windows" is reworded to "browser windows for which the title extractor returns a match," reflecting that window enumeration is vendor-neutral post-rename (`browser_windows`) and the Meet filter lives inside the extractor adapter. The dropdown's content is unchanged (it still shows the candidate titles the extractor matches); only the naming is swept.

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

The default title shown in the auto-start banner SHALL be resolved in this priority order:
(1) foreground window at detection-transition moment if `MeetingTitleExtractorPort` returns
a title for it; (2) the most recently focused window (last 10 minutes) for which the
extractor returns a title; (3) the first window returned by `EnumWindows` for which the
extractor returns a title; (4) a generic timestamp `Meeting <YYYY-MM-DD HH:MM>`. The
extractor is the sole source of vendor title patterns (v1: the Meet adapter with the EN-dash
regex).

#### Scenario: No vendor window has a title
- **WHEN** `MeetingTitleExtractorPort::extract_title` returns `None` for every enumerated
  window (e.g. a TURN-less call with no recognizable title, or a cross-vendor FP)
- **THEN** the default is `Meeting <YYYY-MM-DD HH:MM>` — detection succeeded; only the name
  is generic

#### Scenario: PWA window is the source
- **WHEN** the user is using the Meet PWA AND the extractor matches the PWA's EN-dash title
  (`Google Meet - Meet – <id>`) AND `strip_google_meet_suffix` yields `<id>`
- **THEN** the PWA window participates in title resolution identically to a browser tab
  window, and the stripped identifier is used as the default title

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

> **Modified by this change:** the `meet_windows`/`MeetWindow` references become `browser_windows`/`BrowserWindow`, and the idle-state field list gains `candidate_titles`. The seam's behavior (driving the real state machine with a synthetic observation) is unchanged.

The seam SHALL expose a `__dev_simulate_meeting(state, title?)` Tauri command (registered only under the feature) where `state = "joined"` sets the observation to a fresh in-call signal — a synthetic browser window, `has_meet_connection = true`, `has_browser_capture_session = true`, and `connection_first_seen_at` equal to the current instant so the conservative app-start guard does not suppress it — and `state = "left"` sets the observation to the **full idle state** — all fields cleared, matching `DetectorObservation::default()` and the real adapter's idle output (`browser_windows = []`, `has_meet_connection = false`, `has_browser_capture_session = false`, `connection_first_seen_at = None`, `default_title = ""`, `candidate_titles = []`, `is_turn_exit = false`, `stable_capture = false`) — after which the real 15 s UDP debounce applies before `meeting-ended` fires. The `title` argument, when provided, SHALL set the resolved default title and the synthetic window title.

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

### Requirement: CallSignalingPort — vendor-neutral signaling seam

The system SHALL expose a port trait `CallSignalingPort`:
```rust
pub trait CallSignalingPort: Send + Sync {
    fn is_call_signaling_active(&self) -> bool;
}
```
through which vendor-specific call-signaling detection is provided. The trait depends on
domain types only (no `windows`, no `tokio` I/O). The meeting detector SHALL treat the OR
of all wired adapters' `is_call_signaling_active()` results as the signaling term of the
entry gate. v1 wires exactly one adapter (`MeetSignalingAdapter` — the Google-CIDR
`GetExtendedTcpTable` check). Adding a vendor is additive: a new adapter file, a re-export
in `detection/signaling/mod.rs`, and composition-root wiring — with no change to the
detector, the state machine, the WASAPI/TURN code, or other vendors' adapters.

A multi-adapter aggregator (`Vec<Box<dyn CallSignalingPort>>`) is **deferred** until the
second vendor adapter lands; with one adapter the OR is trivially that adapter's result.

#### Scenario: Meet adapter detects a UDP call via HTTPS signaling
- **WHEN** a browser process has a TCP connection to a Google CIDR IP (Meet's HTTPS
  signaling, present even when media transport is UDP) AND the Meet signaling adapter is
  the sole wired adapter
- **THEN** `signaling_active` is `true` and the entry gate's signaling term is satisfied —
  Meet-UDP calls remain detected after the de-vendoring of the gate

#### Scenario: A second vendor adapter is additive
- **WHEN** a future change adds `ZoomSignalingAdapter impl CallSignalingPort` AND wires it
  at the composition root
- **THEN** no edit to `detection/windows.rs`, `use_cases/meeting_detection.rs`, or the Meet
  adapter is required; the detector's OR of adapter results includes Zoom with no core
  change

#### Scenario: Dual-stack IPv6-mapped addresses unwrap before CIDR match
- **WHEN** the kernel reports an established connection as IPv4-mapped IPv6
  (`::ffff:x.x.x.x`) on a dual-stack host AND the Meet adapter enumerates the TCP6 table
- **THEN** the address is unwrapped to IPv4 before the Google-CIDR check so the Meet
  signaling adapter returns the correct result on dual-stack configurations (preserved from
  the pre-extraction implementation)

### Requirement: MeetingTitleExtractorPort — best-effort title decoration

The system SHALL expose a port trait `MeetingTitleExtractorPort`:
```rust
pub trait MeetingTitleExtractorPort: Send + Sync {
    fn extract_title(&self, windows: &[BrowserWindow]) -> Option<String>;
}
```
through which vendor-specific window-title extraction is provided. The `windows` argument
carries `BrowserWindow { hwnd_id, pid, title }` values — all visible browser-process windows
(not Meet-filtered; the Meet filter lives inside the Meet adapter's `extract_title`). The
detector SHALL call the extractor **after** the `Idle → InCall` transition to resolve the
recording's default title; it SHALL NOT consult the extractor (nor any window title) for the
entry decision. The extractor SHALL return `None` when no wired vendor's pattern matches, in
which case the generic-timestamp default is used. The extractor SHALL treat any title longer
than 1024 characters as non-matching (returns `None`) — a defensive bound above the Win32
`GetWindowTextW` 512-char buffer that protects future non-Win32 adapters from unbounded
regex input (CLAUDE.md §9: external input is untrusted; validate at the boundary). v1 wires
a single Meet adapter carrying the Meet title regex and `strip_google_meet_suffix`, using the
**EN dash (U+2013)** the PWA emits across in-call, green-room, and Meet-Space states.

Decoupling title extraction from detection is mandatory: a title-pattern regression (a
future format change, a dash variant, an unhandled script) SHALL affect only the recording
name, never the start/stop of recording.

#### Scenario: Meet PWA EN-dash title is extracted
- **WHEN** the enumerated windows include `Google Meet - Meet – opv-augt-jbm` (EN dash)
- **THEN** the Meet extractor returns `Some("opv-augt-jbm")` (suffix stripped) and that
  value becomes the recording's default title

#### Scenario: EM-dash variant does not match
- **WHEN** a window title is `Google Meet - Meet \u{2014} Test` (EM dash — the pre-fix bug
  shape) AND only the Meet adapter is wired
- **THEN** the extractor returns `None` for that window — the fix accepts the EN dash the
  PWA emits, not both dash types

#### Scenario: No vendor matches — generic title, detection unaffected
- **WHEN** the extractor returns `None` for every enumerated window (e.g. a vendor with no
  adapter, or a TURN-only call with no titled window)
- **THEN** the recording uses the generic-timestamp default AND the detector's `Idle →
  InCall` transition already fired on the gate signals — title failure does not block
  detection

#### Scenario: A future title-format change fails loudly, not silently
- **WHEN** a vendor ships a new title format (a third dash variant, a restructured prefix)
  that no wired extractor matches
- **THEN** the recording falls back to the generic title (graceful) AND a unit test with
  the new fixture fails (loud) — the desired failure mode, since silent title-regression
  was the original PWA-detection outage

