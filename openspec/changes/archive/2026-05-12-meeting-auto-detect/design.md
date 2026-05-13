## Context

Meetily currently requires the user to click "Start Recording" manually for every meeting. The user joins ~5 Google Meet calls per day, often mid-conversation, and routinely misses the opening minutes. The codebase already has the integration points needed: a notification system, a Tauri event surface (`emit`/`listen`), an audio pipeline triggered by `start_recording_with_devices_and_meeting`, and a precedent for Win32 FFI in `console_utils.rs`.

Three constraints shape the design:
1. **Windows-only for v1** — the user can't test macOS without a physical Mac. macOS is a future change.
2. **Hexagonal architecture** (CLAUDE.md §2) — the detector must sit behind a port trait so the macOS adapter can be added without touching use-case code.
3. **No orphan audio in steady state** — if the user cancels during the countdown, the speculatively-captured audio must be deleted atomically (file + DB row). A startup GC pass reconciles edge-case orphans.

## Goals / Non-Goals

**Goals:**
- Detect an active Google Meet call on Windows within ~10s of the user joining.
- Avoid false positives from "Meet tab open in background, not in a call," from other browser audio (Spotify, dictation tools, Discord PWAs), and from any non-Meet workload.
- Cover muted joins, solo-speaker scenarios, and silent pre-meeting periods (the last by design choice: silent periods are not detected, manual record is the fallback).
- Capture audio from the moment of detection, not from countdown expiry.
- Clean cancel path: no orphan audio or DB rows.
- Auto-stop when the call ends, with a 10s prompt giving the user an escape hatch.
- Be on by default with a single settings toggle to disable.

**Non-Goals:**
- macOS / Linux support (future change against the same port).
- Detection of non-Meet platforms (Zoom, Teams, Slack Huddles, Discord). Future work; the port can be extended.
- Calendar-aware detection ("a Meet is scheduled in 5 min").
- Speculative-recording UX beyond cancel: no "pause/resume" of the speculative phase.
- Runtime toggle of the auto-detect setting (v2 — see Future Work).

## Decisions

### D1: Two-signal detection — title + Meet WebRTC connection

**Decision:** Detection requires **both** of the following, simultaneously:

1. A top-level window owned by a browser process (`chrome.exe` / `msedge.exe` / `firefox.exe`) with title matching `* - Google Meet` (suffix-anchored regex).
2. The same browser process has an active UDP or TCP socket to a remote IP within a known Google CIDR range — i.e., a live Meet WebRTC media connection.

When both are present and the connection appeared during the detector's observation window (D12), the state machine transitions toward `InCall` and the `meeting-detected` event fires.

**Rationale:** The previous design layered title + WASAPI mic capture + render peak meter, with focus-correlation and coordinated-activation filters bolted on to suppress false positives from Spotify-in-browser, dictation tools, and Discord PWAs. The Meet WebRTC connection signal directly answers the question those filters were approximating — *is this user actually connected to a Meet call?* — and does so with near-perfect specificity. Title + connection is both simpler and more correct:

- **Muted joins**: the WebRTC connection is established at click-Join regardless of mute state → ✓ detected.
- **Solo-speaker meetings**: the connection exists regardless of who is talking → ✓ detected.
- **Spotify in browser**: connection is to Spotify CDN, not Google → ✗ suppressed.
- **Dictation tool in browser**: no Meet WebRTC connection → ✗ suppressed.
- **Discord PWA call in same Chrome instance**: connection is to Discord servers, not Google → ✗ suppressed.

**Removed:** WASAPI mic capture session check, WASAPI render peak meter check, focus-correlation gate (formerly D13). All redundant with the connection signal for detection purposes. Focus history is retained but only for title resolution (D10).

### D2: Polling loop, not event subscription

**Decision:** A Tokio task polls the two signals every 5 seconds.

**Rationale:** Windows offers no efficient native subscription for "browser process started a UDP socket to a specific IP range." Window title changes have no native subscription either (would require `SetWinEventHook` with a message pump). Polling is dramatically simpler. A 5s cadence is well within "user joins → recording starts" tolerance, and the speculative-recording path (D5) captures from the moment of detection so the cadence doesn't translate into lost meeting content.

**Trade-off:** Up to ~5s detection lag in the worst case. Acceptable.

### D3: Hexagonal split — port + Windows adapter

**Decision:**

