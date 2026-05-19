## Context

The Windows meeting detector polls every 2 s. Exit detection has two branches inside `current_state()` in `detection/windows.rs`:

**Branch 1 — TCP TURN established:** If a TCP TURN relay connection (`is_in_turn_cidrs()`) was ever seen for this call, exit is detected when TURN is gone. This works correctly: TURN drops within ~1 s of the user leaving the call.

**Branch 2 — TURN never established (the broken path):** Most Google Meet sessions use UDP for media transport on typical networks. UDP connections carry no remote address in `GetExtendedUdpTable`, so TURN is invisible; `turn_established` stays `false` throughout the call. The fallback is `has_meet_connection()` — a broad TCP scan for Google CIDR IPs. After leaving a UDP call, Chrome/Edge stays on `meet.google.com/<code>` (the "You've left the meeting" lobby), with the tab title unchanged ("Meet - xxx" persists until the user explicitly navigates away) and HTTPS TCP connections to Google still open. `has_meet_connection()` returns `true` indefinitely. The exit debounce never starts. `meeting-ended` never fires.

D19 in the archived `meeting-auto-detect` design designated WASAPI capture peak metering as the contingency fallback for exactly this scenario. This change promotes D19 to the primary fix.

## Goals / Non-Goals

**Goals:**
- `meeting-ended` fires reliably for UDP-transport Meet calls within ~17 s of the user leaving (WASAPI session drops in ~1–2 s + 15 s debounce). 15 s debounce required — see D11.
- TCP TURN call exit latency is preserved at ~5 s: `is_turn_exit` enables a fast 4 s debounce that fires as soon as TURN drops, before WASAPI drops — see D11.
- COM acquisition is safe in a tokio async context.
- `brave.exe` added to the browser allowlist (omitted from original implementation).

**Non-Goals:**
- Meet calls joined without mic permission (WASAPI session never opens; same degraded path as today, no regression).
- macOS detection (separate adapter, unaffected).
- Configurable poll interval or debounce window (deferred to `transcription-scheduler-advanced`).
- UDP table remote-IP detection (OS-level limitation; not feasible without a kernel driver or raw sockets).

## Decisions

### D1: WASAPI capture session as the exit signal

Signal landscape for the UDP call exit problem:

| Signal | During call | Lobby after leaving | Viable? |
|---|---|---|---|
| TCP TURN | Sometimes (TCP calls only) | Gone | ✓ (TCP path — already used) |
| Broad Google TCP (`has_meet_connection`) | true | true (HTTPS stays) | ✗ |
| Window title | "Meet - xxx" | "Meet - xxx" (unchanged) | ✗ confirmed by user |
| `GetExtendedUdpTable` remote IP | N/A — field absent in API | — | ✗ OS limitation |
| WASAPI audio capture session | Active | Released on leave | ✓ |

Chrome and Edge hold the `getUserMedia` mic capture session open throughout a Meet call — even when the user is muted, even when no other participant is speaking. The capture session is released within ~1–2 s of the user clicking "Leave call." This signal is not available via TCP scanning but is directly readable from the Windows Audio Session API (WASAPI) through `IAudioSessionManager2`.

### D2: Asymmetric detection — conjunction for entry, WASAPI alone for exit

**Entry signal (Idle → InCall): `has_meet_connection() && has_browser_capture_session()`.**

WASAPI alone would false-positive if a browser tab is using the mic for a non-Meet reason — browser-based dictation, a Zoom call in the same Chrome instance, a PWA with mic access. The conjunction requires both a Google CIDR TCP connection and an active capture session, which is specific enough to match the spec's scenario coverage (Discord PWA, Spotify-in-browser + dictation, etc.).

`DetectorObservation::has_meet_connection` carries the conjunction (`mc && bc`). Used only in the Idle → InCall transition.

**Exit signal (InCall → Idle): `has_browser_capture_session` alone (bc).**

Using the conjunction for exit introduces the 90s+ false-exit bug: Chrome/Edge keeps HTTPS connections to Google alive on the lobby page, so `mc` stays true. If a 90s TCP drop occurs during an active UDP call, `mc` becomes false — making the conjunction false — and the debounce starts even though the call is active. Using `bc` alone for exit eliminates this: WASAPI Active state is the definitive "call is live" signal, regardless of TCP state.

