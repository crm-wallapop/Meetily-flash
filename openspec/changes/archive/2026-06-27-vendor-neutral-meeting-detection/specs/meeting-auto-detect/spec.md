## MODIFIED Requirements

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

## ADDED Requirements

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