```
ports/meeting_detector.rs       trait MeetingDetectorPort {
                                    fn current_state(&self) -> DetectorObservation;
                                }

                                struct DetectorObservation {
                                    meet_windows: Vec<MeetWindow>,  // all matches
                                    has_meet_connection: bool,
                                    connection_first_seen_at: Option<Instant>,
                                }

detection/windows.rs            struct WindowsMeetingDetector {
                                    /* Win32 EnumWindows + iphlpapi sockets */
                                }
                                impl MeetingDetectorPort for WindowsMeetingDetector

use_cases/meeting_detection.rs  pub fn spawn_detector<P: MeetingDetectorPort>(
                                    port: P, app: AppHandle, settings: Settings,
                                ) -> JoinHandle  // polling loop + state machine
```

**Rationale:** Per CLAUDE.md §2. The use case (polling, state machine, event emission) is platform-agnostic Rust and fully unit-testable with a mock port. The Windows-specific FFI is contained in one adapter file.

**macOS later:** add `detection/macos.rs` implementing the same trait. The port returns the same `DetectorObservation` shape; only the means of obtaining it changes. The use case is unchanged.

### D4: State machine

**Decision:**

```
                   title ∧ Meet connection ∧
                   connection appeared during observation
   ┌─────┐ ────────────────────────────────────────▶ ┌────────┐
   │Idle │                                            │ InCall │
   └─────┘ ◀────────────connection gone───────────── └────────┘
            (10s debounce; absorbs transient network drops)

   Transitions emit Tauri events:
     Idle → InCall:  emit "meeting-detected" { default_title }
     InCall → Idle:  emit "meeting-ended"
```

**Rationale:** Two signals are enough. The intermediate `MaybeInCall` state from the prior design existed to sustain-filter the lobby — no longer needed because the lobby doesn't establish a WebRTC media connection (lobby uses HTTPS signaling only). The 10s debounce on the return to `Idle` absorbs transient network drops (a few seconds of packet loss, brief NAT issues) without triggering a false `meeting-ended`.

### D5: Speculative recording during countdown

**Decision:** When `meeting-detected` fires, the frontend immediately invokes `start_recording_with_devices_and_meeting` AND displays the countdown banner. If the user clicks "Cancel" within 10s, the frontend invokes `cancel_recording`. If the user clicks "Start now" or the timer expires, the recording continues normally and the banner dismisses.

**Rationale:** The user explicitly prefers orphan-audio-with-cleanup over missing 10-17s of meeting opening. Mid-call joins are common; losing the first 10s of conversation is unacceptable. Cleanup on cancel is one atomic operation; in steady state, recordings flow through the existing finalise path unchanged.

### D6: `cancel_recording` is a new command, not a flag on `stop_recording`

**Decision:** Add a new Tauri command `cancel_recording(meeting_id)` that:
1. Stops the audio capture (same internals as `stop_recording`).
2. Deletes the audio file from disk.
3. Deletes the meeting row from the database.
4. Does NOT emit `recording-stopped` notification or trigger transcription/summarisation.

**Rationale:** Conflating cancel into `stop_recording` via a parameter would force every existing caller to think about cleanup semantics. A separate command keeps the happy path of `stop_recording` unchanged. Atomic at the command level — all three steps succeed, or the function returns an error and we surface it.

**Failure mode:** If file deletion succeeds but DB deletion fails (or vice versa), we log loudly. D14's GC pass reconciles on next startup.

### D7: Settings: `auto_detect_meetings` toggle, on by default

**Decision:** Store `auto_detect_meetings: bool` (default `true`) in the existing settings store. On startup, `lib.rs` checks the flag; if true, spawns the detector. Toggling at runtime requires app restart for v1.

**Rationale:** On-by-default matches the user's stated preference. Settings UI is the standard place to disable. The runtime-toggle limitation is communicated by inline help text "Changes take effect after restart."

### D8: Detection signal sources (Win32 + iphlpapi)

**Decision:** Browser allowlist constant `BROWSER_PROCESSES: &[&str] = &["chrome.exe", "msedge.exe", "firefox.exe"]`.

- **Window enumeration:** `EnumWindows` (user32.dll), `GetWindowThreadProcessId` to filter to browser-allowlist windows, `GetWindowTextW` for the title, suffix-anchored regex `\s-\sGoogle Meet$`. Result: `Vec<MeetWindow { hwnd, pid, title }>`.
- **Network enumeration:** `GetExtendedUdpTable` and `GetExtendedTcpTable` (iphlpapi.dll) with `TCP_TABLE_OWNER_PID_ALL` / `UDP_TABLE_OWNER_PID`. Iterate rows; filter to those whose owning PID belongs to a browser-allowlist process; check the remote IP against the hardcoded `GOOGLE_MEDIA_CIDRS` list (see D17). If any row matches → `has_meet_connection = true`.