`DetectorObservation::has_browser_capture_session` carries `bc` directly (not the conjunction). Used only in the InCall → Idle transition.

This asymmetry is intentional. See D11 for the debounce rationale and trade-offs.

Note: the TURN path benefits from `is_turn_exit`. When `turn_established = true` and TURN drops, the adapter sets `is_turn_exit = true`. The InCall branch sees this and starts the fast 4 s debounce immediately — before WASAPI drops — restoring the original ~5 s TURN exit latency. See D11.

### D3: COM initialization — per-call `CoInitializeEx`/`CoUninitialize`

WASAPI is a COM API. `CoInitializeEx` must be called on each thread before any COM interface is used, and `CoUninitialize` must be called to balance each successful init.

The detection loop is `tokio::spawn` (a regular async task on the multi-threaded executor). After each `tokio::time::sleep().await` the future may resume on a different worker thread. A thread-local init guard would only initialise COM on the first thread the future lands on — every subsequent thread migration would call WASAPI COM interfaces uninitialised, producing `CO_E_NOTINITIALIZED` and causing `has_browser_capture_session()` to return `false` on those polls.

The correct pattern is per-call initialisation:

```
hr = CoInitializeEx(None, COINIT_APARTMENTTHREADED)
we_inited = (hr >= 0)             // S_OK (0) or S_FALSE (1): both increment COM ref count, both require CoUninitialize
if hr < 0 && hr != RPC_E_CHANGED_MODE { return false }   // genuine COM failure
// ... WASAPI work ...
if we_inited { CoUninitialize() }
```

`CoInitializeEx` returns `S_FALSE` (`0x1`) if COM is already initialised on this thread in the same apartment — **this is not an error, and `CoUninitialize` MUST be called** because `S_FALSE` also increments the COM reference count per MSDN. Both `S_OK` and `S_FALSE` are non-negative, so `we_inited = hr >= 0` covers both correctly. A real failure returns a negative `HRESULT`.

### D4: Graceful degradation on COM failure

If `CoInitializeEx` returns a negative `HRESULT` that is not `RPC_E_CHANGED_MODE`, `has_browser_capture_session()` returns `false`. This is the conservative default. For entry: `has_meet_connection = mc && false = false`, so the Idle state never transitions to InCall. For exit: InCall sees `bc = false`, starts the 15 s debounce, and eventually fires `meeting-ended`. At worst this fires `meeting-ended` slightly early if COM is systematically unavailable. That is preferable to the current state of never firing.

`RPC_E_CHANGED_MODE` (`0x80010106`) is a special case: it means COM is already initialised on this thread but in a different concurrency model (MTA). This arises when the tokio thread pool reuses a thread that the audio capture layer (cpal/WASAPI) previously initialised as MTA. Unlike a genuine COM failure, the thread is still in a valid COM state and `IMMDeviceEnumerator` is apartment-agnostic — it works correctly from both STA and MTA. On `RPC_E_CHANGED_MODE` the function proceeds with the existing COM initialisation (does not call `CoUninitialize` since we did not initialise) and enumerates sessions normally.

### D7: `turn_established` reset via `notify_exit()` port method

Pre-review finding: `turn_established` is a sticky flag reset only when `meet_windows.is_empty()`. On the lobby page, `meet_windows` is never empty (title unchanged). Back-to-back calls in the same tab carry the flag forward from the previous call, preventing new call detection.

**Conditional WASAPI reset (rejected after third-round review):** An earlier draft reset `turn_established` only when `has_browser_capture_session()` was also `false`. This still fails when a persistent browser-mic service (Otter.ai web, Fireflies, browser dictation extension) keeps WASAPI active continuously across calls — the flag is never cleared by the polling path alone, and the next UDP call in the same tab is permanently undetectable until the tab is closed.

**Accepted fix: `notify_exit()` on the port trait (hexagonal, per CLAUDE.md §2).** The use case already knows when `meeting-ended` fires (it drives the `DetectorEvent::MeetingEnded` transition). The cleanest wiring is a callback from the use case layer down to the adapter:

1. `MeetingDetectorPort` gains `fn notify_exit(&mut self) {}` with a default no-op so all existing implementations compile unchanged.
2. `WindowsMeetingDetector::notify_exit()` sets `self.turn_established = false`.
3. `spawn_detector` calls `port.notify_exit()` **before** emitting the `MeetingEnded` event. This ordering ensures adapter state is always reset even if the Tauri emitter panics — a panicking emitter would otherwise leave `turn_established = true` indefinitely, blocking detection of all subsequent calls.

With this in place, `else if self.turn_established` needs no WASAPI check — it holds `has_conn = false` unconditionally until `notify_exit()` definitively clears the flag on actual exit.

State-machine walk-through:
- **TCP exit, no parallel mic:** TURN drops → debounce → `notify_exit()` → `turn_established = false` → `meeting-ended` emitted. Next call (UDP): WASAPI check active → detected. ✓
- **TCP exit, persistent Otter.ai/Fireflies browser mic:** TURN drops → WASAPI stays true → `else if turn_established { false }` holds for full debounce → `notify_exit()` → `turn_established = false` → `meeting-ended` emitted. Next call: detectable. ✓
- **Transient TURN blip, still in call:** TURN drops 1 poll → `bc` stays true → InCall clears debounce timer → `notify_exit()` is NOT called (no `MeetingEnded` event) → `turn_established` stays set → TURN returns → normal. ✓
- **Back-to-back TCP-then-UDP:** TCP call ends → `notify_exit()` → `turn_established = false` → UDP call: WASAPI check active → detected. ✓

### D11: 15 s debounce and TURN regression

**Debounce increase from 4 s to 15 s.**

With the `AudioSessionStateActive`-only filter, the exit signal (`bc`) can briefly go false during an active call if Chrome transiently releases and re-acquires the mic capture session — e.g., mic re-negotiation after a network route change, or Chrome's internal audio device switch. Empirical logs from smoke testing show ~10 s Inactive transients during live UDP calls (Chrome mic re-acquisition). A 4 s debounce absorbs nothing; 15 s absorbs the observed maximum with 5 s margin.

**TURN regression.**

Before this change, the InCall branch used `has_meet_connection` (the conjunction) as the exit signal with a 4 s debounce. When TURN dropped, the adapter immediately returned `has_meet_connection = false` (since `mc = false` in the TURN-gone branch). Debounce started, fired in 4 s: total ~5 s exit latency.

After this change, the InCall branch uses `bc` (`has_browser_capture_session`) as the exit signal. Naively, when TURN drops, `bc` stays true for ~1–2 s (WASAPI session is still streaming). Then `bc` drops → debounce starts → 15 s: total ~17 s exit latency for TCP TURN calls.

**TURN regression resolved by `is_turn_exit`.**

`DetectorObservation::is_turn_exit` is set `true` in the adapter when `turn_established = true && !turn` (TURN was seen for this call and has just dropped). The InCall branch treats `is_turn_exit = true` as an early exit trigger: the fast 4 s debounce starts immediately upon TURN drop, independently of `bc`. If TURN returns transiently (blip), `is_turn_exit` flips back to `false` and the timer clears. When `bc` eventually drops (1–2 s after TURN), `is_turn_exit` is still `true`, so the 4 s debounce continues. Total TURN exit latency: TURN drops → 4 s debounce → `meeting-ended` ≈ 5 s, matching pre-change behaviour.

`DetectorSettings::turn_debounce_duration` (default 4 s) controls the TURN path. `debounce_duration` (default 15 s) controls the UDP path. Both are injectable via `spawn_detector` for testability.

### D8: Enumerate all active capture endpoints, not just the default

`GetDefaultAudioEndpoint(eCapture, eConsole)` returns the single device assigned the `eConsole` role. Chrome's WebRTC input stack calls `GetDefaultAudioEndpoint(eCapture, eCommunications)` — if the user has set a separate "Communications" device in Windows Sound settings (a common corporate configuration: headset as communications, webcam or built-in as system default), Chrome's capture session is on the `eCommunications` device while the query returns the `eConsole` device. Additionally, if the user has explicitly selected a non-default mic in Chrome Settings or Meet's pre-join settings, Chrome opens the session on that specific device, which is invisible to a single `GetDefaultAudioEndpoint` call.

**Fix:** Enumerate all active capture endpoints via `IMMDeviceEnumerator::EnumAudioEndpoints(eCapture, DEVICE_STATE_ACTIVE)` and iterate sessions on each device. Typically 1–3 devices; enumeration is sub-millisecond per device. Return `true` on the first browser-process capture session found across any device.

### D10: Filter to `AudioSessionStateActive` only

Smoke-test finding: browsers hold a persistent `AudioSessionStateInactive` WASAPI capture session for any tab that has previously been granted mic permission, even when that tab is not currently streaming audio. On a typical machine with Edge open and a prior mic-permission grant, `has_browser_capture_session()` returned `true` at all times — before, during, and after the Meet call — because the Inactive background session satisfied `state != AudioSessionStateExpired`. The conjunction with `has_meet_connection()` (which also stays true on the lobby page) produced `has_conn = true` continuously, preventing the exit debounce from ever starting.

WASAPI `AudioSessionStateActive` (= 1) is set by Windows when the audio endpoint is actively streaming. Chrome/Edge opens `IAudioClient::Start()` when `getUserMedia` delivers media, keeping the session Active throughout the call. On "Leave call," the session transitions to Inactive (not Expired as originally assumed — empirically observed with Edge PID 32700 during smoke testing). Chrome/Edge behaviour: releases `IAudioClient` → session goes Inactive and eventually Expired after the OS audio session timeout (~several seconds to minutes depending on Windows version). Idle background sessions with mic permission that are not streaming remain Inactive indefinitely.

The correct filter is `state == AudioSessionStateActive`. All non-Expired sessions are still logged at DEBUG for diagnostics, but only Active browser sessions satisfy `has_browser_capture_session()`. This matches the design intent stated in D2: "A browser process has an **active** WASAPI audio capture session."

### D9: Remove `IsSystemSoundsSession` check

Pre-review finding: `IAudioSessionControl2::IsSystemSoundsSession` returns `S_OK` for system sounds and `S_FALSE` for non-system-sounds sessions. `S_FALSE` (`0x1`) satisfies `is_ok()` (which tests `>= 0`), so any `if hr.is_ok() { continue }` guard skips every session without exception, making `has_browser_capture_session()` always return `false`.

Correct fix: remove the check entirely. `is_browser_process(pid)` already restricts matches to `chrome.exe`, `msedge.exe`, `firefox.exe`, and `brave.exe` — the system sounds session runs under the audio engine process, not a browser, so it is unreachable by the PID filter.

### D5: Add `brave.exe` to browser allowlist

The existing `BROWSER_PROCESSES` slice contains `["chrome.exe", "msedge.exe", "firefox.exe"]`. Brave is Chromium-based, common among privacy-focused users, and uses WASAPI the same way Chrome does. Adding `"brave.exe"` keeps the allowlist consistent with real-world browser diversity.

### D6: New Windows feature flag `Win32_Media_Audio`

The interfaces needed (`IMMDeviceEnumerator`, `MMDeviceEnumerator`, `IAudioSessionManager2`, `IAudioSessionControl`, `IAudioSessionControl2`, `IAudioSessionEnumerator`, `AudioSessionState`) all live in `Win32_Media_Audio`. `Win32_System_Com` is already present (required for `CoCreateInstance` and `CLSCTX_ALL`). No new crate dependency.

## Risks / Trade-offs

- **[Risk]** Mic session established after first `has_meet_connection()` = true. During the join handshake (1-3 s before TURN is established), `has_meet_connection()` may be true before Chrome has fully opened the mic capture session (getUserMedia completes asynchronously). On that one poll, `has_conn = false`. On the next poll (2 s later), both are true and detection fires normally. The join-detection latency is at most one 2 s poll interval longer than today. Acceptable — the current TURN wait is already 1-3 s.