**Rationale:** These are standard Windows API primitives, both available through the `windows` crate. The codebase already has Win32 FFI precedent in `console_utils.rs`. iphlpapi is a new dependency feature but well-trodden.

**Dependency:** Add to `frontend/src-tauri/Cargo.toml` under the Windows target:
```toml
windows = { version = "0.58", features = [
    "Win32_UI_WindowsAndMessaging",
    "Win32_System_Threading",
    "Win32_NetworkManagement_IpHelper",
    "Win32_Networking_WinSock",
    "Win32_System_Com",
] }
```

### D9: Detection is tab-agnostic; title is best-effort

**Decision:** Detection only answers "is the user in a Meet call?" — never "which tab?" By the OS-level constraint that a user can only be in one Meet call at a time, the network connection signal is unambiguous for *whether* a call exists. The *title* question (what to label the recording) is separate and resolved via D10.

**Rationale:** Tab attribution from outside the browser is inherently hard. Detection doesn't need it; title resolution accepts an optimistic default and lets the user correct via the banner.

### D10: Layered title resolution

**Decision:** At the moment the state machine emits `meeting-detected`, the default title is resolved in this priority order:

```
1. Foreground snapshot at the transition moment
   GetForegroundWindow() → GetWindowTextW(). If title matches the
   Meet pattern, use it. Captures the "user just clicked Join" case.

2. Most-recently-focused Meet window in focus history (last 10 min)
   A continuous 1Hz poller maintains a VecDeque<(title, Instant)> capped
   at ~10 entries. Captures "user joined, then switched to notes" and
   "lobby admit while user is elsewhere" cases.

3. First Meet-titled window from EnumWindows
   Deterministic enumeration order. Captures the case where no Meet
   window has held focus recently.

4. Generic timestamp: "Meeting <YYYY-MM-DD HH:MM>"
   Last resort when no Meet-titled window exists at all.
```

The banner shows the resolved title in an editable text field. A dropdown lists all currently-enumerated Meet windows for one-click override. The committed title is written to the meeting row on banner dismiss (confirm/timeout).

**Cost of focus tracking:** a 1Hz `GetForegroundWindow()` + `GetWindowTextW()` call, plus a bounded in-memory deque. Negligible.

### D11: PWA handling is transparent

**Decision:** No PWA-specific code path. Chrome/Edge PWAs are top-level windows owned by the same browser process; they appear in `EnumWindows` with `document.title` as their window title. Filtering by *process name* (not window class) means PWAs pass through all detection and title-resolution layers unchanged.

**Verification:** Manual smoke test (tasks §3.7) explicitly opens a Meet PWA and confirms (a) the window appears in `EnumWindows`, (b) its title matches the Meet regex, (c) detection fires correctly, (d) the title appears in the dropdown.

**Edge case — PWA title differs:** If Chrome strips the `- Google Meet` suffix in PWA mode, the regex is broadened. Detection still works regardless (network signal is process-level), so the worst case is a worse default title.

### D12: Connection recency as the "join event"

**Decision:** The transition `Idle → InCall` requires that the Meet WebRTC connection appeared **during the detector's observation window** (i.e., it was not present in the previous poll). Track `connection_first_seen_at: Option<Instant>`; only fire `meeting-detected` if `connection_first_seen_at` is more recent than the detector's startup time AND the connection is currently present.

**Rationale:** Catches the moment of user-initiated join. Provides a clean trigger for the speculative-recording flow. Distinguishes "user just joined" from "user has been in this call since before Meetily started" (handled by D14).

**Implementation:** On each poll, compare the current `has_meet_connection` to the previous poll's value. Inactive→Active transition = the user just joined. The transition timestamp is the moment the connection was first observed.

### D13: (Removed) Focus correlation as detection gate

**Decision:** This decision is intentionally retained as a placeholder to make the numbering history readable. The prior design used a focus-correlation gate to suppress Discord-PWA-in-same-browser false positives. With D1's WebRTC connection signal providing Meet-specific discrimination directly, focus correlation is no longer required for detection.

Focus tracking is retained for title resolution only (D10).

### D14: Startup GC pass for orphan reconciliation

**Decision:** On app startup, before spawning the detector, run a synchronous GC pass:

1. **Orphan DB rows:** Query for meetings whose recorded audio file path is set but the file does not exist on disk → delete the row. Logged: `gc: deleted orphan meeting row {id} (audio path missing: {path})`.
2. **Orphan audio files:** List files in the recordings directory → for each, check if any meeting row references its absolute path. If none → delete the file. Logged: `gc: deleted orphan audio file {path}`.