- **[Risk]** User joins Meet without granting mic permission. `has_browser_capture_session()` returns `false`; `has_conn = false`. Meetily never detects the call. This is identical to current behaviour: with no mic, Meetily has no useful audio to capture anyway. No regression.

- **[Risk]** Corporate WASAPI policy disables `IMMDeviceEnumerator`. Treated as COM failure (D4 — returns `false`). Exit detection would stop working on that machine. Mitigation: the TURN path is unaffected; only UDP-only calls on restricted WASAPI environments are impacted. This is a narrow edge case — if WASAPI is locked down, microphone recording is also disabled, so Meetily is already non-functional in that environment.

- **[Trade-off]** `CoInitializeEx` with `COINIT_APARTMENTTHREADED` may be called on a thread already initialised as MTA by the audio capture layer (cpal/WASAPI). This returns `RPC_E_CHANGED_MODE`. As of D4 this is handled gracefully — the function proceeds using the existing MTA context rather than bailing. No `CoUninitialize` is called. `IMMDeviceEnumerator` works from MTA, so enumeration proceeds normally. Other negative HRESULTs (true COM failure) still return `false`.

- **[Risk]** Chrome's "release mic when muted" optimisation: with the `AudioSessionStateActive`-only filter (D10), if Chrome transitions the session from Active to Inactive when the user mutes (via `track.enabled = false`), `has_browser_capture_session()` would return `false`, triggering a false `meeting-ended` after 4 s. The standard `track.enabled = false` mute path keeps the `IAudioClient` streaming — Chrome continues reading from the capture endpoint and discarding samples — so the session stays Active. Only `track.stop()` (full getUserMedia release) causes the session to go Active→Inactive. Google Meet uses `track.enabled` for muting. The 15 s debounce also provides a practical buffer against transient state flips (observed ~10 s mic re-acquisition transients). Verified via smoke test 7.4: Edge capture session shows state Active in WASAPI debug log while muted.

- **[Trade-off]** `has_browser_capture_session()` iterates audio sessions every 2 s. The session enumerator is a lightweight in-process COM object; enumeration is sub-millisecond. No measurable impact on the 2 s poll budget.

- **[Known design gap — C2]** The entry conjunction (`has_meet_connection = mc && bc`, used for the Idle → InCall transition) is also satisfied by the Meet lobby page (HTTPS TCP connections to Google + `getUserMedia` camera/mic preview from the pre-join screen). If Meetily launches while the user has the Meet lobby open, `connection_first_seen_at` is set to `detector_start` on the first poll (D15). The user then clicks "Join" — the connection never drops, so `connection_first_seen_at` is never reset, and `not_preexisting` remains `false` permanently for that session. `meeting-detected` never fires; the user must start recording manually. A subsequent change should add UDP connection detection (`GetExtendedUdpTable` — the WebRTC media signal, absent from the lobby) as a more discriminating entry signal and restrict D15 first-poll pinning to connections that satisfy the UDP media check.

- **[Implementation note — D15 first-poll window gate]** The `connection_first_seen_at` update is gated on `!meet_windows.is_empty()` on subsequent polls (to prevent background Google TCP from poisoning D15), but the **first-poll branch must not apply this gate**. If the Meet window is minimized at detector start, the window list is empty on poll 1 and the C_FSA stamp is skipped. On poll 2 the window appears and C_FSA is set to `Instant::now()` — which is later than `detector_start` — so `not_preexisting` becomes `true` and detection fires for a pre-existing call (D15 bypass). Fix: first-poll branch stamps C_FSA unconditionally on any `has_conn`; window gate applies only on `else if` (subsequent polls).

- **[Implementation note — IPv4-mapped IPv6]** On dual-stack Windows hosts, the kernel may report established TCP connections in the IPv6 table using IPv4-mapped notation (`::ffff:x.x.x.x`). Both `check_tcp6_connections()` and `check_turn_tcp6_connections()` call `Ipv6Addr::to_ipv4_mapped()` before CIDR matching so that IPv4 Google ranges are correctly matched on dual-stack configurations.

## Open Questions

*(All open questions resolved — smoke tests 7.2, 7.3, 7.4 passed.)*