The pass is idempotent and fast (<100ms in typical cases).

**Rationale:** `cancel_recording` performs atomic cleanup, but power loss, app crashes, or partial failures can leave inconsistent state. The GC pass converts "possibly inconsistent" into "definitely consistent" on every launch.

**Safety:** Only touches files in the recordings directory and rows in the meetings table; never deletes anything that any meeting row points to.

**Trade-off:** No undo (hard delete). If recovery is desired in the future, switch to `.trash/` quarantine. Not in v1 scope.

### D15: App-start state is conservative

**Decision:** When the detector starts, the initial poll establishes a baseline. The state machine starts in `Idle` regardless of whether a Meet connection already exists. The connection-recency gate (D12) ensures that pre-existing connections do NOT fire `meeting-detected` — the user must have joined the call *after* the detector started observing.

**Rationale:** If Meetily launches mid-call, we have no way to know how long the call has been running or whether the user wants this call recorded. Conservatively, the user can start manually for in-progress calls. Detecting only joins-during-observation gives a clean mental model: "Meetily watches for joins, not for ongoing calls."

**Future Work:** A liberal alternative — fire on first observation if focus history shows a recent Meet focus — could be added if users find this annoying. v1 ships conservative.

### D16: Cancel-suppression scope

**Decision:** The state machine tracks `cancelled_this_call: bool`. When the user clicks Cancel in the banner, this flag is set. While the flag is true AND the state remains `InCall`, no further `meeting-detected` events fire (the connection might briefly drop and reappear due to network blips, which would otherwise re-prompt). The flag resets when the state machine transitions back to `Idle` (call truly ended).

**Rationale:** A user who explicitly cancels a recording prompt does not want to be re-prompted for the same call. Resetting on `Idle` means "fresh call" gets a fresh prompt.

**Trade-off:** If a network blip causes a real `meeting-ended → meeting-detected` cycle within the same call (10s debounce should absorb this, but not always), the user gets prompted again. Acceptable: this is rare and the prompt is dismissible.

### D17: Concurrent-action precedence

**Decision:**
- **Manual "Start Recording" during the auto-start countdown:** the manual click silently cancels the auto-recording (deleting its file and DB row via `cancel_recording`) and begins a fresh manual recording. Manual always wins.
- **Signal re-engagement during the stop-prompt:** if `Idle → InCall` happens while the stop-prompt is displayed (the call reconnected after a transient drop), the prompt is dismissed silently and the recording continues. No need to bother the user with "false alarm."
- **Auto-detect fires while a manual recording is in progress:** the event is ignored. The manual recording continues unmodified. No new banner is shown.

**Rationale:** User-initiated actions always take precedence over automation. The state machine should never surprise the user by overriding a deliberate click.

### D18: Google media CIDR list and refresh

**Decision:** A hardcoded `GOOGLE_MEDIA_CIDRS: &[Ipv4Net]` constant in `detection/windows.rs` lists Google-owned IPv4 CIDR ranges known to host Meet's media servers. Sourced from Google's published ASN data and confirmed empirically. Approximately 10-20 `/16` and `/19` blocks. IPv6 ranges follow the same pattern.

**Examples (illustrative — actual list determined during implementation):**
```rust
const GOOGLE_MEDIA_CIDRS: &[&str] = &[
    "142.250.0.0/15",
    "172.217.0.0/16",
    "216.58.192.0/19",
    // ...
];
```

**Rationale:** Google's published ranges drift gradually (a few additions per year). Hardcoding is appropriate for v1; an automated refresh from `_spf.google.com` TXT records or Google's `cloud.json` is overkill until we observe drift causing misses.

**Refresh policy:** review and update the list annually, or sooner if false-negatives appear. Tracked as a maintenance task in repository docs.

**False-negative risk:** if Google moves Meet media to new IPs not in our list, detection silently fails. Mitigated by: an ops alert can be added later, or the user notices and reports. v1 accepts this risk.

### D19: Capture peak as a contingency fallback (not in v1)

**Decision:** WASAPI capture peak metering is **not** implemented in v1. It is documented as the designated fallback to pivot to if smoke testing reveals that the WebRTC connection signal is unreliable or has unexpected false-positive/false-negative behaviour.

**Trigger to pivot:** if during the manual smoke test (or early production use) any of the following are observed:
- Detection fires for non-Meet activity that involves Google IPs (rare but possible — Google Hangouts, Stadia).
- Detection misses real Meet calls due to IP-range gaps in D18's list.
- Connection-recency check has timing flakiness that causes misses.

**Pivot plan:** add `IAudioMeterInformation` on the WASAPI capture session for browser PIDs as a secondary required signal. Adds ~50 LOC, requires the multi-device enumeration approach (see "Implementation Notes" below) to handle non-default microphones.

**Implementation note for the pivot:** if peaks are added, the implementation MUST enumerate all `DEVICE_STATE_ACTIVE` audio devices (via `IMMDeviceEnumerator::EnumAudioEndpoints`), not just the default. Users with USB headsets routinely select a non-default mic in Meet's settings; checking only the default device would silently miss them.

**Rationale:** Belt-and-suspenders complexity is not free, and the WebRTC signal is highly specific by construction. We optimise for simpler, smaller code now; the pivot path is documented so it's a small change if needed.

## Risks / Trade-offs

**[Risk] Google IP CIDR drift causes false negatives** → D18 hardcoded list. Refreshed annually or sooner if reports surface. Pivot to D19 capture-peak signal if drift becomes a regular issue.

**[Risk] Detection misses Meet calls that route through user-side TURN servers in non-Google IP ranges** → Some enterprise networks force WebRTC through corporate TURN relays. Connection signal would fail. Same mitigation as above; for v1 the manual record button is the fallback.

**[Risk] User joins a call during the silent pre-meeting period and no audio plays** → Detection fires on connection regardless of audio. Recording starts immediately and captures silence until audio begins. VAD compresses silent segments downstream. ✓ Correct behaviour with no peak signal dependency.

**[Risk] User has Hangouts or another Google WebRTC service active in same browser instance with a Meet tab open** → Title check requires `- Google Meet` suffix; Hangouts windows don't match. Connection check matches Google IP range (correctly so). So title narrows correctly. False-positive risk only if user has both a Meet tab AND non-Meet Google WebRTC active; the dropdown lets them clarify the title or cancel.

**[Risk] User has multiple Meet tabs / multiple browser windows** → Detection fires (correctly: there is an active Meet call). Title resolution picks a best-effort default via D10; the banner dropdown shows all candidates for one-click correction.

**[Risk] Browser variants beyond Chrome/Edge/Firefox** → Add to `BROWSER_PROCESSES` allowlist on user demand. Brave (`brave.exe`), Arc, Vivaldi, Opera all use standard Windows sockets so the network detection works identically. Not v1 scope.

**[Risk] User has USB headset selected as Meet's mic, default Windows mic is something else** → No longer relevant in v1: detection no longer depends on WASAPI device enumeration. If D19 pivot happens, the implementation must enumerate all active devices (called out explicitly in D19).

**[Risk] `cancel_recording` partial failure leaves orphan file or row** → Logged loudly. D14's GC pass reconciles on next launch. Worst case: until next app restart, an orphan exists — bounded and self-healing.

**[Risk] Detector adds CPU overhead from polling** → 5s polling of two cheap Windows APIs (`EnumWindows` + `GetExtendedUdpTable`/`GetExtendedTcpTable`). Sub-millisecond per poll. Trivial.

**[Risk] Malicious browser extension fakes a Meet window title** → Title match alone wouldn't trigger; the connection signal also requires an actual WebRTC connection to Google IPs. An extension would need to also drive real network traffic to Google to spoof. The user is already trusting their extensions; this is a low-impact concern.

**[Risk] Auto-detect surprises a user who didn't realise it was on** → On-by-default with a clear settings toggle and a 10s cancel countdown. The first time it fires, the banner explains itself. Acceptable.

## Migration Plan

No migration needed. Feature is additive. Users on existing installs get the toggle on first launch with default-on; they can disable in settings.

## Open Questions

- **What if a recording is already active when `meeting-detected` fires?** — D17 covers this: the event is ignored; no second recording starts.
- **Should the meeting title remain editable post-recording?** — Verify this exists in the current meeting detail UI; if so, document; if not, add as a small follow-up. Not blocking.

## Future Work (to become GitHub issues at archive time)

- **Runtime toggle of `auto_detect_meetings`** — currently restart-required (D7). v2 would dynamically start/stop the detector task when the setting changes.
- **macOS adapter** — implement `MeetingDetectorPort` for macOS. The port trait stays unchanged; the adapter is new.
- **Additional browser allowlist** — Brave, Arc, Vivaldi, Opera on demand.
- **D19 pivot to peak-based fallback** — if smoke testing or production use reveals connection-detection issues.
- **Google CIDR auto-refresh** — fetch from Google's published ASN data periodically rather than hardcoding.
- **Detection for non-Meet platforms** — Zoom, Teams, Slack Huddles. The port abstraction supports this without restructuring.
